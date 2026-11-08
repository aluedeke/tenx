//! A task's pull requests as one GitHub GraphQL request (`gh api graphql`):
//! the newest PR of each repo's branch, and every PR the task links to — its
//! state, each check, its comments and how many review threads are still
//! unresolved. The watcher sends [`query`] and reads the answer with
//! [`parse`]; the sidebar and the chips read the [`PrInfo`]s it makes.

use crate::live::PrInfo;

/// A repo on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

/// A PR by its place on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub repo: Repo,
    pub number: u64,
}

/// A repo worktree's branch, whose PR is the task's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchRef {
    /// The worktree's directory name: what [`PrInfo::repo`] says.
    pub dir: String,
    pub repo: Repo,
    pub branch: String,
}

/// How a remote names its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// `https://host/…`: the host itself.
    Web(String),
    /// `git@host:…`, `ssh://git@host/…`: maybe an alias of `~/.ssh/config`,
    /// which the caller resolves (`ssh -G`).
    Ssh(String),
}

/// `origin`'s URL → its host and the repo: `git@github.com:o/r.git`,
/// `https://github.com/o/r(.git)`, `ssh://git@github.com:22/o/r`, and the
/// same on any other host (GitHub Enterprise, an SSH alias).
pub fn remote_repo(remote: &str) -> Option<(Host, Repo)> {
    let rest = remote.trim();
    let (host, path) = if let Some((scheme, after)) = rest.split_once("://") {
        let (authority, path) = after.split_once('/')?;
        let host = authority.rsplit('@').next()?.split(':').next()?.to_string();
        (if scheme == "ssh" { Host::Ssh(host) } else { Host::Web(host) }, path)
    } else {
        let (authority, path) = rest.split_once(':')?;
        (Host::Ssh(authority.rsplit('@').next()?.to_string()), path)
    };
    let mut seg = path.trim_end_matches('/').trim_end_matches(".git").split('/');
    let owner = seg.next().filter(|s| !s.is_empty())?;
    let name = seg.next().filter(|s| !s.is_empty())?;
    if seg.next().is_some() || matches!(&host, Host::Web(h) | Host::Ssh(h) if h.is_empty()) {
        return None;
    }
    Some((host, Repo { owner: owner.to_string(), name: name.to_string() }))
}

/// A PR link → the PR: `https://github.com/o/r/pull/12[/files]`.
pub fn pr_ref(url: &str) -> Option<PrRef> {
    let path = url.split_once("github.com/")?.1;
    let mut seg = path.split(['/', '?', '#']);
    let owner = seg.next().filter(|s| !s.is_empty())?;
    let name = seg.next().filter(|s| !s.is_empty())?;
    if seg.next()? != "pull" {
        return None;
    }
    let number = seg.next()?.parse().ok()?;
    Some(PrRef { repo: Repo { owner: owner.into(), name: name.into() }, number })
}

const FIELDS: &str = "number title url state isDraft reviewDecision headRepositoryOwner { login } \
comments { totalCount } \
reviewThreads(first: 100) { totalCount nodes { isResolved } } \
commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes { \
__typename ... on CheckRun { status conclusion } ... on StatusContext { state } } } } } } }";

