//! tenx's own session registry — where every task's state comes from, for every
//! agent.
//!
//! tenx keeps one record per live agent process at
//! `~/.config/tenx/sessions/<pid>.json`. Each agent writes it through the same
//! path: its hook (Claude Code, Codex) or extension (pi) fires on a lifecycle
//! event, calls `tenx internal session-event`, and that command applies
//! `tenx_core::session_event` and rewrites this file. The record's shape mirrors
//! what Claude Code's internal registry used, so the reader and the status
//! model below are unchanged from when tenx read that file directly — only the
//! source moved, from one vendor's private file to a file tenx owns and every
//! agent feeds.
//!
//! Two liveness filters apply to every read, exactly as before. A pid check
//! drops crashed sessions (their file stays behind until the watcher prunes it).
//! A *scope* check drops live sessions that aren't in tenx's tmux server
//! (`tenx_core::status::in_panes`), because the registry is per user, not per
//! server: an agent left running in an abandoned multiplexer, or started in a
//! plain terminal in the same task directory, would otherwise report for a pane
//! nobody can see.
//!
//! This module is the impure half (filesystem + pid checks); the types and the
//! meaning of a session list live in `tenx_core::status`, and what an event
//! means for a record lives in `tenx_core::session_event`.

pub use tenx_core::status::{Session, SessionStatus, fold_parked, in_panes};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The default agent when a workspace/task names none, and the value the reader
/// substitutes for a record whose `agent` field predates the field.
pub const DEFAULT_AGENT: &str = "claude";

/// The on-disk record. Field names match Claude Code's original registry so the
/// format is familiar; `agent` and `transcriptPath` are tenx's additions.
/// Everything optional: a record may be written by an older tenx or a partially
/// updated agent integration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Record {
    pub pid: Option<u32>,
    pub cwd: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "waitingFor", skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    #[serde(rename = "statusUpdatedAt", skip_serializing_if = "Option::is_none")]
    pub status_updated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "transcriptPath", skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// The pane the session runs in (`"tenx:@14.%40"`), captured from the
    /// hook's `TMUX_PANE`. Only the pane id is kept in `Session`
    /// (`tenx_core::dialog::pane_id`) — the overlay's approve-in-place preview
    /// sends keys to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
    /// Parked-turn linkage, when an agent hands a turn to a daemon worker
    /// (Claude Code's `claude bg-spare`). Absent for hook-written records; then
    /// `fold_parked` is a no-op and the interactive session reports directly.
    #[serde(rename = "parkedJobId", skip_serializing_if = "Option::is_none")]
    pub parked_job_id: Option<String>,
    #[serde(rename = "jobId", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

/// Directory holding tenx's session records.
pub fn registry_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/tenx/sessions"))
}

fn record_path(pid: u32) -> Option<PathBuf> {
    Some(registry_dir()?.join(format!("{pid}.json")))
}

