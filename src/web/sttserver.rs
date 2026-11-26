//! The speech server at `[speech] url`, through the system `curl` (as push
//! and `whisper-server` go): one WAV per request, the time limit and the
//! outage rules from `tenx_core::sttserver`. A server that failed is left
//! alone for `PAUSE_SECS`, for every recording: `tenx-whisper` hears
//! meanwhile.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tenx_core::sttserver::{self, AfterFailure, Failure, Heard};
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
/// (`sttserver::language_field`) and the prompt. A failure is logged, and
/// handled as `sttserver::after_failure` says for a recording with
/// `tenx-whisper` (`fallback`) or without: its outcome comes back with it.
pub(super) fn ask(endpoint: &str, audio: &[f32], language: Option<(&str, String)>, prompt: &str, fallback: bool) -> Result<Heard, AfterFailure> {
    request(endpoint, audio, language, prompt).map_err(|failure| {
        let next = sttserver::after_failure(&failure, fallback);
        let then = match next {
            AfterFailure::Fallback { pause: true } => format!("; tenx-whisper hears for {} s", sttserver::PAUSE_SECS),
            AfterFailure::Fallback { pause: false } => "; tenx-whisper hears this recording".to_string(),
            AfterFailure::Retry => "; asked again".to_string(),
            AfterFailure::GiveUp => "; not asked again for this recording".to_string(),
        };
        eprintln!("tenx web: speech server {endpoint}: {failure}{then}");
        if next == (AfterFailure::Fallback { pause: true }) {
            mark_down(endpoint);
        }
        next
    })
}

/// A file only this user can read, removed when dropped: the prompt holds
/// what was said, and curl's arguments are anybody's to read in `ps`.
struct PrivateFile(PathBuf);

impl PrivateFile {
    fn new(text: &str) -> std::io::Result<Self> {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("tenx-stt-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        let file = PrivateFile(path);
        f.write_all(text.as_bytes())?;
        Ok(file)
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
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
    // The prompt from a file (`<`: its text as the field's value).
    let prompt_file = if prompt.is_empty() { None } else { Some(PrivateFile::new(prompt).map_err(|e| Failure::Down(format!("prompt file: {e}")))?) };
    if let Some(file) = &prompt_file {
        cmd.args(["-F", &format!("prompt=<{}", file.0.display())]);
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
