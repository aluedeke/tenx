//! Live, external facts about a task that neither Claude Code nor tmux
//! know: which TCP ports its processes are listening on, and the state of
//! its pull request(s). The binary gathers the raw material (`lsof`, `ps`,
//! `gh api graphql`, read by `crate::prstate`) and caches the result per
//! task; the parsing, the summaries and when to look again ([`due`]) live
//! here so they can be tested without any of those tools.

use crate::links::Link;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};

/// What's cached per task (`.tenx-live.json`). Everything optional/empty by
/// default, so a task with no cache reads as "nothing known yet".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Live {
    /// Listening TCP ports owned by processes under the task's window.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// One entry per repo that has a PR for the task's branch.
    #[serde(default)]
    pub prs: Vec<PrInfo>,
    /// The other PRs the task links to (`TASK.md`, the agent's replies),
    /// looked up with the task's own (`crate::prstate`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_prs: Vec<PrInfo>,
    /// When the PRs were last looked up (unix seconds); 0 = never.
    #[serde(default)]
    pub pr_checked: u64,
    /// PR numbers a `tenx pr wait` started in this task is waiting on right
    /// now (`prwatch::watched_in`), refreshed every watcher tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watching: Vec<u64>,
    /// The `tenx service`s started in this task (`crate::taskservice`),
    /// refreshed every watcher tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<crate::taskservice::Service>,
    /// Links the agent named in its replies, newest first
    /// (`crate::links::merge`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// Links in `TASK.md`'s `## Links` section.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_links: Vec<Link>,
    /// How far the agent's transcript has been read for `links`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned: Option<Scanned>,
    /// The state of the Linear issues among the links (the sidebar's).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linear: Vec<crate::sidebar::LinearIssue>,
    /// When they were last looked up (unix seconds); 0 = never.
    #[serde(default)]
    pub linear_checked: u64,
    /// Each repo worktree's git state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub git: Vec<crate::sidebar::RepoGit>,
    /// When it was last read (unix seconds); 0 = never.
    #[serde(default)]
    pub git_checked: u64,
    /// `TASK.md`'s todo checklist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<crate::sidebar::Todos>,
}

/// A transcript read up to `offset` bytes: the next read starts there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scanned {
    pub path: String,
    pub offset: u64,
}

/// What a PR chip gains while a wait is running on it.
pub const WATCHING: &str = "👀";

impl Live {
    /// `pr`'s chip, marked when a wait is running on it.
    pub fn chip(&self, pr: &PrInfo) -> String {
        if self.watching.contains(&pr.number) { format!("{} {WATCHING}", pr.chip()) } else { pr.chip() }
    }

    /// Everything the task links to, in display order (`crate::links::all`).
    pub fn all_links(&self) -> Vec<Link> {
        crate::links::all(&self.prs, &self.task_links, &self.links, &self.ports)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrInfo {
    pub repo: String,
    pub number: u64,
    /// `OPEN`, `MERGED`, `CLOSED` as `gh` reports it.
    pub state: String,
    pub url: String,
    pub draft: bool,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or empty.
    pub review: String,
    /// Rolled up from the PR's checks: `success`, `failure`, `pending`, or
    /// empty when there are none.
    pub checks: String,
    #[serde(default)]
    pub title: String,
    /// The checks of its last commit, by outcome.
    #[serde(default)]
    pub checks_pass: u32,
    #[serde(default)]
    pub checks_fail: u32,
    #[serde(default)]
    pub checks_pending: u32,
    /// Comments and review threads.
    #[serde(default)]
    pub comments: u32,
    /// Review threads not resolved yet.
    #[serde(default)]
    pub unresolved: u32,
}

impl PrInfo {
    /// The compact form the column shows: `#12 ✓`, `#12 ✗`, `#12 …`,
    /// `#12 draft`, `#12 merged`, `#12 closed`.
    pub fn chip(&self) -> String {
        let tail = match self.state.as_str() {
            "MERGED" => " merged".to_string(),
            "CLOSED" => " closed".to_string(),
            _ if self.draft => " draft".to_string(),
            _ => match self.checks.as_str() {
                "success" => " ✓".to_string(),
                "failure" => " ✗".to_string(),
                "pending" => " …".to_string(),
                _ => String::new(),
            },
        };
        format!("#{}{tail}", self.number)
    }

    /// Its name in a list of links: `#12 Fix the parser`.
    pub fn label(&self) -> String {
        if self.title.is_empty() { format!("#{}", self.number) } else { format!("#{} {}", self.number, self.title) }
    }
}

/// A task waiting for a lookup (its PRs, its Linear issues, its repos' git
/// state): when it was last looked up, whether it is open or running, and
/// what a lookup costs (repos to read; 1 for one request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due {
    pub checked: u64,
    pub active: bool,
    pub cost: usize,
}

/// Which of `tasks` get a lookup now: those older than their time to live
/// (`ttl.0` active, `ttl.1` not, in seconds), the longest waiting first — a
/// fixed order would let the first tasks take every turn —, within `budget`.
/// A task costing more than the budget goes alone, so it gets its turn too;
/// a task costing nothing has nothing to look up.
pub fn due(tasks: &[Due], now: u64, ttl: (u64, u64), budget: usize) -> Vec<usize> {
    let mut waiting: Vec<usize> = (0..tasks.len())
        .filter(|&i| {
            let t = tasks[i];
            t.cost > 0 && now.saturating_sub(t.checked) >= if t.active { ttl.0 } else { ttl.1 }
        })
        .collect();
    waiting.sort_by_key(|&i| (tasks[i].checked, !tasks[i].active));
    let mut chosen = Vec::new();
    let mut left = budget;
    for i in waiting {
        if tasks[i].cost > left && !chosen.is_empty() {
            break;
        }
        left = left.saturating_sub(tasks[i].cost);
        chosen.push(i);
        if left == 0 {
            break;
        }
    }
    chosen
}

/// `lsof -nP -iTCP -sTCP:LISTEN -Fpn` → (pid, port) pairs. The `-F` format is
/// one field per line: `p<pid>` starts a process, `n<addr>` names a socket
/// (`*:3000`, `127.0.0.1:3000`, `[::1]:3000`).
pub fn parse_lsof(text: &str) -> Vec<(u32, u16)> {
    let mut out = Vec::new();
    let mut pid: Option<u32> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.trim().parse().ok();
        } else if let (Some(pid), Some(addr)) = (pid, line.strip_prefix('n'))
            && let Some(port) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok())
        {
            out.push((pid, port));
        }
    }
    out
}

