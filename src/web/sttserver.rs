//! The speech server at `[speech] url`, through the system `curl` (as push
//! and `whisper-server` go): one WAV per request, the time limit and the
//! outage rules from `tenx_core::sttserver`. A server that failed is left
//! alone for `PAUSE_SECS`, for every recording: `tenx-whisper` hears
//! meanwhile.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tenx_core::sttserver::{self, Failure, Heard};
use tenx_core::whisper::{self, RATE};

/// Until when a failed server is left alone, by endpoint.
static DOWN: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// Whether the server may be asked now.
pub(super) fn available(endpoint: &str) -> bool {
    let down = DOWN.lock().unwrap_or_else(|e| e.into_inner());
    !down.iter().any(|(url, until)| url == endpoint && Instant::now() < *until)
}

fn mark_down(endpoint: &str) {
    let mut down = DOWN.lock().unwrap_or_else(|e| e.into_inner());
    down.retain(|(url, _)| url != endpoint);
    down.push((endpoint.to_string(), Instant::now() + Duration::from_secs(sttserver::PAUSE_SECS)));
}

/// One request: `audio` (16 kHz mono), the language field
/// (`sttserver::language_field`) and the prompt. A failure that isn't a
/// refusal leaves the server alone for a while.
pub(super) fn ask(endpoint: &str, audio: &[f32], language: Option<(&str, String)>, prompt: &str) -> Result<Heard, Failure> {
    let result = request(endpoint, audio, language, prompt);
    if let Err(Failure::Down(e)) = &result {
        eprintln!("tenx web: speech server {endpoint}: {e}; tenx-whisper hears for {} s", sttserver::PAUSE_SECS);
        mark_down(endpoint);
    }
    if let Err(Failure::Refused(e)) = &result {
        eprintln!("tenx web: speech server {endpoint} refused: {e} — check [speech] url and languages");
    }
    result
}

fn request(endpoint: &str, audio: &[f32], language: Option<(&str, String)>, prompt: &str) -> Result<Heard, Failure> {
    let (connect, total) = sttserver::timeout(audio.len() as f32 / RATE as f32);
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--connect-timeout", &connect.to_string(), "--max-time", &total.to_string(), "-w", "\n%{http_code}"])
        .args(["-F", "file=@-;type=audio/wav;filename=speech.wav"]);
    // --form-string: a value starting with @ or < is text, not a file.
    if let Some((name, value)) = language {
        cmd.args(["--form-string", &format!("{name}={value}")]);
    }
    if !prompt.is_empty() {
        cmd.args(["--form-string", &format!("prompt={prompt}")]);
    }
    let child = cmd.arg(endpoint).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = child.map_err(|e| Failure::Down(format!("run curl: {e}")))?;
    let wav = whisper::wav(audio);
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&wav);
    }
    let out = child.wait_with_output().map_err(|e| Failure::Down(format!("curl: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (body, status) = stdout.rsplit_once('\n').unwrap_or(("", &stdout));
    let answered = if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) };
    sttserver::outcome(answered, status, body)
}
