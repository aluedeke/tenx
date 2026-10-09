//! `tenx pr wait`: what counts as news on a pull request. The binary polls
//! `gh` for the PR (`gh pr view --json …`) and its inline review comments
//! (`gh api …/pulls/<n>/comments`); this module turns the two documents into
//! the events an agent should react to, newer than a cursor.
//!
//! News is any review or comment, from a person or a bot, and any check that
//! finished failing. Comments the agent wrote itself carry [`AGENT_MARKER`]
//! and are skipped, or every reply it posts would wake it again. The cursor
//! is the newest GitHub timestamp seen, so it never depends on the local
//! clock. GitHub writes every timestamp as `YYYY-MM-DDTHH:MM:SSZ`, which
//! orders correctly as a string.

use serde_json::Value;

/// What an agent puts in every comment it posts on a watched PR, so the
/// watch doesn't report the agent's own replies as feedback. An HTML
/// comment: invisible on GitHub.
pub const AGENT_MARKER: &str = "<!-- tenx:agent -->";

/// How much of a comment body is printed; the URL has the rest.
const BODY_LIMIT: usize = 1500;

/// Check conclusions that mean "this failed and someone should look". A
/// `CANCELLED` run is left out: it is almost always one a newer push replaced.
const FAILED: &[&str] = &["FAILURE", "TIMED_OUT", "STARTUP_FAILURE", "ACTION_REQUIRED", "ERROR"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// A submitted review; the state is `APPROVED`, `CHANGES_REQUESTED` or
    /// `COMMENTED`.
    Review(String),
    /// A comment on the PR's conversation.
    Comment,
    /// A review comment on a line: `path:line`.
    LineComment(String),
    /// A check that finished failing: its name and conclusion.
    CheckFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// GitHub's timestamp for it.
    pub at: String,
    pub kind: EventKind,
    /// Login of who wrote it; empty for a check.
    pub author: String,
    pub body: String,
    pub url: String,
}

/// One poll's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poll {
    pub number: u64,
    pub url: String,
    pub state: PrState,
    /// News since the cursor, oldest first.
    pub events: Vec<Event>,
    /// Checks not finished yet.
    pub running_checks: usize,
    /// Where the next poll starts: the newest timestamp seen, or the old
    /// cursor when nothing newer was there.
    pub cursor: Option<String>,
}

/// Whether the wait is over, and how. `None` = keep polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// There is news; the PR is still open.
    News,
    Merged,
    Closed,
    TimedOut,
}

impl Outcome {
    /// The exit code `tenx pr wait` reports it with. 1 stays for ordinary
    /// errors, 3 is a timeout as in `task wait`.
    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::News => 0,
            Outcome::TimedOut => 3,
            Outcome::Merged => 10,
            Outcome::Closed => 11,
        }
    }
}

/// How the wait ends after `poll`; `None` = poll again. A merge or close
/// ends it even with news, so the loop around it stops.
pub fn outcome(poll: &Poll) -> Option<Outcome> {
    match poll.state {
        PrState::Merged => Some(Outcome::Merged),
        PrState::Closed => Some(Outcome::Closed),
        PrState::Open if !poll.events.is_empty() => Some(Outcome::News),
        PrState::Open => None,
    }
}

/// `https://github.com/<owner>/<repo>/pull/<n>` → (`owner/repo`, n).
pub fn parse_pr_url(url: &str) -> Option<(String, u64)> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    match parts.as_slice() {
        [_host, owner, repo, "pull", n, ..] => Some((format!("{owner}/{repo}"), n.parse().ok()?)),
        _ => None,
    }
}

/// Whether `s` looks like a GitHub timestamp, the only form a cursor takes.
pub fn valid_cursor(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20 && b[4] == b'-' && b[10] == b'T' && b[19] == b'Z'
}

