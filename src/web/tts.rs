//! `tenx-tts`, the text-to-speech program `tenx web` runs for the page's
//! speaker: started on the first reading, given one piece of text at a time
//! over its stdin/stdout (`tenx_core::tts`), started again if it died, and
//! ended — closing its stdin — once unused for `[read_aloud] idle_minutes`,
//! which gives the model's memory back. A changed `[read_aloud]` takes
//! effect on the next piece. Like `tenx-whisper`, it sits next to the `tenx`
//! binary, else on `PATH`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tenx_core::tts::{self, Reply, Request};

use crate::workspace::{ReadAloudConfig, expand_home};

struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    sample_rate: u32,
    /// What it was started with: a different `[read_aloud]` restarts it.
    config: ReadAloudConfig,
    last_used: Instant,
}

impl Running {
    fn stop(mut self) {
        drop(self.stdin); // EOF: it exits by itself
        let until = Instant::now() + Duration::from_secs(5);
        while !matches!(self.child.try_wait(), Ok(Some(_))) {
            if Instant::now() >= until {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn reply(&mut self) -> Result<Reply> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            bail!("tenx-tts exited");
        }
        serde_json::from_str(line.trim()).with_context(|| format!("tenx-tts said {:?}", line.trim()))
    }

    /// One piece's audio, as samples.
    fn speak(&mut self, req: &Request) -> Result<Result<Vec<f32>, String>> {
        let mut msg = serde_json::to_vec(req)?;
        msg.push(b'\n');
        self.stdin.write_all(&msg)?;
        self.stdin.flush()?;
        match self.reply()? {
            Reply::Audio { samples } => {
                let mut bytes = vec![0u8; samples * 4];
                self.stdout.read_exact(&mut bytes).context("tenx-tts's samples")?;
                Ok(Ok(tenx_core::whisper::bytes_samples(&bytes)))
            }
            Reply::Error { error } => Ok(Err(error)),
            other => bail!("tenx-tts answered {other:?}"),
        }
    }
}

/// The one program; also what keeps pieces one at a time.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static REAPER: Once = Once::new();

fn idle(config: &ReadAloudConfig) -> Duration {
    Duration::from_secs(60 * u64::from(if config.idle_minutes == 0 { 10 } else { config.idle_minutes }))
}

/// `tenx-tts` beside this binary, else whatever `PATH` has.
fn program() -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("tenx-tts")));
    beside.filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("tenx-tts"))
}

/// `[read_aloud]` from the global config, checked enough to say what's missing.
pub(super) fn config() -> Result<ReadAloudConfig> {
    let config = crate::workspace::load_global()?.read_aloud;
    if config.model.is_empty() {
        bail!("no voice model: download kokoro-multi-lang-v1_0 (sherpa-onnx's tts-models release) and set [read_aloud] model in ~/.config/tenx/config.toml");
    }
    if !config.voice.is_empty() && tts::voice_id(&config.voice).is_none() {
        bail!("no Kokoro voice {:?} ([read_aloud] voice; e.g. af_heart, bm_george)", config.voice);
    }
    Ok(config)
}

fn start(config: &ReadAloudConfig) -> Result<Running> {
    let model = expand_home(&config.model);
    if !std::path::Path::new(&model).is_dir() {
        bail!("no voice model at {model} ([read_aloud] model in ~/.config/tenx/config.toml)");
    }
    let program = program();
    let mut cmd = Command::new(&program);
    cmd.args(["--model", &model]);
    if !config.voice.is_empty() {
        cmd.args(["--voice", &config.voice]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("start {} (it ships next to tenx)", program.display()))?;
    let mut running = Running {
        stdin: child.stdin.take().context("tenx-tts's stdin")?,
        stdout: BufReader::new(child.stdout.take().context("tenx-tts's stdout")?),
        child,
        sample_rate: 0,
        config: config.clone(),
        last_used: Instant::now(),
    };
    match running.reply() {
        Ok(Reply::Ready { sample_rate, .. }) => running.sample_rate = sample_rate,
        Ok(Reply::Error { error }) => {
            running.stop();
            bail!("tenx-tts: {error}");
        }
        other => {
            running.stop();
            bail!("tenx-tts didn't start: {other:?}");
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

/// Start the program now if it isn't running, so its model is loading while
/// the page asks for the first piece. Errors wait for that request.
pub(super) fn warm(config: &ReadAloudConfig) {
    let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|r| r.config != *config)
        && let Some(r) = slot.take()
    {
        r.stop();
    }
    if slot.is_none()
        && let Ok(r) = start(config)
    {
        *slot = Some(r);
    }
}

/// `text` read aloud, as a WAV.
pub(super) fn speak(config: &ReadAloudConfig, text: &str) -> Result<Vec<u8>> {
    let speed = if config.speed > 0.0 { config.speed } else { 1.0 };
    let req = Request { text: text.to_string(), speed };
    let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|r| r.config != *config)
        && let Some(r) = slot.take()
    {
        r.stop();
    }
    // Once more from a fresh start if it had died since the last piece.
    for attempt in 0..2 {
        if slot.is_none() {
            *slot = Some(start(config)?);
        }
        let running = slot.as_mut().expect("just started");
        match running.speak(&req) {
            Ok(done) => {
                running.last_used = Instant::now();
                let samples = done.map_err(anyhow::Error::msg)?;
                return Ok(tenx_core::whisper::wav_at(&samples, running.sample_rate));
            }
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
