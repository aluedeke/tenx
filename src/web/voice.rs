//! The voice assistant for the page's talk mode: `tenx-voice`, a Pipecat
//! pipeline in Python that its user runs on their own (`uv run --script
//! tenx-voice.py`), listening on a Unix socket (`tenx_core::voice::
//! socket_path`). `tenx web` looks for it every couple of seconds
//! (`watch`), connects, tells it what to run with (`Setup`: `[voice]`,
//! `[speech]`, `[say]`, this server's `/mcp` and the API key — again when
//! they change), and hands it the page's WebRTC setup and tenx's news as
//! lines (`tenx_core::voice`). The page shows 🎧 only while it is connected
//! (`available`, sent to each tab as `{type: "voice"}`).
//!
//! The audio never passes through here: only the setup, behind the page's
//! cookie (`server.rs`). The program reaches tenx's tools through this
//! server's own `/mcp`, with its bearer token.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tenx_core::voice::{FromVoice, Setup, ToVoice};

use crate::workspace::{VoiceConfig, expand_home};

/// An offer is answered once the program has gathered its ICE candidates.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// How often `watch` looks for the program, and for changed settings.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// Replies the reader thread hands to whoever asked, by request id.
type Waiting = Arc<Mutex<HashMap<u64, mpsc::Sender<FromVoice>>>>;

/// The connection to the running program.
struct Link {
    stream: UnixStream,
    waiting: Waiting,
    /// Pages talking through it now, as the program last said.
    sessions: Arc<AtomicUsize>,
    /// The reader still reads: the program hasn't gone away.
    open: Arc<AtomicBool>,
    /// What it was last told to run with.
    setup: Setup,
}

