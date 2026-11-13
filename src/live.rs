//! Gathering the live facts `tenx_core::live` summarises — listening ports,
//! PR state, and the links the agent and `TASK.md` name — and caching them
//! per task in `.tenx-live.json`.
//!
//! Only `tenx watch` writes the cache (on its 2 s cadence: ports every tick,
//! PRs on a staggered schedule, since each `gh api graphql` is a network
//! call);
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
/// Tasks whose PRs are looked up per watch tick, at most — one `gh api
/// graphql` request each. Two keeps a cold start (every task stale) from
/// taking a minute of solid network per tick.
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

/// How stale a repo's git state may get: it is local and cheap, but a few
/// processes per repo, so a parked task's is read seldom.
pub const GIT_TTL_ACTIVE: Duration = Duration::from_secs(30);
pub const GIT_TTL_PARKED: Duration = Duration::from_secs(10 * 60);
/// Repos whose git state is read per watch tick, at most.
pub const GIT_REPOS_PER_TICK: usize = 4;

/// The links in the task's `TASK.md` `## Links` section, and its todos.
pub fn task_md(task_dir: &Path) -> (Vec<tenx_core::links::Link>, Option<tenx_core::sidebar::Todos>) {
    match std::fs::read_to_string(task_dir.join("TASK.md")) {
        Ok(md) => (tenx_core::links::task_md_links(&md), tenx_core::sidebar::todos(&md)),
        Err(_) => (Vec::new(), None),
    }
}

/// A repo worktree's git state: `git status`, then how far its branch is from
/// the default branch it started from (`origin/<default>`, or the local one
/// while there is no remote-tracking ref: `git::base_ref`).
///
/// `--no-optional-locks`: a plain `git status` may take `index.lock` to
/// refresh the index, and the agent's `git commit` in the same worktree
/// would then fail on it.
pub fn fetch_git(repo: &str, worktree: &Path) -> Option<tenx_core::sidebar::RepoGit> {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let mut g = tenx_core::sidebar::parse_status(repo, &git(&["status", "--porcelain=v2", "--branch"])?);
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).map(|s| PathBuf::from(s.trim()));
    let base = common.and_then(|c| crate::git::base_ref(&c).ok()).unwrap_or_else(|| "origin/main".to_string());
    g.base = base.trim_start_matches("origin/").to_string();
    if let Some((behind, ahead)) = git(&["rev-list", "--left-right", "--count", &format!("{base}...HEAD")]).and_then(|o| tenx_core::sidebar::parse_counts(&o)) {
        g.behind = behind;
        g.ahead = ahead;
    }
    Some(g)
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
    port_scan().by_window
}

/// One port scan: the ports per task window, and what they came from — every
/// listener (pid, port) and the process tree — for the ports of processes
/// outside a window's own tree (`cli::service::shown_in`).
#[derive(Default)]
pub struct PortScan {
    pub by_window: HashMap<String, Vec<u16>>,
    pub listeners: Vec<(u32, u16)>,
    pub tree: Vec<(u32, u32)>,
}

pub fn port_scan() -> PortScan {
    let panes = crate::tmux::list_pane_pids().unwrap_or_default();
    if panes.is_empty() {
        return PortScan::default();
    }
    let listeners = tenx_core::live::parse_lsof(&run_capture("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]));
    if listeners.is_empty() {
        return PortScan { by_window: panes.into_iter().map(|(w, _)| (w, vec![])).collect(), ..Default::default() };
    }
    let tree = tenx_core::live::parse_ps(&run_capture("ps", &["-axo", "pid=,ppid="]));
    let mut roots: HashMap<String, Vec<u32>> = HashMap::new();
    for (window, pid) in panes {
        roots.entry(window).or_default().push(pid);
    }
    let by_window = roots.into_iter().map(|(w, pids)| (w, tenx_core::live::ports_for(&pids, &listeners, &tree))).collect();
    PortScan { by_window, listeners, tree }
}

/// A task's PRs in one `gh api graphql` request per GitHub host
/// (`tenx_core::prstate`): the newest PR of each repo worktree's branch, and
/// the `linked` PRs (github.com links, asked with github.com's). `None` when
/// `gh` is missing, unauthenticated or offline, or every host refused the
/// request — the caller keeps what it had.
pub fn fetch_prs(task_dir: &Path, repos: &[String], linked: &[tenx_core::prstate::PrRef]) -> Option<(Vec<PrInfo>, Vec<PrInfo>)> {
    use tenx_core::prstate;
    let git = |wt: &Path, args: &[&str]| -> Option<String> {
        let out = Command::new("git").arg("-C").arg(wt).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !s.is_empty()).then_some(s)
    };
    let mut hosts: Vec<(String, Vec<prstate::BranchRef>)> = vec![(GITHUB.to_string(), Vec::new())];
    for r in repos {
        let wt = task_dir.join(r);
        let Some((host, repo)) = git(&wt, &["remote", "get-url", "origin"]).and_then(|u| prstate::remote_repo(&u)) else { continue };
        let Some(branch) = git(&wt, &["branch", "--show-current"]) else { continue };
        let host = match host {
            prstate::Host::Web(h) => h,
            prstate::Host::Ssh(h) => ssh_hostname(&h),
        };
        let b = prstate::BranchRef { dir: r.clone(), repo, branch };
        match hosts.iter_mut().find(|(h, _)| *h == host) {
            Some((_, list)) => list.push(b),
            None => hosts.push((host, vec![b])),
        }
    }
    let (mut own, mut others, mut answered) = (Vec::new(), Vec::new(), false);
    for (host, branches) in &hosts {
        let linked: &[prstate::PrRef] = if host == GITHUB { linked } else { &[] };
        if branches.is_empty() && linked.is_empty() {
            continue;
        }
        let query = format!("query={}", prstate::query(branches, linked));
        let Ok(out) = Command::new("gh").args(["api", "graphql", "--hostname", host, "-f", &query]).stdin(Stdio::null()).stderr(Stdio::null()).output() else {
            continue;
        };
        // A PR that is gone fails its part of the request, and gh exits 1, but
        // the rest of the answer is still there.
        let Some((o, l)) = serde_json::from_slice(&out.stdout).ok().and_then(|json| prstate::parse(&json, branches, linked)) else { continue };
        answered = true;
        own.extend(o);
        others.extend(l);
    }
    if !answered && hosts.iter().any(|(h, b)| !b.is_empty() || (h == GITHUB && !linked.is_empty())) {
        return None;
    }
    others.retain(|o: &PrInfo| !own.iter().any(|p: &PrInfo| p.url == o.url));
    Some((own, others))
}

const GITHUB: &str = "github.com";

/// The host an SSH remote's host name stands for: an alias in
/// `~/.ssh/config` (`github-work`) is resolved with `ssh -G`, as git would.
fn ssh_hostname(host: &str) -> String {
    if host == GITHUB {
        return host.to_string();
    }
    let out = run_capture("ssh", &["-G", host]);
    out.lines().find_map(|l| l.strip_prefix("hostname ")).map(|h| h.trim().to_string()).unwrap_or_else(|| host.to_string())
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
