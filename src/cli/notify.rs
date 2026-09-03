//! Desktop notification delivery, one backend per platform behind a trait so
//! `cli::watch` doesn't care which OS it's on. Every backend is best-effort:
//! a notification that fails to deliver must never stop the watcher.

use std::path::PathBuf;
use std::process::{Command, Stdio};

pub trait Notifier {
    fn notify(&self, title: &str, subtitle: &str, body: &str);
}

/// The notifier for the OS this binary was built for.
pub fn platform() -> Box<dyn Notifier> {
    #[cfg(target_os = "macos")]
    {
        Box::new(MacOs)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(Linux)
    }
}

/// `terminal-notifier` when installed (it can carry a subtitle and group by
/// task), `osascript` otherwise — always present on macOS, so there's no path
/// where the watcher runs but can't speak.
#[cfg(target_os = "macos")]
pub struct MacOs;

#[cfg(target_os = "macos")]
impl Notifier for MacOs {
    fn notify(&self, title: &str, subtitle: &str, body: &str) {
        if which("terminal-notifier").is_some() {
            let _ = Command::new("terminal-notifier")
                .args(["-title", title, "-subtitle", subtitle, "-message", body])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            return;
        }
        let script = format!(
            "display notification {} with title {} subtitle {}",
            applescript_str(body),
            applescript_str(title),
            applescript_str(subtitle),
        );
        let _ = Command::new("osascript")
            .args(["-e", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `notify-send` (libnotify), the de facto desktop-notification CLI on Linux.
/// Absent it, the watcher still runs — it just stays quiet — so a headless box
/// over SSH doesn't error every time a task starts waiting.
#[cfg(not(target_os = "macos"))]
pub struct Linux;

#[cfg(not(target_os = "macos"))]
impl Notifier for Linux {
    fn notify(&self, title: &str, subtitle: &str, body: &str) {
        if which("notify-send").is_none() {
            return;
        }
        let _ = Command::new("notify-send")
            .args(["--app-name", "tenx", title, &format!("{subtitle}\n{body}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Quote a string for AppleScript. Task titles come from `TASK.md`, i.e. from
/// whatever anyone typed — an unescaped quote would turn a notification into a
/// syntax error at best.
#[cfg(target_os = "macos")]
fn applescript_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let out = Command::new("command").args(["-v", bin]).output().ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}
