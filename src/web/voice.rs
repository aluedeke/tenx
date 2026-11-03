//! `tenx-voice`, the voice assistant `tenx web` runs for the page's talk
//! mode: a Pipecat pipeline in Python (`voice.py`, embedded here), written
//! to `~/.config/tenx/voice/` and run with `uv run --script`, which fetches
//! its packages on the first start. Started on the first connection, given
//! the page's WebRTC setup and tenx's news over its stdin/stdout
//! (`tenx_core::voice`), started again if it died, and ended — closing its
//! stdin — once nobody has been connected for `[voice] idle_minutes`. It
//! hears through `tenx-whisper` with `[speech]`'s model and glossary, the
//! microphone button's, so a model tuned to its user serves both.
//!
//! The audio never passes through here: only the setup, behind the page's
//! cookie (`server.rs`). The program reaches tenx's tools through this
//! server's own `/mcp`, with its bearer token.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tenx_core::voice::{FromVoice, ToVoice};

use crate::workspace::{SpeechConfig, VoiceConfig, expand_home};

/// What the program runs with: a change to either restarts it.
#[derive(Clone, PartialEq)]
struct Config {
    voice: VoiceConfig,
    speech: SpeechConfig,
    say: std::collections::BTreeMap<String, String>,
}

impl Config {
    fn load() -> Result<Config> {
        let global = crate::workspace::load_global()?;
        Ok(Config { voice: global.voice, speech: global.speech, say: global.say })
    }
}

const SCRIPT: &str = include_str!("voice.py");

/// The first start installs the packages (torch among them): minutes.
const START_TIMEOUT: Duration = Duration::from_secs(600);
/// An offer is answered once the program has gathered its ICE candidates.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Replies the reader thread hands to whoever asked, by request id.
type Waiting = Arc<Mutex<HashMap<u64, mpsc::Sender<FromVoice>>>>;

struct Running {
    child: Child,
    stdin: ChildStdin,
    waiting: Waiting,
    /// Pages connected now, as the program last said.
    sessions: Arc<AtomicUsize>,
    /// What it was started with: a different `[voice]` or `[speech]`
    /// restarts it.
    config: Config,
    last_used: Instant,
}

impl Running {
    fn stop(mut self) {
        drop(self.stdin); // EOF: it exits by itself
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn write(&mut self, msg: &ToVoice) -> Result<()> {
        self.stdin.write_all(tenx_core::voice::line(msg).as_bytes())?;
        self.stdin.flush()?;
        Ok(())
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static REAPER: Once = Once::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// This server's `/mcp`, set once it listens (`set_mcp`).
static MCP: OnceLock<(String, String)> = OnceLock::new();

/// Where the program finds tenx's tools: this server's `/mcp` and its token.
pub(super) fn set_mcp(url: String, token: String) {
    let _ = MCP.set((url, token));
}

fn idle(config: &Config) -> Duration {
    let minutes = config.voice.idle_minutes;
    Duration::from_secs(60 * u64::from(if minutes == 0 { 10 } else { minutes }))
}

/// The API key: `[voice] key_file`, else `~/.config/tenx/anthropic-key`,
/// else this process's `ANTHROPIC_API_KEY`.
fn api_key(config: &VoiceConfig) -> Result<String> {
    let path = if config.key_file.is_empty() { "~/.config/tenx/anthropic-key".to_string() } else { config.key_file.clone() };
    let path = expand_home(&path);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let key = text.trim();
        if !key.is_empty() {
            return Ok(key.to_string());
        }
    }
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) if !k.trim().is_empty() => Ok(k.trim().to_string()),
        _ => bail!("no Anthropic API key: put it in {path} (chmod 600), or set [voice] key_file"),
    }
}

