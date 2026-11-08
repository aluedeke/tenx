//! The sidebar: what a task is about, beside its screen — its links grouped
//! (PRs with their state, Linear issues with their status, what was made
//! with Claude, Slack messages, other pages and ports), its repos' git state
//! and its `TASK.md` todos. [`build`] turns a task's cached facts
//! ([`crate::live::Live`]) into [`Sidebar`], which the terminal client draws
//! right of the task and `tenx web` sends to its page.
//!
//! The facts are gathered by the watcher (`tenx watch`), never by a front
//! end: the Linear lookup ([`linear_request`], [`parse_linear`]) is a network
//! call, the git one ([`parse_status`], [`parse_counts`]) a few processes per
//! repo. A front end only reads the cache, so drawing never waits.

use crate::links::{Link, LinkKind};
use crate::live::{Live, PrInfo};
use serde::{Deserialize, Serialize};

/// A Linear issue's state, as the Linear lookup found it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearIssue {
    /// `TNX-142`.
    pub key: String,
    pub title: String,
    /// The workflow state's name: `In Progress`, `Done`, …
    pub state: String,
    /// Linear's kind of state: `triage`, `backlog`, `unstarted`, `started`,
    /// `completed`, `canceled`.
    pub state_type: String,
}

/// A repo worktree's git state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoGit {
    pub name: String,
    pub branch: String,
    /// Files changed, staged or not, untracked included.
    pub changed: u32,
    /// Files with merge conflicts.
    pub conflicts: u32,
    /// The branch has a remote branch to push to.
    pub upstream: bool,
    /// Commits not pushed to it yet.
    pub unpushed: u32,
    /// Commits the branch has on top of the default branch (`origin/main`).
    pub ahead: u32,
    /// Commits the default branch got since the branch left it.
    pub behind: u32,
}

/// `TASK.md`'s `## Todo` checklist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todos {
    pub done: usize,
    pub total: usize,
    /// The first open items, in order (at most [`OPEN_TODOS`]).
    pub open: Vec<String>,
}

/// How many open todos the sidebar lists.
pub const OPEN_TODOS: usize = 3;

/// The sidebar's width in the terminal client, when there is room for it.
pub const WIDTH: u16 = 36;

/// The task screen keeps at least this many columns beside a sidebar.
pub const MIN_TASK_COLS: u16 = 80;

/// The sidebar's width for a `cols`-wide terminal whose task list takes
/// `column`: [`WIDTH`], or 0 when the task screen would get too narrow.
pub fn width(cols: u16, column: u16) -> u16 {
    if cols.saturating_sub(column).saturating_sub(WIDTH) >= MIN_TASK_COLS { WIDTH } else { 0 }
}

/// The checklist under `TASK.md`'s `## Todo` heading.
pub fn todos(md: &str) -> Option<Todos> {
    let mut inside = false;
    let mut t = Todos::default();
    for line in md.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            inside = heading.trim().eq_ignore_ascii_case("todo");
            continue;
        }
        if !inside {
            continue;
        }
        let item = line.trim_start().strip_prefix("- [").or_else(|| line.trim_start().strip_prefix("* ["));
        let Some(rest) = item else { continue };
        let (mark, text) = match rest.split_once(']') {
            Some(p) => p,
            None => continue,
        };
        t.total += 1;
        if mark.trim().eq_ignore_ascii_case("x") {
            t.done += 1;
        } else if t.open.len() < OPEN_TODOS {
            t.open.push(text.trim().to_string());
        }
    }
    (t.total > 0).then_some(t)
}

/// `git status --porcelain=v2 --branch` → the repo's branch, changed files,
/// conflicts and what's not pushed. `ahead`/`behind` against the default
/// branch come from [`parse_counts`].
pub fn parse_status(name: &str, porcelain: &str) -> RepoGit {
    let mut g = RepoGit { name: name.to_string(), ..Default::default() };
    for line in porcelain.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            g.branch = head.trim().to_string();
        } else if line.starts_with("# branch.upstream ") {
            g.upstream = true;
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            g.unpushed = ab.split_whitespace().find_map(|w| w.strip_prefix('+')).and_then(|n| n.parse().ok()).unwrap_or(0);
        } else if line.starts_with("u ") {
            g.conflicts += 1;
            g.changed += 1;
        } else if line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("? ") {
            g.changed += 1;
        }
    }
    g
}