impl Link {
    fn write(&mut self, msg: &ToVoice) -> Result<()> {
        self.stream.write_all(tenx_core::voice::line(msg).as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }
}

static LINK: Mutex<Option<Link>> = Mutex::new(None);
/// Connected, and the program took its setup: what the page is told.
static AVAILABLE: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// This server's `/mcp` and its token (`start`).
static MCP: OnceLock<(String, String)> = OnceLock::new();

/// Whether the page may offer talk mode.
pub(super) fn available() -> bool {
    AVAILABLE.load(Ordering::SeqCst)
}

/// Start looking for the program, given where it finds tenx's tools.
pub(super) fn start(mcp_url: String, mcp_token: String) {
    if MCP.set((mcp_url, mcp_token)).is_ok() {
        std::thread::Builder::new().name("voice-watch".into()).spawn(watch).ok();
    }
}

fn socket() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    tenx_core::voice::socket_path(&home, std::env::var("TENX_VOICE_SOCKET").ok().as_deref())
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

/// What the program should run with now. A missing key is the program's to
/// report (on the first offer), so the page still shows why.
fn setup() -> Result<Setup> {
    let (mcp_url, mcp_token) = MCP.get().context("tenx web isn't listening yet")?.clone();
    let global = crate::workspace::load_global()?;
    let (voice, speech) = (&global.voice, &global.speech);
    let opt_home = |p: &str| if p.is_empty() { String::new() } else { expand_home(p) };
    Ok(Setup {
        mcp_url,
        mcp_token,
        api_key: api_key(voice).unwrap_or_default(),
        model: voice.model.clone(),
        tts_voice: voice.voice.clone(),
        stt_model: voice.stt_model.clone(),
        // Unset, the microphone button's: one setting for how you speak.
        language: if voice.language.is_empty() { speech.language.clone() } else { voice.language.clone() },
        whisper: super::whisper::program().to_string_lossy().into_owned(),
        whisper_model: opt_home(&speech.model),
        whisper_prompt_file: opt_home(&speech.prompt_file),
        say: tenx_core::readaloud::say_table(&global.say),
    })
}

/// Look for the program for as long as `tenx web` runs: connect when its
/// socket answers, tell it again what to run with when that changed, and
/// let go when it went away.
fn watch() {
    loop {
        if let Err(e) = look() {
            eprintln!("tenx-voice: {e:#}");
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

fn look() -> Result<()> {
    let mut slot = LINK.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|l| !l.open.load(Ordering::SeqCst)) {
        *slot = None;
        AVAILABLE.store(false, Ordering::SeqCst);
        eprintln!("tenx-voice went away");
    }
    let path = socket();
    let Some(link) = slot.as_mut() else {
        let Ok(stream) = UnixStream::connect(&path) else { return Ok(()) };
        *slot = Some(connect(stream)?);
        eprintln!("tenx-voice connected ({})", path.display());
        return Ok(());
    };
    let now = setup()?;
    if now != link.setup {
        link.write(&ToVoice::Setup(now.clone()))?;
        link.setup = now;
    }
    Ok(())
}

fn connect(stream: UnixStream) -> Result<Link> {
    // A program that stopped reading must not hold the lock — and every
    // page's request behind it — for ever.
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let reader = BufReader::new(stream.try_clone()?);
    let mut link = Link {
        stream,
        waiting: Arc::default(),
        sessions: Arc::new(AtomicUsize::new(0)),
        open: Arc::new(AtomicBool::new(true)),
        setup: setup()?,
    };
    link.write(&ToVoice::Setup(link.setup.clone()))?;
    let (waiting, sessions, open) = (link.waiting.clone(), link.sessions.clone(), link.open.clone());
    std::thread::Builder::new()
        .name("voice".into())
        .spawn(move || read(reader, &waiting, &sessions, &open))
        .context("start the voice reader")?;
    Ok(link)
}

/// The socket, until it closes: `ready` makes talk mode available, replies
/// go to whoever waits on their id, `sessions` to the count. When it closes,
/// every waiter's sender is dropped, so none waits for nothing.
fn read(reader: BufReader<UnixStream>, waiting: &Waiting, sessions: &AtomicUsize, open: &AtomicBool) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let msg = match serde_json::from_str::<FromVoice>(line.trim()) {
            Ok(m) => m,
            Err(_) => {
                eprintln!("tenx-voice said {:?}", line.trim());
                continue;
            }
        };
        match msg {
            FromVoice::Ready => AVAILABLE.store(true, Ordering::SeqCst),
            FromVoice::Sessions { count } => sessions.store(count, Ordering::SeqCst),
            FromVoice::Error { id: None, ref error } => eprintln!("tenx-voice: {error}"),
            reply => {
                let to = reply.id().and_then(|id| waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id));
                if let Some(to) = to {
                    let _ = to.send(reply);
                }
            }
        }
    }
    AVAILABLE.store(false, Ordering::SeqCst);
    open.store(false, Ordering::SeqCst);
    sessions.store(0, Ordering::SeqCst);
    waiting.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Send a request made with a fresh id and wait for its reply.
pub(super) fn request(make: impl FnOnce(u64) -> ToVoice) -> Result<FromVoice> {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    {
        let mut slot = LINK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(link) = slot.as_mut().filter(|l| l.open.load(Ordering::SeqCst)) else {
            bail!("the voice assistant isn't running");
        };
        link.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        if let Err(e) = link.write(&make(id)) {
            link.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(e).context("write to tenx-voice");
        }
    }
    match rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(FromVoice::Error { error, .. }) => bail!("{error}"),
        Ok(reply) => Ok(reply),
        Err(mpsc::RecvTimeoutError::Timeout) => bail!("tenx-voice didn't answer"),
        Err(mpsc::RecvTimeoutError::Disconnected) => bail!("tenx-voice went away"),
    }
}

/// A message that needs no reply, for a program with someone connected;
/// dropped otherwise — nobody would hear it.
pub(super) fn tell(msg: &ToVoice) {
    let mut slot = LINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(l) = slot.as_mut()
        && l.sessions.load(Ordering::SeqCst) > 0
        && let Err(e) = l.write(msg)
    {
        eprintln!("tenx-voice: {e:#}");
    }
}

/// Whether anyone is talking to the assistant: news is only worth making
/// when someone will hear it.
pub(super) fn listening() -> bool {
    LINK.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|l| l.sessions.load(Ordering::SeqCst) > 0)
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
