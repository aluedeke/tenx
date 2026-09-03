//! Gathering the live facts `tenx_core::live` summarises — listening ports
//! and PR state — and caching them per task in `.tenx-live.json`.
//!
//! Only `tenx watch` writes the cache (on its 2 s cadence: ports every tick,
//! PRs on a staggered schedule, since each `gh pr view` is a network call);
//! the overlay and `task_json` just read it. So a task's chips can be at
//! most a couple of seconds (ports) or a few minutes (PR) behind, and the
//! overlay never blocks on `lsof` or the network.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use tenx_core::live::{Live, PrInfo};

pub const LIVE_FILE: &str = ".tenx-live.json";

/// How stale a PR lookup may be before it's refreshed — short for a task
/// that's open or running (its PR is what you're working on), long for one
/// that's parked, so a workspace with dozens of old tasks doesn't hammer
/// GitHub.
pub const PR_TTL_ACTIVE: Duration = Duration::from_secs(120);
pub const PR_TTL_PARKED: Duration = Duration::from_secs(30 * 60);
/// `gh` calls per watch tick, at most. Two keeps a cold start (every task
/// stale) from taking a minute of solid network per tick.
pub const PR_LOOKUPS_PER_TICK: usize = 2;

pub fn read(task_dir: &Path) -> Live {
    std::fs::read_to_string(task_dir.join(LIVE_FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn write(task_dir: &Path, live: &Live) -> Result<()> {
    let path = task_dir.join(LIVE_FILE);
    let tmp = task_dir.join(".tenx-live.json.tmp");
    std::fs::write(&tmp, serde_json::to_string(live)?).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Listening ports per task window (keyed by window name = slug), from one
/// `lsof` + one `ps` + one `tmux list-panes` — a few tens of milliseconds
/// for the whole session, regardless of task count.
pub fn ports_by_window() -> HashMap<String, Vec<u16>> {
    let panes = crate::tmux::list_pane_pids().unwrap_or_default();
    if panes.is_empty() {
        return HashMap::new();
    }
    let listeners = tenx_core::live::parse_lsof(&run_capture("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]));
    if listeners.is_empty() {
        return panes.into_iter().map(|(w, _)| (w, vec![])).collect();
    }
    let tree = tenx_core::live::parse_ps(&run_capture("ps", &["-axo", "pid=,ppid="]));
    let mut roots: HashMap<String, Vec<u32>> = HashMap::new();
    for (window, pid) in panes {
        roots.entry(window).or_default().push(pid);
    }
    roots.into_iter().map(|(w, pids)| (w, tenx_core::live::ports_for(&pids, &listeners, &tree))).collect()
}

/// `gh pr view` in `worktree`: the PR for its checked-out branch, if any.
/// `None` also when `gh` is missing, unauthenticated, or offline — the chip
/// simply doesn't appear.
pub fn fetch_pr(repo: &str, worktree: &Path) -> Option<PrInfo> {
    let out = Command::new("gh")
        .current_dir(worktree)
        .args(["pr", "view", "--json", "number,state,url,isDraft,reviewDecision,statusCheckRollup"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    tenx_core::live::summarize_pr(repo, &json)
}

pub fn gh_available() -> bool {
    crate::cli::notify::which("gh").is_some()
}

fn run_capture(bin: &str, args: &[&str]) -> String {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}
