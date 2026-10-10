//! `/transcribe`: speech from the page's microphone, turned into text on this
//! machine — a model too big for a phone's browser, maybe tuned to its
//! user's voice. The page records; this decides who listens:
//!
//! - `TENX_WEB_STT_URL` set: a whisper.cpp `whisper-server` there (another
//!   machine's, or one run by hand), through the system `curl`, as push does;
//! - else `[speech] model` in the config: `tenx-whisper`, the program tenx
//!   ships and runs itself (`whisper.rs`);
//! - else nobody, and the page shows how to set a model.
//!
//! Recordings go one at a time either way — the model decodes one at a time.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use tenx_core::web;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// An outside server's URL, when one is set.
fn url() -> Option<String> {
    std::env::var("TENX_WEB_STT_URL").ok().map(|u| u.trim().to_string()).filter(|u| !u.is_empty())
}

/// The text of one recording (`content_type` as the browser recorded it).
pub(super) fn transcribe(content_type: &str, audio: &[u8], language: Option<&str>) -> Result<String> {
    if let Some(url) = url() {
        return from_server(&url, content_type, audio, language);
    }
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        bail!("tenx-whisper needs a Mac with Apple silicon; here, set TENX_WEB_STT_URL to a whisper.cpp server");
    }
    let speech = crate::workspace::load_global()?.speech;
    if speech.model.is_empty() {
        bail!("no speech model: set [speech] model = \"…/ggml-….bin\" in ~/.config/tenx/config.toml");
    }
    let samples = tenx_core::whisper::wav_samples(audio).map_err(anyhow::Error::msg)?;
    super::whisper::transcribe(&speech, &samples, language)
}

/// The same from a `whisper-server`, which reads any format it can convert.
fn from_server(url: &str, content_type: &str, audio: &[u8], language: Option<&str>) -> Result<String> {
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
