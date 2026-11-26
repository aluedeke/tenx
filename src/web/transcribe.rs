//! `/transcribe`: speech from the page's microphone, turned into text on this
//! machine — a model too big for a phone's browser, maybe tuned to its
//! user's voice. The page records; this decides who listens:
//!
//! - `TENX_WEB_STT_URL` set: a whisper.cpp `whisper-server` there (another
//!   machine's, or one run by hand), through the system `curl`, as push does;
//! - else `[speech] url`: a speech server with OpenAI's transcription API
//!   (`sttserver.rs`), which also says the language it heard — with
//!   `tenx-whisper` (`[speech] model`) hearing whenever it fails;
//! - else `[speech] model` in the config: `tenx-whisper`, the program tenx
//!   ships and runs itself (`whisper.rs`);
//! - else nobody, and the page shows how to set a model.
//!
//! Recordings go one at a time either way — the model decodes one at a time.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use tenx_core::dictation::{has_speech, split_at_pauses};
use tenx_core::sttserver;
use tenx_core::web;

use crate::workspace::{SpeechConfig, expand_home};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// An outside server's URL, when one is set.
fn url() -> Option<String> {
    std::env::var("TENX_WEB_STT_URL").ok().map(|u| u.trim().to_string()).filter(|u| !u.is_empty())
}

/// Who turns speech into text here.
pub(super) enum Engine {
    /// A `whisper-server` at this URL.
    Server(String),
    /// A speech server, `tenx-whisper` as its fallback.
    Speech(SpeechServer),
    /// `tenx-whisper` with this `[speech]`.
    Local(SpeechConfig),
}

/// `[speech] url` and what goes with it.
pub(super) struct SpeechServer {
    endpoint: String,
    languages: Vec<String>,
    /// `[speech] prompt_file`'s words, the `prompt` of every request.
    glossary: String,
    /// `tenx-whisper`, when this machine can run it and has a model.
    fallback: Option<SpeechConfig>,
}

/// `[speech]` as `tenx-whisper` sees it: what the server needs left out, so
/// a change there doesn't restart it.
fn whisper_part(speech: &SpeechConfig) -> SpeechConfig {
    SpeechConfig { url: String::new(), languages: Vec::new(), ..speech.clone() }
}

const LOCAL: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

pub(super) fn engine() -> Result<Engine> {
    if let Some(url) = url() {
        return Ok(Engine::Server(url));
    }
    let speech = crate::workspace::load_global()?.speech;
    if !speech.url.trim().is_empty() {
        let glossary = if speech.prompt_file.is_empty() {
            String::new()
        } else {
            std::fs::read_to_string(expand_home(&speech.prompt_file)).unwrap_or_default().trim().to_string()
        };
        return Ok(Engine::Speech(SpeechServer {
            endpoint: sttserver::endpoint(&speech.url),
            languages: speech.languages.clone(),
            glossary,
            fallback: (LOCAL && !speech.model.is_empty()).then(|| whisper_part(&speech)),
        }));
    }
    if !LOCAL {
        bail!("tenx-whisper needs a Mac with Apple silicon; here, set [speech] url to a speech server or TENX_WEB_STT_URL to a whisper.cpp server");
    }
    if speech.model.is_empty() {
        bail!("no speech model: set [speech] model = \"…/ggml-….bin\" in ~/.config/tenx/config.toml");
    }
    Ok(Engine::Local(whisper_part(&speech)))
}

impl SpeechServer {
    /// Start `tenx-whisper` in the background, so it's ready if the server
    /// fails.
    pub(super) fn warm_fallback(&self) {
        if let Some(config) = self.fallback.clone() {
            std::thread::spawn(move || super::whisper::warm(&config));
        }
    }
}

/// One recording's ears: the speech server until it fails, then
/// `tenx-whisper` for the rest of it.
pub(super) struct Listener<'a> {
    server: &'a SpeechServer,
    on_server: bool,
    /// The language: the page's, else the first the server heard clearly.
    language: Option<String>,
    /// What the page asked for (`auto` included), for `tenx-whisper`.
    page: Option<String>,
}

