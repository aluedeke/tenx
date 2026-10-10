//! `tenx-whisper`, the speech-to-text program `tenx web` runs for the
//! page's microphone: started on the first recording, given one recording at
//! a time over its stdin/stdout (`tenx_core::whisper`), started again if it
//! died, and ended — closing its stdin — once unused for `[speech]
//! idle_minutes`, which gives the model's gigabytes back. A changed
//! `[speech]` takes effect on the next recording.
//!
//! The program sits next to the `tenx` binary (a release ships both), else
//! on `PATH`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tenx_core::whisper::{self, Reply, Request};

use crate::workspace::{SpeechConfig, expand_home};

struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// What it was started with: a different `[speech]` restarts it.
    config: SpeechConfig,
    last_used: Instant,
}

impl Running {
    fn stop(mut self) {
        drop(self.stdin); // EOF: it exits by itself
        if self.child.wait_timeout(Duration::from_secs(5)).is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn reply(&mut self) -> Result<Reply> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            bail!("tenx-whisper exited");
        }
        serde_json::from_str(line.trim()).with_context(|| format!("tenx-whisper said {:?}", line.trim()))
    }
}

/// Wait up to `d` for a child to exit.
trait WaitTimeout {
    fn wait_timeout(&mut self, d: Duration) -> Option<std::process::ExitStatus>;
}

impl WaitTimeout for Child {
    fn wait_timeout(&mut self, d: Duration) -> Option<std::process::ExitStatus> {
        let until = Instant::now() + d;
        loop {
            if let Ok(Some(status)) = self.try_wait() {
                return Some(status);
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The one program; also what keeps recordings one at a time.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static REAPER: Once = Once::new();

fn idle(config: &SpeechConfig) -> Duration {
    Duration::from_secs(60 * u64::from(if config.idle_minutes == 0 { 10 } else { config.idle_minutes }))
}

/// `tenx-whisper` beside this binary, else whatever `PATH` has.
fn program() -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("tenx-whisper")));
    beside.filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("tenx-whisper"))
}

fn start(config: &SpeechConfig) -> Result<Running> {
    let model = expand_home(&config.model);
    if !std::path::Path::new(&model).is_file() {
        bail!("no speech model at {model} ([speech] model in ~/.config/tenx/config.toml)");
    }
    let program = program();
    let mut cmd = Command::new(&program);
    cmd.args(["--model", &model]);
    if !config.prompt_file.is_empty() {
        cmd.args(["--prompt-file", &expand_home(&config.prompt_file)]);
    }
    if !config.language.is_empty() {
        cmd.args(["--language", &config.language]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("start {} (it ships next to tenx)", program.display()))?;
    let mut running = Running {
        stdin: child.stdin.take().context("tenx-whisper's stdin")?,
        stdout: BufReader::new(child.stdout.take().context("tenx-whisper's stdout")?),
        child,
        config: config.clone(),
        last_used: Instant::now(),
    };
    match running.reply() {
        Ok(Reply::Ready { .. }) => {}
        Ok(Reply::Error { error }) => {
            running.stop();
            bail!("tenx-whisper: {error}");
        }
        other => {
            running.stop();
            bail!("tenx-whisper didn't start: {other:?}");
        }
    }
    REAPER.call_once(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(30));
                let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
                if slot.as_ref().is_some_and(|r| r.last_used.elapsed() >= idle(&r.config))
                    && let Some(r) = slot.take()
                {
                    r.stop();
                }
            }
        });
    });
    Ok(running)
}

/// The text of one recording (16 kHz mono samples); `language` overrides
/// `[speech] language`.
pub(super) fn transcribe(config: &SpeechConfig, audio: &[f32], language: Option<&str>) -> Result<String> {
    let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|r| r.config != *config)
        && let Some(r) = slot.take()
    {
        r.stop();
    }
    // Once more from a fresh start if it had died since the last recording.
    for attempt in 0..2 {
        if slot.is_none() {
            *slot = Some(start(config)?);
        }
        let running = slot.as_mut().expect("just started");
        match send(running, audio, language) {
            Ok(Reply::Text { text }) => {
                running.last_used = Instant::now();
                return Ok(text);
            }
            Ok(Reply::Error { error }) => {
                running.last_used = Instant::now();
                bail!("{error}");
            }
            Ok(other) => bail!("tenx-whisper answered {other:?}"),
            Err(e) => {
                if let Some(r) = slot.take() {
                    r.stop();
                }
                if attempt == 1 {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}

fn send(running: &mut Running, audio: &[f32], language: Option<&str>) -> Result<Reply> {
    let req = Request { samples: audio.len(), language: language.map(str::to_string) };
    let mut msg = serde_json::to_vec(&req)?;
    msg.push(b'\n');
    msg.extend(whisper::samples_bytes(audio));
    running.stdin.write_all(&msg)?;
    running.stdin.flush()?;
    running.reply()
}