/// `git rev-list --left-right --count <base>...HEAD` → (behind, ahead).
pub fn parse_counts(out: &str) -> Option<(u32, u32)> {
    let mut it = out.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// A Linear issue link's key: `…/issue/TNX-142/slug` → `TNX-142`.
pub fn linear_key(url: &str) -> Option<String> {
    let mut seg = url.split(['/', '?', '#']).skip_while(|s| *s != "issue").skip(1);
    let key = seg.next()?;
    let (team, number) = key.rsplit_once('-')?;
    let ok = !team.is_empty() && team.chars().all(|c| c.is_ascii_alphanumeric()) && number.parse::<u64>().is_ok();
    ok.then(|| key.to_ascii_uppercase())
}

/// The GraphQL request body that looks up `keys` in one call: an aliased
/// `issue(id:)` per key (Linear takes an identifier there).
pub fn linear_request(keys: &[String]) -> String {
    let fields: Vec<String> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| format!("i{i}: issue(id: {}) {{ identifier title state {{ name type }} }}", serde_json::Value::String(k.clone())))
        .collect();
    serde_json::json!({ "query": format!("{{ {} }}", fields.join(" ")) }).to_string()
}

/// The answer to [`linear_request`]. `Err` with Linear's message when the
/// whole request failed (a wrong key); an issue it doesn't know is left out.
pub fn parse_linear(json: &serde_json::Value, keys: &[String]) -> Result<Vec<LinearIssue>, String> {
    let data = json.get("data").filter(|d| d.is_object());
    if data.is_none() {
        let msg = json["errors"][0]["message"].as_str().unwrap_or("no answer from Linear");
        return Err(msg.to_string());
    }
    let data = data.unwrap_or(&serde_json::Value::Null);
    Ok((0..keys.len())
        .filter_map(|i| {
            let issue = data.get(format!("i{i}")).filter(|v| v.is_object())?;
            Some(LinearIssue {
                key: issue["identifier"].as_str()?.to_string(),
                title: issue["title"].as_str().unwrap_or("").to_string(),
                state: issue["state"]["name"].as_str().unwrap_or("").to_string(),
                state_type: issue["state"]["type"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect())
}

/// What the sidebar shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Sidebar {
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    pub title: &'static str,
    /// Beside the title: the todos' `3/5`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub count: String,
    pub entries: Vec<Entry>,
}

/// One line of a section, and the state under it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub icon: &'static str,
    pub label: String,
    /// The state: `checks pass · approved`, `In Progress`, `3 changed`; may be
    /// empty.
    pub detail: String,
    pub tone: Tone,
    /// What a click opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<LinkKind>,
}

/// The colour of an entry's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Plain,
    /// Ready, passing, approved, done.
    Good,
    /// Wants a look: running checks, a review to get, work not pushed.
    Warn,
    /// Broken: failing checks, changes requested, conflicts.
    Bad,
    /// Merged.
    Merged,
    /// Over with: closed, canceled.
    Muted,
}

/// A PR's state line and its colour.
pub fn pr_state(pr: &PrInfo, watching: bool) -> (String, Tone) {
    let (mut parts, mut tone): (Vec<&str>, Tone) = match pr.state.as_str() {
        "MERGED" => (vec!["merged"], Tone::Merged),
        "CLOSED" => (vec!["closed"], Tone::Muted),
        _ => {
            let mut parts = Vec::new();
            let mut tone = Tone::Plain;
            if pr.draft {
                parts.push("draft");
            }
            match pr.checks.as_str() {
                "failure" => {
                    parts.push("checks failing");
                    tone = Tone::Bad;
                }
                "pending" => {
                    parts.push("checks running");
                    tone = Tone::Warn;
                }
                "success" => {
                    parts.push("checks pass");
                    tone = Tone::Good;
                }
                _ => {}
            }
            match pr.review.as_str() {
                "CHANGES_REQUESTED" => {
                    parts.push("changes requested");
                    tone = Tone::Bad;
                }
                "REVIEW_REQUIRED" if !pr.draft => {
                    parts.push("review needed");
                    if tone != Tone::Bad {
                        tone = Tone::Warn;
                    }
                }
                "APPROVED" => parts.push("approved"),
                _ => {}
            }
            if parts.is_empty() {
                parts.push("open");
            }
            (parts, tone)
        }
    };
    if watching {
        parts.push("watching");
        if tone == Tone::Plain {
            tone = Tone::Warn;
        }
    }
    (parts.join(" · "), tone)
}

/// A Linear state's colour, by its kind.
pub fn linear_tone(state_type: &str) -> Tone {
    match state_type {
        "completed" => Tone::Good,
        "started" => Tone::Warn,
        "canceled" => Tone::Muted,
        _ => Tone::Plain,
    }
}

/// A repo's state line and its colour: commits on the branch, changes,
/// conflicts, what's not pushed, and whether the default branch moved on.
pub fn repo_state(g: &RepoGit) -> (String, Tone) {
    let mut parts = Vec::new();
    let mut tone = Tone::Good;
    if g.conflicts > 0 {
        parts.push(format!("{} conflicts", g.conflicts));
        tone = Tone::Bad;
    }
    if g.changed > g.conflicts {
        parts.push(format!("{} changed", g.changed - g.conflicts));
        if tone == Tone::Good {
            tone = Tone::Warn;
        }
    }
    if g.upstream && g.unpushed > 0 {
        parts.push(format!("{} to push", g.unpushed));
        if tone == Tone::Good {
            tone = Tone::Warn;
        }
    } else if !g.upstream && g.ahead > 0 {
        parts.push("not pushed".to_string());
        if tone == Tone::Good {
            tone = Tone::Warn;
        }
    }
    if g.ahead > 0 {
        parts.insert(0, format!("{} {}", g.ahead, if g.ahead == 1 { "commit" } else { "commits" }));
    }
    if g.behind > 0 {
        parts.push(format!("main +{}", g.behind));
        if tone == Tone::Good {
            tone = Tone::Warn;
        }
    }
    if parts.is_empty() {
        parts.push("clean".to_string());
    }
    (parts.join(" · "), tone)
}

fn link_entry(l: &Link, detail: String, tone: Tone) -> Entry {
    Entry { icon: l.kind.icon(), label: l.label.clone(), detail, tone, url: Some(l.url.clone()), kind: Some(l.kind) }
}

/// The sidebar for a task whose cached facts are `live`.
pub fn build(live: &Live) -> Sidebar {
    let links = live.all_links();
    let of = |kinds: &[LinkKind]| links.iter().filter(|l| kinds.contains(&l.kind)).collect::<Vec<_>>();
    let mut sections = Vec::new();
    let mut push = |title: &'static str, count: String, entries: Vec<Entry>| {
        if !entries.is_empty() {
            sections.push(Section { title, count, entries });
        }
    };

    let prs = of(&[LinkKind::Pr])
        .into_iter()
        .map(|l| match live.prs.iter().find(|p| p.url == l.url) {
            Some(p) => {
                let (detail, tone) = pr_state(p, live.watching.contains(&p.number));
                link_entry(l, detail, tone)
            }
            None => link_entry(l, String::new(), Tone::Plain),
        })
        .collect();
    push("Pull requests", String::new(), prs);

    let linear = of(&[LinkKind::Linear])
        .into_iter()
        .map(|l| {
            let found = linear_key(&l.url).and_then(|k| live.linear.iter().find(|i| i.key == k));
            match found {
                Some(i) => {
                    let mut e = link_entry(l, i.state.clone(), linear_tone(&i.state_type));
                    if !i.title.is_empty() {
                        e.label = format!("{} {}", i.key, i.title);
                    }
                    e
                }
                None => link_entry(l, String::new(), Tone::Plain),
            }
        })
        .collect();
    push("Linear", String::new(), linear);

    let claude = of(&[LinkKind::Design, LinkKind::Artifact])
        .into_iter()
        .map(|l| link_entry(l, if l.kind == LinkKind::Design { "Claude Design" } else { "artifact" }.to_string(), Tone::Muted))
        .collect();
    push("Claude", String::new(), claude);

    let slack = of(&[LinkKind::Slack]).into_iter().map(|l| link_entry(l, String::new(), Tone::Plain)).collect();
    push("Slack", String::new(), slack);

    let other = of(&[LinkKind::Port, LinkKind::Web])
        .into_iter()
        .map(|l| match l.kind {
            LinkKind::Port => link_entry(l, "running here".to_string(), Tone::Good),
            _ => link_entry(l, String::new(), Tone::Plain),
        })
        .collect();
    push("Other", String::new(), other);

    let repos = live
        .git
        .iter()
        .map(|g| {
            let (detail, tone) = repo_state(g);
            Entry { icon: "⎇", label: format!("{} · {}", g.name, g.branch), detail, tone, url: None, kind: None }
        })
        .collect();
    push("Repos", String::new(), repos);

    if let Some(t) = &live.todos {
        let entries = if t.open.is_empty() {
            vec![Entry { icon: "✓", label: "all done".to_string(), detail: String::new(), tone: Tone::Good, url: None, kind: None }]
        } else {
            t.open
                .iter()
                .map(|s| Entry { icon: "○", label: s.clone(), detail: String::new(), tone: Tone::Plain, url: None, kind: None })
                .collect()
        };
        push("Todo", format!("{}/{}", t.done, t.total), entries);
    }
    Sidebar { sections }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidebar_fits_only_beside_a_wide_enough_task() {
        assert_eq!(width(200, 40), WIDTH);
        assert_eq!(width(40 + WIDTH + MIN_TASK_COLS, 40), WIDTH);
        assert_eq!(width(40 + WIDTH + MIN_TASK_COLS - 1, 40), 0);
        assert_eq!(width(30, 40), 0);
    }

    #[test]
    fn todos_count_the_checklist_and_list_what_is_open() {
        let md = "# T\n\n## Todo\n\n- [x] one\n- [ ] two\n  - [X] nested done\n- [ ] three\n- [ ] four\n- [ ] five\n\n## Notes\n\n- [ ] not a todo\n";
        let t = todos(md).unwrap();
        assert_eq!((t.done, t.total), (2, 6));
        assert_eq!(t.open, vec!["two", "three", "four"]);
        assert_eq!(todos("# T\n\n## Notes\n- [ ] x\n"), None);
    }

    #[test]
    fn git_status_is_read_from_porcelain_v2() {
        let out = "# branch.oid abc\n# branch.head task-sidebar\n# branch.upstream origin/task-sidebar\n# branch.ab +2 -0\n1 .M N... 100644 100644 100644 a b src/x.rs\n? new.txt\nu UU N... 1 2 3 4 a b c conflict.rs\n";
        let g = parse_status("tenx", out);
        assert_eq!(g.branch, "task-sidebar");
        assert!(g.upstream);
        assert_eq!((g.unpushed, g.changed, g.conflicts), (2, 3, 1));
        assert_eq!(parse_counts("4\t7\n"), Some((4, 7)));
        assert_eq!(parse_counts("fatal"), None);
    }

    #[test]
    fn repo_state_names_what_needs_doing() {
        let clean = RepoGit { name: "r".into(), branch: "b".into(), ..Default::default() };
        assert_eq!(repo_state(&clean), ("clean".into(), Tone::Good));
        let local = RepoGit { ahead: 3, changed: 2, ..clean.clone() };
        assert_eq!(repo_state(&local), ("3 commits · 2 changed · not pushed".into(), Tone::Warn));
        let pushed = RepoGit { ahead: 1, upstream: true, unpushed: 1, behind: 5, ..clean.clone() };
        assert_eq!(repo_state(&pushed), ("1 commit · 1 to push · main +5".into(), Tone::Warn));
        let broken = RepoGit { changed: 2, conflicts: 1, ..clean };
        assert_eq!(repo_state(&broken), ("1 conflicts · 1 changed".into(), Tone::Bad));
    }

    #[test]
    fn linear_keys_requests_and_answers() {
        assert_eq!(linear_key("https://linear.app/acme/issue/TNX-142/quick-links"), Some("TNX-142".into()));
        assert_eq!(linear_key("https://linear.app/acme/issue/tnx-7"), Some("TNX-7".into()));
        assert_eq!(linear_key("https://linear.app/acme/issue/nope"), None);
        let keys = vec!["TNX-1".to_string(), "TNX-2".to_string()];
        let body: serde_json::Value = serde_json::from_str(&linear_request(&keys)).unwrap();
        let q = body["query"].as_str().unwrap();
        assert!(q.contains(r#"i0: issue(id: "TNX-1")"#) && q.contains(r#"i1: issue(id: "TNX-2")"#), "{q}");
        let answer = serde_json::json!({"data": {"i0": {"identifier": "TNX-1", "title": "Fix it", "state": {"name": "In Progress", "type": "started"}}, "i1": null}});
        let found = parse_linear(&answer, &keys).unwrap();
        assert_eq!(found, vec![LinearIssue { key: "TNX-1".into(), title: "Fix it".into(), state: "In Progress".into(), state_type: "started".into() }]);
        let denied = serde_json::json!({"errors": [{"message": "Authentication required"}]});
        assert_eq!(parse_linear(&denied, &keys), Err("Authentication required".into()));
    }

    #[test]
    fn pr_states() {
        let pr = |state: &str, draft: bool, checks: &str, review: &str| PrInfo {
            state: state.into(),
            draft,
            checks: checks.into(),
            review: review.into(),
            ..Default::default()
        };
        assert_eq!(pr_state(&pr("MERGED", false, "success", "APPROVED"), false), ("merged".into(), Tone::Merged));
        assert_eq!(pr_state(&pr("CLOSED", false, "", ""), false), ("closed".into(), Tone::Muted));
        assert_eq!(pr_state(&pr("OPEN", false, "success", "APPROVED"), false), ("checks pass · approved".into(), Tone::Good));
        assert_eq!(pr_state(&pr("OPEN", false, "failure", "REVIEW_REQUIRED"), false), ("checks failing · review needed".into(), Tone::Bad));
        assert_eq!(pr_state(&pr("OPEN", true, "pending", "REVIEW_REQUIRED"), false), ("draft · checks running".into(), Tone::Warn));
        assert_eq!(pr_state(&pr("OPEN", false, "", ""), true), ("open · watching".into(), Tone::Warn));
    }

    #[test]
    fn the_sidebar_groups_links_and_adds_state() {
        let link = |url: &str, label: &str| Link { url: url.into(), label: label.into(), kind: crate::links::classify(url) };
        let live = Live {
            prs: vec![PrInfo {
                repo: "tenx".into(),
                number: 32,
                state: "OPEN".into(),
                url: "https://github.com/a/tenx/pull/32".into(),
                checks: "success".into(),
                title: "Bar".into(),
                ..Default::default()
            }],
            task_links: vec![
                link("https://linear.app/acme/issue/TNX-9/x", "TNX-9 x"),
                link("https://acme.slack.com/archives/C1/p2", "the thread"),
                link("https://claude.ai/design/p/1", "Mock"),
                link("https://docs.rs/x", "docs"),
            ],
            linear: vec![LinearIssue { key: "TNX-9".into(), title: "Sidebar".into(), state: "In Review".into(), state_type: "started".into() }],
            ports: vec![3000],
            git: vec![RepoGit { name: "tenx".into(), branch: "task-sidebar".into(), changed: 1, ..Default::default() }],
            todos: Some(Todos { done: 1, total: 2, open: vec!["ship".into()] }),
            ..Default::default()
        };
        let s = build(&live);
        let titles: Vec<&str> = s.sections.iter().map(|s| s.title).collect();
        assert_eq!(titles, vec!["Pull requests", "Linear", "Claude", "Slack", "Other", "Repos", "Todo"]);
        assert_eq!(s.sections[0].entries[0].label, "#32 Bar");
        assert_eq!(s.sections[0].entries[0].detail, "checks pass");
        assert_eq!(s.sections[1].entries[0].label, "TNX-9 Sidebar");
        assert_eq!((s.sections[1].entries[0].detail.as_str(), s.sections[1].entries[0].tone), ("In Review", Tone::Warn));
        assert_eq!(s.sections[4].entries.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), vec!["localhost:3000", "docs"]);
        assert_eq!(s.sections[5].entries[0].detail, "1 changed");
        assert_eq!(s.sections[6].count, "1/2");
        assert!(build(&Live::default()).sections.is_empty());
    }
}
