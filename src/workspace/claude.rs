//! Claude Code's own session registry — where every task state comes from.
//!
//! Claude Code writes one `~/.claude/sessions/<pid>.json` per running session
//! and rewrites it whenever the session's status changes (an effect on the
//! status value, not a poll), so it is a push-updated status file — authored by
//! the process that actually knows. Reading it replaced tenx's entire hook
//! pipeline, and fixed what the hooks got wrong:
//!
//! - `Notification` → blocked was a *latch*. Approving a permission prompt fires
//!   no further hook (the tool call just resumes), so a task stayed 💬 while
//!   Claude was busy working. `waiting` is a live value that clears itself.
//! - Session liveness was inferred from whether the *zellij tab* still existed,
//!   after a three-minute grace period. A pid either exists or it doesn't.
//!
//! What it can't express is *how* a turn ended — a failed one goes quiet exactly
//! like a successful one. That distinction was dropped rather than keep a hook
//! for it; see `tenx_core::status::resolve_task_state`.
//!
//! The registry is Claude Code's internal file, not a published API — the
//! supported reader is `claude agents --json`, which costs ~310 ms of node
//! startup and can't serve a 1 s poll. So this is deliberately best-effort:
//! every field is optional and a parse failure drops that one file. If the
//! format ever changes wholesale, the visible result is every task reading as
//! `Inactive`, not a broken overlay.
//!
//! This module is the impure half (filesystem + pid checks); the types and the
//! meaning of a session list live in `tenx_core::status`.

pub use tenx_core::status::{Session, SessionStatus};

use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

/// The on-disk shape. Everything optional: this file belongs to another program
/// and may gain or lose fields between Claude Code releases.
#[derive(Deserialize)]
struct RawSession {
    pid: Option<u32>,
    cwd: Option<String>,
    status: Option<String>,
    #[serde(rename = "waitingFor")]
    waiting_for: Option<String>,
    #[serde(rename = "statusUpdatedAt")]
    status_updated_at: Option<u64>,
    kind: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

/// Every live Claude Code session. Dead entries are dropped: a crashed session
/// leaves its file behind (Claude Code's own concurrent-session count
/// pid-checks for the same reason), and a stale `busy` or `waiting` would be
/// exactly the kind of lie the hooks used to tell.
///
/// Returns empty on any failure — no `~/.claude`, no permission, no sessions.
pub fn sessions() -> Vec<Session> {
    let Ok(home) = std::env::var("HOME") else {
        return vec![];
    };
    let dir = PathBuf::from(home).join(".claude/sessions");
    let Ok(entries) = fs::read_dir(&dir) else {
        return vec![];
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(raw) = serde_json::from_str::<RawSession>(&text) else {
            continue;
        };
        let (Some(pid), Some(cwd)) = (raw.pid, raw.cwd) else {
            continue;
        };
        if !pid_alive(pid) {
            continue;
        }
        out.push(Session {
            pid,
            session_id: raw.session_id,
            cwd: PathBuf::from(cwd),
            status: raw.status.as_deref().map(SessionStatus::from_token).unwrap_or(SessionStatus::Idle),
            waiting_for: raw.waiting_for,
            status_updated_at: raw.status_updated_at.map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
            kind: raw.kind.unwrap_or_default(),
        });
    }
    out
}

/// Where Claude Code keeps a directory's transcripts: `~/.claude/projects/`
/// plus the absolute path with every `/` turned into `-`.
pub fn project_dir(cwd: &std::path::Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let encoded = cwd.to_string_lossy().replace('/', "-");
    Some(PathBuf::from(home).join(".claude/projects").join(encoded))
}

/// True if the process exists. `kill(pid, 0)` performs the permission and
/// existence checks without sending anything. POSIX, so identical on Linux.
pub fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
