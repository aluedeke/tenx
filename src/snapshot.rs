//! The watcher's task snapshot on disk (`~/.config/tenx/sidebar.json`) —
//! written only by `tenx watch`, read by every sidebar pane. The document
//! shape is `tenx_core::snapshot`; this is the I/O.
//!
//! Written atomically (temp file + rename) so a reader never sees a partial
//! document. When nothing changed the watcher only bumps the mtime: readers
//! poll the mtime, not the contents, and the mtime doubles as the liveness
//! signal (`Snapshot::is_fresh`) — a file nobody has touched for a while
//! means no watcher is running.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub use tenx_core::snapshot::{Snapshot, TaskSnap};

/// One file per tmux server, like the watcher's pidfile and the generated
/// tmux config, so a build tried on its own socket has its own snapshot.
pub fn path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("$HOME not set")?;
    let sock = crate::tmux::socket();
    let name = if sock == crate::tmux::SOCKET { "sidebar.json".to_string() } else { format!("sidebar-{sock}.json") };
    Ok(PathBuf::from(home).join(".config").join("tenx").join(name))
}

/// Write `snapshot` if it differs from `last` (the JSON last written, updated
/// in place), else just touch the file. Returns the serialised document.
pub fn publish(snapshot: &Snapshot, last: &mut String) -> Result<()> {
    let text = serde_json::to_string(snapshot)?;
    let path = path()?;
    if text == *last && path.exists() {
        touch(&path);
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    *last = text;
    Ok(())
}

fn touch(path: &std::path::Path) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// The snapshot's mtime, if the file exists and the watcher touched it
/// recently. What a reader polls: `None` means resolve for yourself.
pub fn modified_fresh() -> Option<SystemTime> {
    let mtime = std::fs::metadata(path().ok()?).ok()?.modified().ok()?;
    is_fresh(mtime).then_some(mtime)
}

fn is_fresh(mtime: SystemTime) -> bool {
    let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Snapshot::is_fresh(secs(mtime), secs(SystemTime::now()))
}

/// Read the snapshot, with its mtime. `None` when there is no file, it
/// doesn't parse, or it is stale — every case where a reader should resolve
/// for itself instead.
pub fn read_fresh() -> Option<(Snapshot, SystemTime)> {
    let mtime = modified_fresh()?;
    let text = std::fs::read_to_string(path().ok()?).ok()?;
    Some((serde_json::from_str(&text).ok()?, mtime))
}
