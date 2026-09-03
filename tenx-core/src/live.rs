//! Live, external facts about a task that neither Claude Code nor tmux
//! know: which TCP ports its processes are listening on, and the state of
//! its pull request(s). The binary gathers the raw material (`lsof`, `ps`,
//! `gh pr view`) and caches the result per task; the parsing and the
//! summaries live here so they can be tested without any of those tools.

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
    /// When the PRs were last looked up (unix seconds); 0 = never.
    #[serde(default)]
    pub pr_checked: u64,
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
}

impl PrInfo {
    /// The compact form the overlay shows: `#12 ✓`, `#12 ✗`, `#12 …`,
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
}

/// `gh pr view --json number,state,url,isDraft,reviewDecision,statusCheckRollup`
/// → a summary. `None` if the document isn't a PR (e.g. `gh` printed an error
/// object, or nothing).
pub fn summarize_pr(repo: &str, json: &serde_json::Value) -> Option<PrInfo> {
    let number = json["number"].as_u64()?;
    let checks = json["statusCheckRollup"].as_array().map(|checks| {
        let mut pending = false;
        let mut failure = false;
        for c in checks {
            // Check runs carry `status`/`conclusion`; status contexts carry
            // `state`. Normalise both.
            let status = c["status"].as_str().unwrap_or("COMPLETED");
            let conclusion = c["conclusion"].as_str().or_else(|| c["state"].as_str()).unwrap_or("");
            if status != "COMPLETED" || conclusion == "PENDING" || conclusion == "EXPECTED" {
                pending = true;
            } else if matches!(conclusion, "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED") {
                failure = true;
            }
        }
        if checks.is_empty() {
            ""
        } else if failure {
            "failure"
        } else if pending {
            "pending"
        } else {
            "success"
        }
    });
    Some(PrInfo {
        repo: repo.to_string(),
        number,
        state: json["state"].as_str().unwrap_or("").to_string(),
        url: json["url"].as_str().unwrap_or("").to_string(),
        draft: json["isDraft"].as_bool().unwrap_or(false),
        review: json["reviewDecision"].as_str().unwrap_or("").to_string(),
        checks: checks.unwrap_or("").to_string(),
    })
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
    fn pr_summary_and_chip() {
        let json = serde_json::json!({
            "number": 12, "state": "OPEN", "url": "https://x/pull/12", "isDraft": false,
            "reviewDecision": "APPROVED",
            "statusCheckRollup": [
                {"status": "COMPLETED", "conclusion": "SUCCESS"},
                {"status": "IN_PROGRESS", "conclusion": null}
            ]
        });
        let pr = summarize_pr("api", &json).unwrap();
        assert_eq!(pr.checks, "pending");
        assert_eq!(pr.chip(), "#12 …");

        let json = serde_json::json!({"number": 7, "state": "MERGED", "statusCheckRollup": []});
        assert_eq!(summarize_pr("api", &json).unwrap().chip(), "#7 merged");
        let json = serde_json::json!({"number": 8, "state": "OPEN", "isDraft": true});
        assert_eq!(summarize_pr("api", &json).unwrap().chip(), "#8 draft");
        let json = serde_json::json!({"number": 9, "state": "OPEN",
            "statusCheckRollup": [{"state": "FAILURE"}]});
        assert_eq!(summarize_pr("api", &json).unwrap().chip(), "#9 ✗");
        assert!(summarize_pr("api", &serde_json::json!({"message": "no pull requests found"})).is_none());
    }

    #[test]
    fn live_roundtrips_and_defaults() {
        let l: Live = serde_json::from_str("{}").unwrap();
        assert_eq!(l, Live::default());
        let l = Live { ports: vec![3000], prs: vec![], pr_checked: 5 };
        let back: Live = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
    }
}