/// The script on disk, rewritten only when this binary carries another.
fn script() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME")?;
    let dir = PathBuf::from(home).join(".config/tenx/voice");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("tenx-voice.py");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SCRIPT) {
        let tmp = dir.join("tenx-voice.py.tmp");
        std::fs::write(&tmp, SCRIPT).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

fn start(config: &Config) -> Result<Running> {
    let (mcp_url, mcp_token) = MCP.get().context("tenx web isn't listening yet")?.clone();
    let key = api_key(&config.voice)?;
    let script = script()?;
    let speech = &config.speech;
    let whisper_model = if speech.model.is_empty() { String::new() } else { expand_home(&speech.model) };
    let whisper_prompt = if speech.prompt_file.is_empty() { String::new() } else { expand_home(&speech.prompt_file) };
    let voice = &config.voice;
    let mut child = Command::new("uv")
        .args(["run", "--quiet", "--script"])
        .arg(&script)
        .env("ANTHROPIC_API_KEY", key)
        .env("TENX_MCP_URL", mcp_url)
        .env("TENX_MCP_TOKEN", mcp_token)
        .env("TENX_VOICE_MODEL", &voice.model)
        .env("TENX_VOICE_TTS", &voice.voice)
        .env("TENX_VOICE_STT", &voice.stt_model)
        .env("TENX_VOICE_LANGUAGE", &voice.language)
        .env("TENX_WHISPER", super::whisper::program())
        .env("TENX_WHISPER_MODEL", whisper_model)
        .env("TENX_WHISPER_PROMPT_FILE", whisper_prompt)
        .env("TENX_VOICE_SAY", serde_json::to_string(&tenx_core::readaloud::say_table(&config.say))?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("start tenx-voice with uv (is uv installed? https://docs.astral.sh/uv/)")?;
    let stdin = child.stdin.take().context("tenx-voice's stdin")?;
    let stdout = BufReader::new(child.stdout.take().context("tenx-voice's stdout")?);
    let waiting: Waiting = Arc::default();
    let sessions = Arc::new(AtomicUsize::new(0));
    let (ready_tx, ready_rx) = mpsc::channel();
    {
        let waiting = waiting.clone();
        let sessions = sessions.clone();
        std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || read(stdout, &waiting, &sessions, ready_tx))
            .context("start the voice reader")?;
    }
    let running = Running { child, stdin, waiting, sessions, config: config.clone(), last_used: Instant::now() };
    match ready_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            running.stop();
            bail!("tenx-voice: {e}");
        }
        Err(_) => {
            running.stop();
            bail!("tenx-voice didn't start (see tenx web's log)");
        }
    }
    REAPER.call_once(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(30));
                let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(r) = slot.as_mut() {
                    if r.sessions.load(Ordering::SeqCst) > 0 {
                        r.last_used = Instant::now();
                    } else if r.last_used.elapsed() >= idle(&r.config)
                        && let Some(r) = slot.take()
                    {
                        r.stop();
                    }
                }
            }
        });
    });
    Ok(running)
}

/// The program's stdout, until it ends: `ready` to the starter, replies to
/// whoever waits on their id, `sessions` to the count. When it ends, every
/// waiter's sender is dropped, so none waits for nothing.
fn read(
    stdout: BufReader<std::process::ChildStdout>,
    waiting: &Waiting,
    sessions: &AtomicUsize,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let mut ready = Some(ready);
    for line in stdout.lines() {
        let Ok(line) = line else { break };
        let msg = match serde_json::from_str::<FromVoice>(line.trim()) {
            Ok(m) => m,
            Err(_) => {
                eprintln!("tenx-voice said {:?}", line.trim());
                continue;
            }
        };
        match msg {
            FromVoice::Ready => {
                if let Some(r) = ready.take() {
                    let _ = r.send(Ok(()));
                }
            }
            FromVoice::Sessions { count } => sessions.store(count, Ordering::SeqCst),
            FromVoice::Error { id: None, ref error } => {
                if let Some(r) = ready.take() {
                    let _ = r.send(Err(error.clone()));
                } else {
                    eprintln!("tenx-voice: {error}");
                }
            }
            reply => {
                let to = reply.id().and_then(|id| waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id));
                if let Some(to) = to {
                    let _ = to.send(reply);
                }
            }
        }
    }
    sessions.store(0, Ordering::SeqCst);
    waiting.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The running program, started (or restarted, for a changed `[voice]` or
/// one that died) as needed.
fn running<'a>(slot: &'a mut Option<Running>, config: &Config) -> Result<&'a mut Running> {
    if slot.as_mut().is_some_and(|r| r.config != *config || !r.alive())
        && let Some(r) = slot.take()
    {
        r.stop();
    }
    if slot.is_none() {
        *slot = Some(start(config)?);
    }
    Ok(slot.as_mut().expect("just started"))
}