/// Turn `gh pr view --json number,url,state,reviews,comments,statusCheckRollup`
/// (`view`) and the inline comments (`lines`: an array of the REST API's
/// review comments) into a [`Poll`]. Everything at or before `since` is old;
/// with no `since` every piece of feedback already there is news.
/// `None` if `view` isn't a PR.
pub fn poll(view: &Value, lines: &[Value], since: Option<&str>) -> Option<Poll> {
    let number = view.get("number")?.as_u64()?;
    let state = match str_at(view, "state") {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    };
    let mut seen: Vec<String> = Vec::new();
    let mut events = Vec::new();

    for r in array(view, "reviews") {
        let state = str_at(r, "state");
        let body = str_at(r, "body");
        let at = str_at(r, "submittedAt");
        // A draft review isn't sent yet; a dismissed one no longer counts;
        // an empty "commented" review only wraps line comments, reported
        // on their own below.
        if state == "PENDING" || state == "DISMISSED" || (state == "COMMENTED" && body.trim().is_empty()) {
            continue;
        }
        seen.push(at.to_string());
        if body.contains(AGENT_MARKER) {
            continue;
        }
        events.push(Event {
            at: at.to_string(),
            kind: EventKind::Review(state.to_string()),
            author: login(r.get("author")),
            body: clip(body),
            url: str_at(view, "url").to_string(),
        });
    }

    for c in array(view, "comments") {
        let at = str_at(c, "createdAt");
        seen.push(at.to_string());
        let body = str_at(c, "body");
        if body.contains(AGENT_MARKER) || c.get("isMinimized").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        events.push(Event {
            at: at.to_string(),
            kind: EventKind::Comment,
            author: login(c.get("author")),
            body: clip(body),
            url: str_at(c, "url").to_string(),
        });
    }

    for c in lines {
        let at = str_at(c, "created_at");
        seen.push(at.to_string());
        let body = str_at(c, "body");
        if body.contains(AGENT_MARKER) {
            continue;
        }
        let line = c.get("line").or_else(|| c.get("original_line")).and_then(Value::as_u64);
        let place = match line {
            Some(n) => format!("{}:{n}", str_at(c, "path")),
            None => str_at(c, "path").to_string(),
        };
        events.push(Event {
            at: at.to_string(),
            kind: EventKind::LineComment(place),
            author: login(c.get("user")),
            body: clip(body),
            url: str_at(c, "html_url").to_string(),
        });
    }

    let mut running_checks = 0;
    for c in array(view, "statusCheckRollup") {
        // A check run (GitHub Actions and apps) or a commit status (older
        // integrations): different field names for the same idea.
        let (name, result, at, url) = if c.get("__typename").and_then(Value::as_str) == Some("StatusContext") {
            let state = str_at(c, "state");
            let done = state != "PENDING" && state != "EXPECTED";
            (str_at(c, "context"), if done { state } else { "" }, str_at(c, "startedAt"), str_at(c, "targetUrl"))
        } else {
            let done = str_at(c, "status") == "COMPLETED";
            (str_at(c, "name"), if done { str_at(c, "conclusion") } else { "" }, str_at(c, "completedAt"), str_at(c, "detailsUrl"))
        };
        if result.is_empty() {
            running_checks += 1;
            continue;
        }
        seen.push(at.to_string());
        if FAILED.contains(&result) {
            events.push(Event {
                at: at.to_string(),
                kind: EventKind::CheckFailed(format!("{name} ({})", result.to_lowercase())),
                author: String::new(),
                body: String::new(),
                url: url.to_string(),
            });
        }
    }

    let newer = |at: &str| valid_cursor(at) && since.is_none_or(|s| at > s);
    events.retain(|e| newer(&e.at));
    events.sort_by(|a, b| a.at.cmp(&b.at));
    let newest = seen.into_iter().filter(|at| newer(at)).max();
    Some(Poll {
        number,
        url: str_at(view, "url").to_string(),
        state,
        events,
        running_checks,
        cursor: newest.or(since.map(str::to_string)),
    })
}