/// Read one session record, or `None` if it's absent or unreadable.
pub fn read_record(pid: u32) -> Option<Record> {
    let path = record_path(pid)?;
    let text = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write (atomically) a session record for `pid`.
pub fn write_record(pid: u32, record: &Record) -> Result<()> {
    let dir = registry_dir().context("no $HOME for session registry")?;
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("{pid}.json"));
    let tmp = dir.join(format!("{pid}.json.tmp.{}", std::process::id()));
    let text = serde_json::to_string(record).context("serialize session record")?;
    fs::write(&tmp, &text).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Remove a session record (a session that ended). Missing file is not an error.
pub fn delete_record(pid: u32) {
    if let Some(path) = record_path(pid) {
        let _ = fs::remove_file(path);
    }
}

/// Milliseconds since the Unix epoch, for `statusUpdatedAt`.
pub fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Every live agent session *in tenx's tmux server*. Dead entries are dropped (a
/// crashed session leaves its file behind, pruned by the watcher); live sessions
/// outside our panes are dropped too (see the module doc and [`in_panes`]).
///
/// Returns empty on any failure — no registry dir, no permission, no server.
pub fn sessions() -> Vec<Session> {
    let Some(dir) = registry_dir() else {
        return vec![];
    };
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
        let Ok(raw) = serde_json::from_str::<Record>(&text) else {
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
            kind: if raw.kind.is_empty() { "interactive".to_string() } else { raw.kind },
            agent: raw.agent.unwrap_or_else(|| DEFAULT_AGENT.to_string()),
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

/// Prune records whose pid is no longer alive — the watcher's housekeeping so a
/// crashed agent's file can't pin a task forever. Returns how many were removed.
pub fn prune_dead() -> usize {
    let Some(dir) = registry_dir() else { return 0 };
    let Ok(entries) = fs::read_dir(&dir) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let alive = read_stem_pid(&path).is_some_and(pid_alive);
        if !alive {
            let _ = fs::remove_file(&path);
            removed += 1;
        }
    }
    removed
}

/// Snapshot of what `in_panes` needs: the server's pane pids and the process
/// tree. Cached for [`SCOPE_TTL`] because the overlay calls `sessions()` on
/// every tick and this costs two `tmux` calls and a `ps`.
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
/// plus the absolute path with every `/` turned into `-`. Still read for the
/// resume decision and the agent-log/standup transcript views — content, not
/// state.
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

/// Claude Code's global config file (`~/.claude.json`), where its
/// per-directory trust grants live.
pub fn global_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude.json"))
}

/// Pre-approve Claude Code's directory-trust dialog for `dir`, so a task's first
/// Claude session there doesn't stop on it. Grants trust for both the given and
/// the canonical path. Returns whether the config was changed; best-effort and
/// never fatal (see `tenx_core::trust::grant_trust`).
pub fn trust_dir(dir: &std::path::Path) -> Result<bool> {
    let Some(path) = global_config_path() else {
        return Ok(false);
    };
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let mut config: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    let given = dir.to_string_lossy().into_owned();
    let mut keys = vec![given.clone()];
    if let Ok(real) = fs::canonicalize(dir) {
        let real = real.to_string_lossy().into_owned();
        if real != given {
            keys.push(real);
        }
    }
    if !tenx_core::trust::grant_trust(&mut config, &keys) {
        return Ok(false);
    }

    let tmp = path.with_extension("json.tenx-tmp");
    let out = serde_json::to_string_pretty(&config)?;
    fs::write(&tmp, &out).with_context(|| format!("write {}", tmp.display()))?;
    if let Ok(meta) = fs::metadata(&path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(true)
}

/// Climb the parent-process chain from `start` (exclusive of nothing — `start`
/// itself is checked first) up to `max_hops` levels, returning the first pid
/// whose process command contains any of `names`. This is how a hook — a short
/// child process of the agent — finds the agent's own pid to key its record by.
///
/// `comm_of` and `ppid_of` are injected so the walk is unit-testable; the binary
/// passes `ps`-backed closures.
pub fn find_agent_pid(
    start: u32,
    names: &[&str],
    max_hops: u32,
    comm_of: &dyn Fn(u32) -> Option<String>,
    ppid_of: &dyn Fn(u32) -> Option<u32>,
) -> Option<u32> {
    let mut pid = start;
    for _ in 0..=max_hops {
        if let Some(comm) = comm_of(pid)
            && names.iter().any(|n| comm.contains(n))
        {
            return Some(pid);
        }
        match ppid_of(pid) {
            Some(pp) if pp != pid && pp > 1 => pid = pp,
            _ => break,
        }
    }
    None
}

fn read_stem_pid(path: &Path) -> Option<u32> {
    path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn climbs_ppid_chain_to_the_agent() {
        // hook(500) → shell(400) → claude(300) → tmux(200)
        let comm = |pid: u32| -> Option<String> {
            Some(match pid {
                500 => "session-event",
                400 => "sh",
                300 => "claude",
                200 => "tmux",
                _ => return None,
            }
            .to_string())
        };
        let ppid = |pid: u32| -> Option<u32> {
            Some(match pid {
                500 => 400,
                400 => 300,
                300 => 200,
                _ => return None,
            })
        };
        assert_eq!(find_agent_pid(500, &["claude"], 6, &comm, &ppid), Some(300));
        // Codex execs the hook directly: parent is already the agent.
        let comm2 = |pid: u32| (pid == 300).then(|| "codex".to_string());
        let ppid2 = |pid: u32| (pid == 500).then_some(300);
        assert_eq!(find_agent_pid(300, &["codex"], 6, &comm2, &ppid2), Some(300));
        // No match within the hop budget → None (caller falls back to getppid).
        assert_eq!(find_agent_pid(500, &["nope"], 6, &comm, &ppid), None);
    }

    #[test]
    fn stem_pid_parses_record_filenames() {
        assert_eq!(read_stem_pid(Path::new("/x/1234.json")), Some(1234));
        assert_eq!(read_stem_pid(Path::new("/x/notapid.json")), None);
    }
}