/// Send a request made with a fresh id and wait for its reply.
pub(super) fn request(make: impl FnOnce(u64) -> ToVoice) -> Result<FromVoice> {
    let config = Config::load()?;
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    {
        let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        let running = running(&mut slot, &config)?;
        running.last_used = Instant::now();
        running.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        if let Err(e) = running.write(&make(id)) {
            running.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(e).context("write to tenx-voice");
        }
    }
    match rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(FromVoice::Error { error, .. }) => bail!("{error}"),
        Ok(reply) => Ok(reply),
        Err(mpsc::RecvTimeoutError::Timeout) => bail!("tenx-voice didn't answer"),
        Err(mpsc::RecvTimeoutError::Disconnected) => bail!("tenx-voice ended (see tenx web's log)"),
    }
}

/// A message that needs no reply, for a program that runs with someone
/// connected; dropped otherwise — nobody would hear it. Never waits: the
/// lock is held for minutes while a first start installs the packages, and
/// the push notifier calls this.
pub(super) fn tell(msg: &ToVoice) {
    let Ok(mut slot) = RUNNING.try_lock() else { return };
    if let Some(r) = slot.as_mut()
        && r.sessions.load(Ordering::SeqCst) > 0
        && let Err(e) = r.write(msg)
    {
        eprintln!("tenx-voice: {e:#}");
    }
}

/// Whether anyone is talking to the assistant: news is only worth making
/// when someone will hear it.
pub(super) fn listening() -> bool {
    RUNNING.try_lock().is_ok_and(|slot| slot.as_ref().is_some_and(|r| r.sessions.load(Ordering::SeqCst) > 0))
}

/// What the watcher's news means to the assistant: tasks that started
/// needing the user (its attention edges, shared with push and desktop
/// notifications) and tasks that finished a turn (`Turns`).
pub(super) struct News {
    turns: tenx_core::voice::Turns,
}

impl News {
    pub(super) fn new() -> News {
        News { turns: tenx_core::voice::Turns::default() }
    }

    /// One poll's snapshot and the attention notes it fired.
    pub(super) fn step(&mut self, snapshot: &crate::cli::watch::Snapshot, notes: &[&crate::cli::watch::Note]) {
        let tasks = snapshot.live_tasks();
        let now: Vec<(String, String, &str, &str)> = tasks
            .iter()
            .map(|t| {
                let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("");
                (format!("{}/{}", s("ws"), s("slug")), s("status").to_string(), s("title"), s("ws"))
            })
            .collect();
        let keys: Vec<(&str, &str)> = now.iter().map(|(k, s, _, _)| (k.as_str(), s.as_str())).collect();
        let finished = self.turns.step(&keys);
        if !listening() {
            return;
        }
        for note in notes {
            let reason = note.reason.as_deref();
            let text = if note.secrets {
                tenx_core::voice::secrets_requested(&note.task, &note.workspace, reason.unwrap_or("secrets"))
            } else {
                tenx_core::voice::needs_you(&note.task, &note.workspace, reason)
            };
            tell(&ToVoice::Event { text });
        }
        for key in finished {
            if let Some((_, _, title, ws)) = now.iter().find(|(k, _, _, _)| k == key) {
                tell(&ToVoice::Event { text: tenx_core::voice::finished(title, ws) });
            }
        }
    }
}
