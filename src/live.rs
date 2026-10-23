//! Gathering the live facts `tenx_core::live` summarises — listening ports,
//! PR state, and the links the agent and `TASK.md` name — and caching them
//! per task in `.tenx-live.json`.
//!
//! Only `tenx watch` writes the cache (on its 2 s cadence: ports every tick,
//! PRs on a staggered schedule, since each `gh pr view` is a network call);
//! the column and `task_json` just read it. So a task's chips can be at
//! most a couple of seconds (ports) or a few minutes (PR) behind, and the
//! column never blocks on `lsof` or the network.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use tenx_core::live::{Live, PrInfo};
use tenx_core::live::Scanned;

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

/// Transcript lookups per watch tick for tasks not read yet or with a new
/// session: finding one lists a directory (Codex: a tree of them), and a
/// first read can be megabytes. Reading on from a known offset is not
/// counted — it is one `stat` and, while an agent works, a few new lines.
pub const TRANSCRIPT_LOOKUPS_PER_TICK: usize = 1;

/// A first read of a transcript starts this far from its end: the links you
/// come back for are the recent ones, and a long session's log can run to
/// tens of megabytes.
const FIRST_READ_MAX: u64 = 8 << 20;

/// The links in the task's `TASK.md` `## Links` section.
pub fn task_links(task_dir: &Path) -> Vec<tenx_core::links::Link> {
    std::fs::read_to_string(task_dir.join("TASK.md")).map(|md| tenx_core::links::task_md_links(&md)).unwrap_or_default()
}

/// Read the agent's replies written since the last look and merge the links
/// they name into `live.links`. With `session`, its own transcript (wait for
/// it rather than read another); without one, the newest for the task
/// directory, found once — nothing writes to it without a session.
pub fn scan_links(task_dir: &Path, agent: &str, session: Option<&str>, live: &mut Live, lookups: &mut usize) {
    let known = live
        .scanned
        .as_ref()
        .map(|s| PathBuf::from(&s.path))
        .filter(|p| session.is_none_or(|id| p.to_string_lossy().contains(id)) && p.is_file());
    let path = match known {
        Some(p) => p,
        None => {
            if (session.is_none() && live.scanned.is_some()) || *lookups == 0 {
                return;
            }
            *lookups -= 1;
            let found = crate::cli::agentlog::locate_transcript(agent, &task_dir.to_string_lossy(), session);
            match found {
                Some(p) if session.is_none_or(|id| p.to_string_lossy().contains(id)) => p,
                // A session that hasn't written its transcript yet: look again.
                Some(_) | None if session.is_some() => return,
                // No transcript at all: remember that it was looked for.
                _ => {
                    live.scanned = Some(Scanned::default());
                    return;
                }
            }
        }
    };
    let Ok(mut file) = std::fs::File::open(&path) else { return };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let path_str = path.to_string_lossy().into_owned();
    let from = match &live.scanned {
        Some(s) if s.path == path_str && s.offset <= len => s.offset,
        _ => len.saturating_sub(FIRST_READ_MAX),
    };
    if from == len {
        live.scanned = Some(Scanned { path: path_str, offset: len });
        return;
    }
    let mut buf = Vec::new();
    use std::io::{Read, Seek, SeekFrom};
    if file.seek(SeekFrom::Start(from)).is_err() || file.take(len - from).read_to_end(&mut buf).is_err() {
        return;
    }
    // Whole lines only: the agent may be halfway through writing the last.
    // A first read that starts mid-file drops the partial line it lands in.
    let end = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let start = if from > 0 && live.scanned.as_ref().is_none_or(|s| s.path != path_str) {
        buf[..end].iter().position(|&b| b == b'\n').map_or(end, |i| i + 1)
    } else {
        0
    };
    let text = String::from_utf8_lossy(&buf[start..end]);
    let found: Vec<_> = text
        .lines()
        .filter_map(|l| tenx_core::transcript::parse_line(agent, l))
        .filter(|e| e.role == tenx_core::transcript::Role::Assistant)
        .flat_map(|e| tenx_core::links::find(&e.text))
        .collect();
    if !found.is_empty() {
        live.links = tenx_core::links::merge(&live.links, &found);
    }
    live.scanned = Some(Scanned { path: path_str, offset: from + end as u64 });
}

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
        .args(["pr", "view", "--json", "number,title,state,url,isDraft,reviewDecision,statusCheckRollup"])
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

/// Whether `gh` is on `PATH` — resolved once per process; the watcher asks
/// every tick and the answer doesn't change under it.
pub fn gh_available() -> bool {
    static GH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GH.get_or_init(|| crate::cli::notify::which("gh").is_some())
}

pub(crate) fn run_capture(bin: &str, args: &[&str]) -> String {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn reply(text: &str) -> String {
        serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}}).to_string() + "\n"
    }

    /// The transcript is read on from where the last tick stopped, a line
    /// still being written waits for the next one, and only the agent's own
    /// words count — a tool result's links are not the task's.
    #[test]
    fn scan_links_reads_on_from_the_offset() {
        let dir = std::env::temp_dir().join(format!("tenx-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let tool = serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "https://claude.ai/design/p/other"}]}});
        std::fs::write(&path, reply("Made [the proposal](https://claude.ai/code/artifact/a1).") + &tool.to_string() + "\n").unwrap();
        let mut live = Live { scanned: Some(Scanned { path: path.to_string_lossy().into(), offset: 0 }), ..Default::default() };
        let mut lookups = 0;
        scan_links(&dir, "claude", None, &mut live, &mut lookups);
        assert_eq!(live.links.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(), vec!["the proposal"]);

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        let next = reply("PR: https://github.com/o/r/pull/7");
        let (done, partial) = next.split_at(next.len() / 2);
        f.write_all(done.as_bytes()).unwrap();
        scan_links(&dir, "claude", None, &mut live, &mut lookups);
        assert_eq!(live.links.len(), 1, "half a line is not read");
        f.write_all(partial.as_bytes()).unwrap();
        scan_links(&dir, "claude", None, &mut live, &mut lookups);
        assert_eq!(live.links.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(), vec!["r#7", "the proposal"]);
        assert_eq!(live.scanned.as_ref().unwrap().offset, std::fs::metadata(&path).unwrap().len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