/// The poll as text for the agent: one block per event, then where the PR
/// stands and the command that continues the wait.
pub fn render(poll: &Poll) -> String {
    let mut out = String::new();
    for e in &poll.events {
        let head = match &e.kind {
            EventKind::Review(state) => format!("review {} by {}", state.to_lowercase().replace('_', " "), e.author),
            EventKind::Comment => format!("comment by {}", e.author),
            EventKind::LineComment(place) => format!("comment on {place} by {}", e.author),
            EventKind::CheckFailed(name) => format!("check failed: {name}"),
        };
        out.push_str(&format!("── {head} · {}\n", e.at));
        if !e.body.trim().is_empty() {
            out.push_str(e.body.trim());
            out.push('\n');
        }
        if !e.url.is_empty() {
            out.push_str(&format!("{}\n", e.url));
        }
        out.push('\n');
    }
    let state = match poll.state {
        PrState::Open => "open",
        PrState::Merged => "merged",
        PrState::Closed => "closed without merging",
    };
    out.push_str(&format!("PR #{} is {state}", poll.number));
    if poll.state == PrState::Open && poll.running_checks > 0 {
        out.push_str(&format!(", {} check(s) still running", poll.running_checks));
    }
    out.push('\n');
    if poll.state == PrState::Open {
        out.push_str(&format!("next: tenx pr wait {}{}\n", poll.url, since_arg(poll.cursor.as_deref())));
    }
    out
}

fn since_arg(cursor: Option<&str>) -> String {
    cursor.map(|c| format!(" --since {c}")).unwrap_or_default()
}

fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn login(author: Option<&Value>) -> String {
    author.and_then(|a| a.get("login")).and_then(Value::as_str).unwrap_or("someone").to_string()
}