fn quoted(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// How many of a branch name's newest PRs are asked for: the name is matched
/// in forks too, and [`parse`] keeps the one from the repo itself.
const BRANCH_PRS: usize = 5;

/// The request body for `branches`' newest PRs (`b0`, `b1`, …) and the
/// `linked` PRs (`l0`, …), in one query.
pub fn query(branches: &[BranchRef], linked: &[PrRef]) -> String {
    let mut parts = Vec::new();
    for (i, b) in branches.iter().enumerate() {
        parts.push(format!(
            "b{i}: repository(owner: {}, name: {}) {{ pullRequests(headRefName: {}, first: {BRANCH_PRS}, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ {FIELDS} }} }} }}",
            quoted(&b.repo.owner),
            quoted(&b.repo.name),
            quoted(&b.branch)
        ));
    }
    for (i, l) in linked.iter().enumerate() {
        parts.push(format!(
            "l{i}: repository(owner: {}, name: {}) {{ pullRequest(number: {}) {{ {FIELDS} }} }}",
            quoted(&l.repo.owner),
            quoted(&l.repo.name),
            l.number
        ));
    }
    format!("{{ {} }}", parts.join(" "))
}

/// One PR node of the answer.
pub fn summarize(repo: &str, pr: &serde_json::Value) -> Option<PrInfo> {
    let number = pr["number"].as_u64()?;
    let (mut pass, mut fail, mut pending) = (0, 0, 0);
    let contexts = &pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"];
    for c in contexts.as_array().into_iter().flatten() {
        let outcome = if c["__typename"] == "StatusContext" {
            c["state"].as_str().unwrap_or("")
        } else if c["status"].as_str().unwrap_or("COMPLETED") != "COMPLETED" {
            "PENDING"
        } else {
            c["conclusion"].as_str().unwrap_or("")
        };
        match outcome {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => pass += 1,
            "PENDING" | "EXPECTED" | "" => pending += 1,
            _ => fail += 1,
        }
    }
    let checks = if fail > 0 {
        "failure"
    } else if pending > 0 {
        "pending"
    } else if pass > 0 {
        "success"
    } else {
        ""
    };
    let threads = &pr["reviewThreads"];
    let unresolved = threads["nodes"].as_array().into_iter().flatten().filter(|t| t["isResolved"] == false).count() as u32;
    Some(PrInfo {
        repo: repo.to_string(),
        number,
        state: pr["state"].as_str().unwrap_or("").to_string(),
        url: pr["url"].as_str().unwrap_or("").to_string(),
        draft: pr["isDraft"].as_bool().unwrap_or(false),
        review: pr["reviewDecision"].as_str().unwrap_or("").to_string(),
        checks: checks.to_string(),
        title: pr["title"].as_str().unwrap_or("").to_string(),
        checks_pass: pass,
        checks_fail: fail,
        checks_pending: pending,
        comments: pr["comments"]["totalCount"].as_u64().unwrap_or(0) as u32 + threads["totalCount"].as_u64().unwrap_or(0) as u32,
        unresolved,
    })
}

/// The answer to [`query`]: the branches' PRs — the newest pushed from the
/// repo itself, not a fork's branch of the same name; a branch without one
/// left out — and the linked PRs that were found. `None` when the request failed
/// as a whole (no `data`), so the caller keeps what it had.
pub fn parse(json: &serde_json::Value, branches: &[BranchRef], linked: &[PrRef]) -> Option<(Vec<PrInfo>, Vec<PrInfo>)> {
    let data = json.get("data").filter(|d| d.is_object())?;
    let own = branches
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            let nodes = data[format!("b{i}")]["pullRequests"]["nodes"].as_array()?;
            let own = nodes.iter().find(|n| n["headRepositoryOwner"]["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(&b.repo.owner)))?;
            summarize(&b.dir, own)
        })
        .collect();
    let others = linked
        .iter()
        .enumerate()
        .filter_map(|(i, l)| summarize(&l.repo.name, &data[format!("l{i}")]["pullRequest"]))
        .collect();
    Some((own, others))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_and_pr_links_are_read() {
        let r = Repo { owner: "aluedeke".into(), name: "tenx".into() };
        let ssh = |h: &str| Some((Host::Ssh(h.into()), r.clone()));
        assert_eq!(remote_repo("git@github.com:aluedeke/tenx.git"), ssh("github.com"));
        assert_eq!(remote_repo("https://github.com/aluedeke/tenx"), Some((Host::Web("github.com".into()), r.clone())));
        assert_eq!(remote_repo("ssh://git@github.com:22/aluedeke/tenx.git/"), ssh("github.com"));
        assert_eq!(remote_repo("git@github-work:aluedeke/tenx.git"), ssh("github-work"), "an SSH alias");
        assert_eq!(remote_repo("https://ghe.corp.com/aluedeke/tenx.git"), Some((Host::Web("ghe.corp.com".into()), r.clone())));
        assert_eq!(remote_repo("/srv/git/tenx.git"), None);
        assert_eq!(pr_ref("https://github.com/aluedeke/tenx/pull/32/files"), Some(PrRef { repo: r, number: 32 }));
        assert_eq!(pr_ref("https://github.com/aluedeke/tenx/issues/3"), None);
    }

    #[test]
    fn one_query_names_every_pr() {
        let repo = Repo { owner: "o".into(), name: "r".into() };
        let q = query(
            &[BranchRef { dir: "r".into(), repo: repo.clone(), branch: "task-x".into() }],
            &[PrRef { repo, number: 7 }],
        );
        assert!(q.contains(r#"b0: repository(owner: "o", name: "r") { pullRequests(headRefName: "task-x""#), "{q}");
        assert!(q.contains(r#"l0: repository(owner: "o", name: "r") { pullRequest(number: 7)"#), "{q}");
        assert!(q.contains("reviewThreads(first: 100)"));
    }

    #[test]
    fn checks_comments_and_threads_are_counted() {
        let node = serde_json::json!({
            "number": 33, "title": "Sidebar", "url": "https://github.com/o/r/pull/33", "state": "OPEN",
            "isDraft": false, "reviewDecision": "REVIEW_REQUIRED",
            "comments": {"totalCount": 1},
            "reviewThreads": {"totalCount": 2, "nodes": [{"isResolved": false}, {"isResolved": true}]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
                {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"__typename": "CheckRun", "status": "IN_PROGRESS", "conclusion": null},
                {"__typename": "StatusContext", "state": "SUCCESS"}
            ]}}}}]}
        });
        let pr = summarize("r", &node).unwrap();
        assert_eq!((pr.checks_pass, pr.checks_fail, pr.checks_pending), (2, 1, 1));
        assert_eq!(pr.checks, "failure");
        assert_eq!((pr.comments, pr.unresolved), (3, 1));
        assert_eq!(pr.chip(), "#33 ✗");
    }

    #[test]
    fn the_answer_splits_into_own_and_linked() {
        let repo = Repo { owner: "o".into(), name: "r".into() };
        let branches = [BranchRef { dir: "api".into(), repo: repo.clone(), branch: "b".into() }, BranchRef { dir: "web".into(), repo: repo.clone(), branch: "c".into() }];
        let linked = [PrRef { repo, number: 30 }];
        let pr = |n: u64| serde_json::json!({"number": n, "state": "MERGED", "url": format!("https://github.com/o/r/pull/{n}"), "headRepositoryOwner": {"login": "O"}});
        let fork = serde_json::json!({"number": 99, "state": "OPEN", "headRepositoryOwner": {"login": "stranger"}});
        let answer = serde_json::json!({"data": {
            "b0": {"pullRequests": {"nodes": [fork, pr(31)]}},
            "b1": {"pullRequests": {"nodes": []}},
            "l0": {"pullRequest": pr(30)}
        }});
        let (own, others) = parse(&answer, &branches, &linked).unwrap();
        assert_eq!(own.iter().map(|p| (p.repo.as_str(), p.number)).collect::<Vec<_>>(), vec![("api", 31)]);
        assert_eq!(others.iter().map(|p| p.number).collect::<Vec<_>>(), vec![30]);
        assert!(parse(&serde_json::json!({"errors": [{"message": "Bad credentials"}]}), &branches, &linked).is_none());
    }
}
