//! Claude Code's own session registry — the live half of a task's status.
//!
//! Claude Code writes one `~/.claude/sessions/<pid>.json` per running session and
//! rewrites it whenever the session's status changes (an effect on the status
//! value, not a poll), so it is a push-updated status file exactly like our
//! `.tenx-status` — but authored by the process that actually knows. Reading it
//! replaces four hooks (`SessionStart`, `SessionEnd`, `UserPromptSubmit`,
//! `Notification`) and, more importantly, fixes what they got wrong:
//!
//! - `Notification` → blocked was a *latch*. Approving a permission prompt fires
//!   no further hook (the tool call just resumes), so a task stayed 💬 while
//!   Claude was busy working. `waiting` is a live value that clears itself.
//! - Session liveness was inferred from whether the *zellij tab* still existed,
//!   after a `STALE_AFTER` grace period. A pid either exists or it doesn't.
//!
//! What it can't do is `done`/`failed`: both are `idle` here, so `Stop` and
//! `StopFailure` remain the only source for those two (see `resolve_status`).
//!
//! The registry is Claude Code's internal file, not a published API — the
//! supported reader is `claude agents --json`, which costs ~310 ms of node
//! startup and can't serve a 1 s poll. So this is deliberately best-effort:
//! every field is optional, a parse failure drops the one file, and a caller
//! with no sessions at all falls back to the hook state it already had.

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Statuses Claude Code writes (its own `["busy","shell","idle","waiting"]`).
/// `shell` is idle-with-a-background-shell; we treat it as idle. Anything
/// unrecognised is treated as idle too, so a new status in a future version
/// degrades to "session present, nothing to report" rather than a wrong glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Busy,
    Waiting,
    Idle,
}

impl SessionStatus {
    fn from_token(token: &str) -> SessionStatus {
        match token {
            "busy" => SessionStatus::Busy,
            "waiting" => SessionStatus::Waiting,
            _ => SessionStatus::Idle,
        }
    }
}

/// One live Claude Code session.
#[derive(Debug, Clone)]
pub struct Session {
    pub cwd: PathBuf,
    pub status: SessionStatus,
    /// Why the session is waiting, straight from Claude Code — "input needed",
    /// "sandbox request", or the open dialog's own label. `None` unless
    /// `status` is `Waiting`.
    pub waiting_for: Option<String>,
    /// When the status last changed. Drives the age column for a waiting task
    /// (how long it's been sitting on the prompt).
    pub status_updated_at: Option<SystemTime>,
    /// `interactive`, `bg`, `daemon`, … Background agents run in a task's
    /// subdirectory and are invisible to our hooks, so they only show up here —
    /// which is why a task's session count can exceed the one tab you opened.
    pub kind: String,
}

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
}

/// Every live Claude Code session, newest status first. Dead entries are
/// dropped: a crashed session leaves its file behind (Claude Code's own
/// concurrent-session count pid-checks for the same reason), and a stale `busy`
/// or `waiting` would be exactly the kind of lie the hooks used to tell.
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
            cwd: PathBuf::from(cwd),
            status: raw.status.as_deref().map(SessionStatus::from_token).unwrap_or(SessionStatus::Idle),
            waiting_for: raw.waiting_for,
            status_updated_at: raw
                .status_updated_at
                .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
            kind: raw.kind.unwrap_or_default(),
        });
    }
    out
}

/// Sessions running in `task_dir` or anywhere beneath it. Background agents get
/// their own subdirectory (`tasks/<slug>/ios-agent`), so this is a prefix match,
/// not equality — which is also why our hooks never saw them: they wrote a
/// `.tenx-status` into a directory no task ever reads.
pub fn sessions_for<'a>(sessions: &'a [Session], task_dir: &Path) -> Vec<&'a Session> {
    sessions
        .iter()
        .filter(|s| s.cwd == task_dir || s.cwd.starts_with(task_dir))
        .collect()
}

/// True if the process exists. `kill(pid, 0)` performs the permission and
/// existence checks without sending anything.
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
