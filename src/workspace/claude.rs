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
//! `Inactive`, not a broken column.
//!
//! Two liveness filters apply to every read. A pid check drops crashed
//! sessions (their file stays behind). A *scope* check drops live sessions that
//! aren't in tenx's tmux server — `tenx_core::status::in_panes`, fed by
//! `tmux list-panes` and one `ps` — because the registry is per user, not per
//! server: a Claude left running in an abandoned multiplexer, or started in a
//! plain terminal in the same task directory, would otherwise report for a
//! pane nobody can see. That is exactly how a task once sat on "permission
//! prompt" for a day while its visible session was idle. The scope check has
//! one exception, a *parked turn*: newer Claude Code hands a running turn to
//! a worker under its daemon (`claude bg-spare`, off `init`), and it is the
//! worker's entry — not the interactive one, which reads `busy` — that says
//! `waiting` while the permission dialog is on screen — and the interactive
//! entry's own status is frozen from the moment it parked, so it must not be
//! read at all. `in_panes` keeps such a worker when the session that parked
//! it is in our panes, and `fold_parked` merges the pair into the interactive
//! session with the worker's status.
//!
//! This module is the impure half (filesystem + pid checks); the types and the
//! meaning of a session list live in `tenx_core::status`.

pub use tenx_core::status::{Session, SessionStatus, fold_parked, in_panes};

use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

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
    /// `"tenx:@14.%40"` — the pane the session runs in, as Claude Code itself
    /// reports it. Only the pane id is kept (`tenx_core::dialog::pane_id`).
    tmux: Option<String>,
    /// A parked turn: the interactive session's `parkedJobId` names the
    /// daemon-hosted worker's `jobId` (`tenx_core::status::Session`).
    #[serde(rename = "parkedJobId")]
    parked_job_id: Option<String>,
    #[serde(rename = "jobId")]
    job_id: Option<String>,
}

/// Every live Claude Code session *in tenx's tmux server*. Dead entries are
/// dropped: a crashed session leaves its file behind (Claude Code's own
/// concurrent-session count pid-checks for the same reason), and a stale `busy`
/// or `waiting` would be exactly the kind of lie the hooks used to tell. Live
/// sessions outside our panes are dropped too (see the module doc and
/// [`in_panes`]).
///
/// Returns empty on any failure — no `~/.claude`, no permission, no sessions,
/// no server.
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
            pane: raw.tmux.as_deref().and_then(tenx_core::dialog::pane_id),
            parked_job_id: raw.parked_job_id,
            job_id: raw.job_id,
        });
    }
    if out.is_empty() {
        return out;
    }
    let scope = pane_scope();
    fold_parked(in_panes(out, &scope.pane_pids, &scope.tree))
}

/// Snapshot of what `in_panes` needs: the server's pane pids and the process
/// tree. Cached for [`SCOPE_TTL`] because the column calls `sessions()` on
/// every 500 ms tick and this costs two `tmux` calls and a `ps` — the same
/// cadence `tmux::signals` already refreshes at. A session that opens in a new
/// pane can therefore lag by up to that long; nothing else does, since the pid
/// check above stays per-call.
fn pane_scope() -> PaneScope {
    static CACHE: Mutex<Option<(Instant, PaneScope)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, scope)) = cache.as_ref()
        && at.elapsed() < SCOPE_TTL
    {
        return scope.clone();
    }
    let pane_pids: Vec<u32> = crate::tmux::list_pane_pids().unwrap_or_default().into_iter().map(|(_, pid)| pid).collect();
    let tree = if pane_pids.is_empty() {
        vec![]
    } else {
        tenx_core::live::parse_ps(&crate::live::run_capture("ps", &["-axo", "pid=,ppid="]))
    };
    let scope = PaneScope { pane_pids, tree };
    *cache = Some((Instant::now(), scope.clone()));
    scope
}

const SCOPE_TTL: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct PaneScope {
    pane_pids: Vec<u32>,
    tree: Vec<(u32, u32)>,
}

/// Where Claude Code keeps a directory's transcripts: `~/.claude/projects/`
/// plus the absolute path with every `/` turned into `-`.
pub fn project_dir(cwd: &std::path::Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let encoded = cwd.to_string_lossy().replace('/', "-");
    Some(PathBuf::from(home).join(".claude/projects").join(encoded))
}

/// True if the process exists and is not a zombie. `kill(pid, 0)` performs
/// the permission and existence checks without sending anything (POSIX, so
/// identical on Linux) — but it also says yes to a zombie: a process that has
/// exited and only waits for its parent to `wait()` on it. A zombie can hold
/// nothing — no dialog, no turn, no pidfile worth honouring — so it counts as
/// dead here, or a killed watcher whose parent never reaps it blocks the next
/// one from starting ("already running") for as long as the parent lives.
pub fn pid_alive(pid: u32) -> bool {
    let exists = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    exists && !zombie(pid)
}

/// Whether `pid` has exited but not been reaped. macOS: `sysctl
/// KERN_PROC_PID` — what `ps` reads; its `p_stat` is `SZOMB` for a zombie.
/// (`proc_pidinfo` cannot serve here: it answers ESRCH for a zombie.) Linux:
/// `/proc/<pid>/stat`'s state field, after the parenthesised command name.
/// Elsewhere, or when the query itself fails, the answer is "not a zombie",
/// so the plain existence check above stays the verdict.
#[cfg(target_os = "macos")]
fn zombie(pid: u32) -> bool {
    // `struct kinfo_proc` (sys/sysctl.h), which libc doesn't declare for
    // macOS. Its layout is a public, frozen ABI on 64-bit Darwin:
    // 648 bytes, `kp_proc.p_stat` (u8) at 36, `kp_proc.p_pid` (i32) at 40.
    // The pid read back is checked against the one asked for, so a layout
    // that ever differs fails closed (reads as "not a zombie"), never wrong.
    const SIZE: usize = 648;
    const P_STAT: usize = 36;
    const P_PID: usize = 40;
    let mut buf = [0u8; SIZE];
    let mut len = SIZE;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid as libc::c_int];
    let rc = unsafe {
        libc::sysctl(mib.as_mut_ptr(), mib.len() as libc::c_uint, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
    };
    if rc != 0 || len != SIZE {
        return false;
    }
    let read_pid = i32::from_ne_bytes(buf[P_PID..P_PID + 4].try_into().unwrap());
    read_pid == pid as i32 && u32::from(buf[P_STAT]) == libc::SZOMB
}

#[cfg(target_os = "linux")]
fn zombie(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (comm) state ...` — comm may contain spaces and parentheses, so
    // the state is the first field after the last `)`.
    stat.rsplit(')').next().and_then(|rest| rest.split_whitespace().next()) == Some("Z")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn zombie(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// A child that has exited but not been waited on is a zombie: it still
    /// passes `kill(pid, 0)`, and must not count as alive.
    #[test]
    fn a_zombie_is_not_alive() {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        // `true` exits at once; give the kernel a moment to mark it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while pid_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(unsafe { libc::kill(pid as libc::pid_t, 0) } == 0, "zombie should still exist for kill(0)");
        assert!(!pid_alive(pid), "an unreaped exited child must read as dead");
        child.wait().unwrap();
        assert!(!pid_alive(pid));
        assert!(pid_alive(std::process::id()));
    }
}
