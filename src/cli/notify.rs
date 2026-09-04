//! Desktop notification delivery, one backend per platform behind a trait so
//! `cli::watch` doesn't care which OS it's on. Every backend is best-effort:
//! a notification that fails to deliver must never stop the watcher.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The mark (`docs/logo/tenx-mark-256.png`), embedded so a notification can
/// carry it without the binary knowing where the repo is.
const ICON_PNG: &[u8] = include_bytes!("../../docs/logo/tenx-mark-256.png");

/// Path of the icon file the notifiers point at: `~/.config/tenx/tenx.png`,
/// written from the embedded bytes when missing or from an older build (a
/// size mismatch is enough — the file is ours). `None` when there is no home
/// directory or the write fails; the notification then just goes without.
pub fn icon_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    icon_path_in(&Path::new(&home).join(".config").join("tenx"))
}

fn icon_path_in(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("tenx.png");
    let current = std::fs::metadata(&path).is_ok_and(|m| m.len() == ICON_PNG.len() as u64);
    if !current {
        std::fs::create_dir_all(dir).ok()?;
        std::fs::write(&path, ICON_PNG).ok()?;
    }
    Some(path)
}

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
            let mut cmd = Command::new("terminal-notifier");
            cmd.args(["-title", title, "-subtitle", subtitle, "-message", body]);
            // `-contentImage` shows on every macOS; `-appIcon` only where the
            // OS still honours it. Both are harmless when ignored.
            if let Some(icon) = icon_path() {
                let icon = icon.to_string_lossy().into_owned();
                cmd.args(["-contentImage", &icon, "-appIcon", &icon]);
            }
            let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
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
        let mut cmd = Command::new("notify-send");
        cmd.args(["--app-name", "tenx"]);
        if let Some(icon) = icon_path() {
            cmd.arg("--icon").arg(icon);
        }
        let _ = cmd
            .args([title, &format!("{subtitle}\n{body}")])
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

/// Find `bin` on `$PATH`. A PATH scan rather than `command -v`: `command` is
/// a shell builtin, and only macOS also ships it as `/usr/bin/command`, so
/// spawning it directly always failed on Linux — silently disabling every
/// caller (notifications, `gh`).
pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| is_executable(p))
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn which_finds_sh_and_not_nonsense() {
        assert!(super::which("sh").is_some());
        assert!(super::which("definitely-not-a-binary-tenx").is_none());
    }

    #[test]
    fn icon_is_written_once_and_refreshed_when_stale() {
        let dir = std::env::temp_dir().join(format!("tenx-icon-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = super::icon_path_in(&dir).expect("icon path");
        assert_eq!(std::fs::read(&path).unwrap(), super::ICON_PNG);
        // A stale file (different size) is replaced; a current one is kept.
        std::fs::write(&path, b"old").unwrap();
        super::icon_path_in(&dir).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), super::ICON_PNG);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