/// The body as worth reading: HTML comments (bots hide state in them, and
/// they don't show on GitHub) dropped, then cut at [`BODY_LIMIT`].
fn clip(body: &str) -> String {
    let mut shown = String::new();
    let mut rest = body;
    while let Some(i) = rest.find("<!--") {
        shown.push_str(&rest[..i]);
        rest = rest[i..].find("-->").map_or("", |j| &rest[i + j + 3..]);
    }
    shown.push_str(rest);
    let body = shown.trim();
    match body.char_indices().nth(BODY_LIMIT) {
        Some((i, _)) => format!("{}…", &body[..i]),
        None => body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const URL: &str = "https://github.com/acme/api/pull/12";

    fn view() -> Value {
        json!({
            "number": 12, "url": URL, "state": "OPEN",
            "reviews": [
                {"author": {"login": "ann"}, "state": "CHANGES_REQUESTED", "body": "Please rename it.", "submittedAt": "2026-10-09T10:00:00Z"},
                {"author": {"login": "ann"}, "state": "COMMENTED", "body": "", "submittedAt": "2026-10-09T10:00:01Z"},
                {"author": {"login": "me"}, "state": "PENDING", "body": "draft", "submittedAt": "2026-10-09T10:00:02Z"},
                {"author": {"login": "bot"}, "state": "DISMISSED", "body": "old", "submittedAt": "2026-10-09T10:00:03Z"},
            ],
            "comments": [
                {"author": {"login": "vercel"}, "body": "Preview ready", "createdAt": "2026-10-09T09:00:00Z", "url": "u1"},
                {"author": {"login": "me"}, "body": "Done.\n<!-- tenx:agent -->", "createdAt": "2026-10-09T11:00:00Z", "url": "u2"},
                {"author": {"login": "spam"}, "body": "hidden", "isMinimized": true, "createdAt": "2026-10-09T09:30:00Z", "url": "u3"},
            ],
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE", "completedAt": "2026-10-09T10:30:00Z", "detailsUrl": "d1"},
                {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS", "completedAt": "2026-10-09T10:31:00Z", "detailsUrl": "d2"},
                {"__typename": "CheckRun", "name": "e2e", "status": "IN_PROGRESS", "conclusion": "", "completedAt": "0001-01-01T00:00:00Z", "detailsUrl": "d3"},
                {"__typename": "CheckRun", "name": "old", "status": "COMPLETED", "conclusion": "CANCELLED", "completedAt": "2026-10-09T10:32:00Z", "detailsUrl": "d4"},
                {"__typename": "StatusContext", "context": "deploy", "state": "ERROR", "startedAt": "2026-10-09T10:33:00Z", "targetUrl": "d5"},
                {"__typename": "StatusContext", "context": "gate", "state": "PENDING", "startedAt": "2026-10-09T10:34:00Z", "targetUrl": ""},
            ],
        })
    }

    fn lines() -> Vec<Value> {
        vec![
            json!({"user": {"login": "ann"}, "path": "src/a.rs", "line": 7, "body": "Off by one?", "created_at": "2026-10-09T10:00:01Z", "html_url": "l1"}),
            json!({"user": {"login": "me"}, "path": "src/a.rs", "line": 7, "body": "Fixed. <!-- tenx:agent -->", "created_at": "2026-10-09T11:30:00Z", "html_url": "l2"}),
        ]
    }

    #[test]
    fn first_poll_reports_all_feedback_but_own_and_hidden() {
        let p = poll(&view(), &lines(), None).unwrap();
        let kinds: Vec<_> = p.events.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::Comment,
                EventKind::Review("CHANGES_REQUESTED".into()),
                EventKind::LineComment("src/a.rs:7".into()),
                EventKind::CheckFailed("test (failure)".into()),
                EventKind::CheckFailed("deploy (error)".into()),
            ]
        );
        assert_eq!(p.running_checks, 2);
        // The agent's own reply is the newest thing seen: no news before it.
        assert_eq!(p.cursor.as_deref(), Some("2026-10-09T11:30:00Z"));
        assert_eq!(outcome(&p), Some(Outcome::News));
    }

    #[test]
    fn nothing_after_the_cursor_means_keep_waiting() {
        let p = poll(&view(), &lines(), Some("2026-10-09T11:30:00Z")).unwrap();
        assert!(p.events.is_empty());
        assert_eq!(p.cursor.as_deref(), Some("2026-10-09T11:30:00Z"));
        assert_eq!(outcome(&p), None);
    }

    #[test]
    fn only_events_after_the_cursor_are_news() {
        let p = poll(&view(), &lines(), Some("2026-10-09T10:30:00Z")).unwrap();
        assert_eq!(p.events.len(), 1);
        assert_eq!(p.events[0].kind, EventKind::CheckFailed("deploy (error)".into()));
    }

    #[test]
    fn merged_or_closed_ends_the_wait() {
        let mut v = view();
        v["state"] = json!("MERGED");
        let p = poll(&v, &[], Some("2026-10-09T12:00:00Z")).unwrap();
        assert_eq!(outcome(&p), Some(Outcome::Merged));
        assert!(!render(&p).contains("next:"));
        v["state"] = json!("CLOSED");
        assert_eq!(outcome(&poll(&v, &[], None).unwrap()), Some(Outcome::Closed));
        assert_eq!(Outcome::Merged.exit_code(), 10);
    }

    #[test]
    fn render_lists_events_and_the_next_command() {
        let p = poll(&view(), &lines(), Some("2026-10-09T10:30:00Z")).unwrap();
        let text = render(&p);
        assert!(text.contains("── check failed: deploy (error) · 2026-10-09T10:33:00Z\nd5\n"));
        assert!(text.contains("PR #12 is open, 2 check(s) still running\n"));
        assert!(text.ends_with(&format!("next: tenx pr wait {URL} --since 2026-10-09T11:30:00Z\n")));
    }

    #[test]
    fn long_bodies_are_clipped() {
        let mut v = view();
        v["comments"] = json!([{"author": {"login": "x"}, "body": "é".repeat(2000), "createdAt": "2026-10-09T09:00:00Z", "url": "u"}]);
        let p = poll(&v, &[], None).unwrap();
        assert_eq!(p.events[0].body.chars().count(), BODY_LIMIT + 1);
        assert_eq!(clip("<!-- state {} -->\nPlan: 1 to change <!-- x -->ok"), "Plan: 1 to change ok");
        assert_eq!(clip("open <!-- never closed"), "open");
    }

    #[test]
    fn pr_urls_and_cursors() {
        assert_eq!(parse_pr_url(URL), Some(("acme/api".into(), 12)));
        assert_eq!(parse_pr_url("https://github.com/acme/api/pull/12/files"), Some(("acme/api".into(), 12)));
        assert_eq!(parse_pr_url("https://github.com/acme/api/issues/12"), None);
        assert!(valid_cursor("2026-10-09T11:30:00Z"));
        assert!(!valid_cursor("2026-10-09"));
        assert!(poll(&json!({"message": "no pull requests found"}), &[], None).is_none());
    }
}