/// `ps -axo pid=,ppid=` → (pid, ppid) pairs.
pub fn parse_ps(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
        })
        .collect()
}

/// Every pid that is one of `roots` or descends from one.
pub fn descendants(roots: &[u32], tree: &[(u32, u32)]) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, ppid) in tree {
        children.entry(*ppid).or_default().push(*pid);
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = roots.to_vec();
    while let Some(p) = stack.pop() {
        if seen.insert(p)
            && let Some(kids) = children.get(&p)
        {
            stack.extend(kids);
        }
    }
    seen
}

/// Listening ports owned by `roots` (a window's pane pids) or anything they
/// spawned, sorted and deduplicated.
pub fn ports_for(roots: &[u32], listeners: &[(u32, u16)], tree: &[(u32, u32)]) -> Vec<u16> {
    let mine = descendants(roots, tree);
    listeners.iter().filter(|(pid, _)| mine.contains(pid)).map(|(_, port)| *port).collect::<BTreeSet<_>>().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsof_fields_map_to_pid_port() {
        let text = "p1077\nf10\nn*:51816\nf14\nn*:51816\np1182\nf9\nn127.0.0.1:7000\nf11\nn[::1]:5000\n";
        assert_eq!(parse_lsof(text), vec![(1077, 51816), (1077, 51816), (1182, 7000), (1182, 5000)]);
    }

    #[test]
    fn ports_follow_the_process_tree() {
        let tree = vec![(100, 1), (200, 100), (300, 200), (400, 1)];
        let listeners = vec![(300, 3000), (400, 4000), (100, 8080)];
        assert_eq!(ports_for(&[100], &listeners, &tree), vec![3000, 8080]);
        assert!(ports_for(&[999], &listeners, &tree).is_empty());
        assert_eq!(parse_ps("  1     0\n  363     1\n"), vec![(1, 0), (363, 1)]);
    }

    #[test]
    fn watched_prs_are_marked() {
        let pr = PrInfo { number: 12, state: "OPEN".into(), url: "u".into(), ..Default::default() };
        let mut l = Live { prs: vec![pr.clone()], watching: vec![12, 30], ..Default::default() };
        assert_eq!(l.chip(&pr), "#12 👀");
        l.watching.clear();
        assert_eq!(l.chip(&pr), "#12");
    }

    #[test]
    fn pr_chips() {
        let pr = |state: &str, draft: bool, checks: &str| PrInfo { number: 12, state: state.into(), draft, checks: checks.into(), ..Default::default() };
        assert_eq!(pr("OPEN", false, "pending").chip(), "#12 …");
        assert_eq!(pr("OPEN", false, "failure").chip(), "#12 ✗");
        assert_eq!(pr("OPEN", false, "success").chip(), "#12 ✓");
        assert_eq!(pr("OPEN", true, "success").chip(), "#12 draft");
        assert_eq!(pr("MERGED", false, "").chip(), "#12 merged");
        assert_eq!(pr("CLOSED", false, "").chip(), "#12 closed");
    }

    #[test]
    fn the_longest_waiting_lookups_go_first_within_the_budget() {
        let t = |checked: u64, active: bool, cost: usize| Due { checked, active, cost };
        let ttl = (30, 600);
        // Fresh (695), parked but young (500), nothing to look up (cost 0) wait.
        let tasks = [t(695, true, 1), t(650, true, 2), t(10, false, 1), t(500, false, 1), t(0, true, 0), t(10, true, 1)];
        assert_eq!(due(&tasks, 700, ttl, 4), vec![5, 2, 1], "oldest first, active before parked at a tie");
        assert_eq!(due(&tasks, 700, ttl, 2), vec![5, 2]);
        // A task bigger than the budget goes alone when it is first in line.
        assert_eq!(due(&[t(0, true, 6), t(5, true, 1)], 100, ttl, 4), vec![0]);
        assert_eq!(due(&[t(5, true, 1), t(0, true, 6)][..], 100, ttl, 4), vec![1]);
        assert_eq!(due(&[t(9, true, 1), t(10, true, 6)], 100, ttl, 4), vec![0]);
    }

    #[test]
    fn live_roundtrips_and_defaults() {
        let l: Live = serde_json::from_str("{}").unwrap();
        assert_eq!(l, Live::default());
        let l = Live {
            ports: vec![3000],
            pr_checked: 5,
            links: vec![Link { url: "https://x".into(), label: "x".into(), kind: crate::links::LinkKind::Web }],
            scanned: Some(Scanned { path: "/t.jsonl".into(), offset: 9 }),
            ..Default::default()
        };
        let back: Live = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
    }
}
