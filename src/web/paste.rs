//! `POST /paste`: an image from the page — pasted, dropped, or picked on a
//! phone — saved on this machine so the agent can read it. The browser's
//! clipboard is on another device; what reaches the agent is the saved
//! file's path, which the page pastes into the terminal (Claude Code attaches
//! a pasted image path as an image).
//!
//! Files go to `~/.config/tenx/web-paste/` (mode 600, the directory 700) and
//! are swept after `PASTE_TTL`.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use tenx_core::web;

fn dir() -> Result<PathBuf> {
    Ok(crate::workspace::home_dir()?.join(".config").join("tenx").join("web-paste"))
}

/// Save `bytes` as a new image of `content_type`; the path it was saved at.
pub(super) fn save(content_type: &str, bytes: &[u8]) -> Result<PathBuf> {
    let ext = web::paste_ext(content_type).with_context(|| format!("not an image an agent can read: {content_type:?}"))?;
    let dir = dir()?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).context("create the paste directory")?;
    sweep(&dir);
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = dir.join(format!("{stamp}-{}.{ext}", super::token::random_hex(4)?));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Delete pastes older than `PASTE_TTL`. Best effort: a file that can't be
/// read or removed is left for next time.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > web::PASTE_TTL);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