impl<'a> Listener<'a> {
    pub(super) fn new(server: &'a SpeechServer, page: Option<&str>) -> Self {
        Listener {
            server,
            on_server: super::sttserver::available(&server.endpoint),
            language: page.filter(|l| *l != "auto").map(str::to_string),
            page: page.map(str::to_string),
        }
    }

    /// The server's text for `audio`, or `None` when it failed — and from
    /// then on, `tenx-whisper` hears. `before`, what was said before it,
    /// goes into the prompt after the glossary: measured, it took live
    /// dictation from 6.4% to 5.7% word errors in English and from 10.6% to
    /// 5.5% in German.
    pub(super) fn ask(&mut self, audio: &[f32], before: &str) -> Option<String> {
        if !self.on_server {
            return None;
        }
        let field = sttserver::language_field(self.language.as_deref(), &self.server.languages);
        let prompt = sttserver::prompt(&self.server.glossary, before);
        match super::sttserver::ask(&self.server.endpoint, audio, field, &prompt) {
            Ok(heard) => {
                if self.language.is_none() {
                    self.language = heard.settled_language().map(str::to_string);
                }
                Some(heard.text)
            }
            Err(_) => {
                self.on_server = false;
                None
            }
        }
    }

    /// `tenx-whisper` and the language for it: what the server heard, else
    /// the page's.
    pub(super) fn fallback(&self) -> Result<(&'a SpeechConfig, Option<String>)> {
        let config = self.server.fallback.as_ref().context("the speech server failed, and there is no [speech] model for tenx-whisper")?;
        Ok((config, self.language.clone().or_else(|| self.page.clone())))
    }
}

/// The longest piece of a one-shot recording sent in one request: the
/// server writes at most 256 tokens.
const PIECE_SECS: f32 = 25.0;

/// The text of one recording (`content_type` as the browser recorded it).
pub(super) fn transcribe(content_type: &str, audio: &[u8], language: Option<&str>) -> Result<String> {
    match engine()? {
        Engine::Server(url) => from_server(&url, content_type, audio, language),
        Engine::Speech(server) => {
            let samples = tenx_core::whisper::wav_samples(audio).map_err(anyhow::Error::msg)?;
            let mut ears = Listener::new(&server, language);
            let mut texts: Vec<String> = Vec::new();
            for range in split_at_pauses(&samples, 0.01, PIECE_SECS) {
                let piece = &samples[range];
                if !has_speech(piece, 0.01, 0.25) {
                    continue;
                }
                let text = match ears.ask(piece, &texts.join(" ")) {
                    Some(text) => text,
                    None => {
                        let (config, language) = ears.fallback()?;
                        super::whisper::transcribe(config, piece, language.as_deref())?
                    }
                };
                texts.push(text);
            }
            Ok(tenx_core::whisper::join_segments(texts.iter().map(String::as_str)))
        }
        Engine::Local(speech) => {
            let samples = tenx_core::whisper::wav_samples(audio).map_err(anyhow::Error::msg)?;
            super::whisper::transcribe(&speech, &samples, language)
        }
    }
}

/// The same from a `whisper-server`, which reads any format it can convert.
pub(super) fn from_server(url: &str, content_type: &str, audio: &[u8], language: Option<&str>) -> Result<String> {
    let mime = content_type.split(';').next().unwrap_or("").trim();
    let ext = match mime {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/webm" => "webm",
        "audio/ogg" => "ogg",
        _ => bail!("not audio: {content_type:?}"),
    };
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--max-time", "120", "-w", "\n%{http_code}"])
        .args(["-F", &format!("file=@-;type={mime};filename=speech.{ext}")])
        .args(["-F", "response_format=json"]);
    if let Some(lang) = language {
        cmd.args(["-F", &format!("language={lang}")]);
    }
    let mut child = cmd
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run curl")?;
    child.stdin.take().context("curl's stdin")?.write_all(audio)?;
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (body, status) = stdout.rsplit_once('\n').unwrap_or(("", &stdout));
    if !out.status.success() {
        bail!("speech server unreachable: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    if status.trim() != "200" {
        bail!("speech server answered {}: {}", status.trim(), body.chars().take(200).collect::<String>());
    }
    web::stt_text(body).map_err(anyhow::Error::msg)
}
