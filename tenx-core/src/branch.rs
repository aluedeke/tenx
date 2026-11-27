//! A task's branch name: the `branch` pattern from the workspace's
//! `config.toml`, else the global one, with `{slug}` replaced by the task's
//! slug. No pattern is the old rule: the branch is the slug.
//!
//! The pattern must hold `{slug}` exactly once, so two tasks never share a
//! branch (`git worktree add -B` would reset the first task's branch to start
//! the second). Whether the result is a valid branch name is git's to say
//! (`git check-ref-format --branch`); the binary asks it.

/// The placeholder a pattern names the slug with.
pub const SLUG: &str = "{slug}";

/// The pattern in force: the workspace's, else the global one, else `{slug}`.
pub fn pattern<'a>(workspace: &'a str, global: &'a str) -> &'a str {
    [workspace.trim(), global.trim()].into_iter().find(|p| !p.is_empty()).unwrap_or(SLUG)
}

/// `pattern` with `{slug}` replaced, or why the pattern can't be used.
/// An empty pattern is `{slug}`.
pub fn render(pattern: &str, slug: &str) -> Result<String, String> {
    let pattern = self::pattern(pattern, "");
    if pattern.matches(SLUG).count() != 1 {
        return Err(format!("branch pattern {pattern:?} must contain {SLUG} exactly once"));
    }
    if pattern.replace(SLUG, "").contains(['{', '}']) {
        return Err(format!("branch pattern {pattern:?}: {SLUG} is the only placeholder"));
    }
    Ok(pattern.replace(SLUG, slug))
}

/// Whether `branch`, the branch a task's worktree is on, is one tenx made for
/// task `slug`: the slug itself (the rule without a pattern) or the current
/// pattern's rendering. Nothing looser: a branch the user switched the
/// worktree to (`main`, `feature/login`, someone's `andreas/eng-640-…`) is
/// theirs, and is never deleted or reused. A branch made under an earlier,
/// different pattern is left behind — a leftover, never a loss.
pub fn owns(branch: &str, pattern: &str, slug: &str) -> bool {
    branch == slug || render(pattern, slug).is_ok_and(|b| b == branch)
}

/// The branch a repo added to an existing task goes on, given the branches
/// its worktrees are on now: the current pattern's rendering if any worktree
/// is on it, else the slug if one is (a task from before the pattern), else
/// `None` — the caller renders the pattern. Never a branch tenx didn't make.
pub fn for_added_repo(current: &[String], pattern: &str, slug: &str) -> Option<String> {
    let rendered = render(pattern, slug).ok();
    if let Some(r) = rendered.filter(|r| current.contains(r)) {
        return Some(r);
    }
    current.iter().any(|b| b == slug).then(|| slug.to_string())
}

/// Where a task's worktree stands, as far as its branch goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head<'a> {
    /// On this branch.
    Branch(&'a str),
    /// A worktree with no branch checked out (a rebase, `git checkout <sha>`).
    Detached,
    /// No worktree to read: creation stopped partway.
    Missing,
}

/// What deleting task `slug`'s worktree in one repo deletes: the branch it
/// was on if tenx made it; nothing if that is the user's or can't be told
/// (detached); and for a missing worktree, the name a new task would get,
/// which `-B` would reset anyway.
pub fn to_delete(head: Head, pattern: &str, slug: &str) -> Option<String> {
    match head {
        Head::Branch(b) => owns(b, pattern, slug).then(|| b.to_string()),
        Head::Detached => None,
        Head::Missing => render(pattern, slug).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pattern_keeps_the_slug() {
        assert_eq!(render("", "cors-config").unwrap(), "cors-config");
        assert_eq!(render("  ", "cors-config").unwrap(), "cors-config");
        assert_eq!(render("{slug}", "cors-config").unwrap(), "cors-config");
    }

    #[test]
    fn a_pattern_puts_the_slug_in_place() {
        assert_eq!(render("andreas/{slug}", "cors-config").unwrap(), "andreas/cors-config");
        assert_eq!(render("feat/{slug}-wip", "x").unwrap(), "feat/x-wip");
    }

    #[test]
    fn a_pattern_without_the_slug_once_is_refused() {
        assert!(render("andreas/main", "x").is_err());
        assert!(render("{slug}/{slug}", "x").is_err());
        assert!(render("{user}/{slug}", "x").is_err());
    }

    #[test]
    fn the_workspace_pattern_wins_over_the_global_one() {
        assert_eq!(pattern("ws/{slug}", "global/{slug}"), "ws/{slug}");
        assert_eq!(pattern("", "global/{slug}"), "global/{slug}");
        assert_eq!(pattern(" ", ""), SLUG);
    }

    #[test]
    fn a_task_owns_the_branches_tenx_made_for_it() {
        assert!(owns("x", "", "x"));
        assert!(owns("andreas/x", "andreas/{slug}", "x"));
        assert!(owns("feat/x-wip", "feat/{slug}-wip", "x"));
        assert!(owns("x", "andreas/{slug}", "x"), "a task from before the pattern");
    }

    #[test]
    fn a_branch_switched_to_by_hand_is_not_the_tasks() {
        assert!(!owns("main", "andreas/{slug}", "x"));
        assert!(!owns("andreas/eng-640-columns", "andreas/{slug}", "billing"));
        assert!(!owns("fix-x", "", "x"));
        assert!(!owns("feature/x", "andreas/{slug}", "x"));
        assert!(!owns("feature/login", "", "login"));
    }

    #[test]
    fn an_added_repo_joins_the_tasks_own_branch() {
        let on = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let p = "andreas/{slug}";
        assert_eq!(for_added_repo(&on(&["x"]), p, "x").as_deref(), Some("x"));
        assert_eq!(for_added_repo(&on(&["x", "andreas/x"]), p, "x").as_deref(), Some("andreas/x"));
        assert_eq!(for_added_repo(&on(&["andreas/x", "x"]), p, "x").as_deref(), Some("andreas/x"));
        assert_eq!(for_added_repo(&on(&["bob/x", "main"]), p, "x"), None);
        assert_eq!(for_added_repo(&[], p, "x"), None);
    }

    #[test]
    fn deleting_takes_only_a_branch_tenx_made() {
        let p = "andreas/{slug}";
        assert_eq!(to_delete(Head::Branch("andreas/x"), p, "x").as_deref(), Some("andreas/x"));
        assert_eq!(to_delete(Head::Branch("x"), p, "x").as_deref(), Some("x"));
        assert_eq!(to_delete(Head::Branch("main"), p, "x"), None);
        assert_eq!(to_delete(Head::Branch("feature/x"), p, "x"), None);
        assert_eq!(to_delete(Head::Detached, p, "x"), None);
        assert_eq!(to_delete(Head::Missing, p, "x").as_deref(), Some("andreas/x"));
        assert_eq!(to_delete(Head::Missing, "", "x").as_deref(), Some("x"));
        assert_eq!(to_delete(Head::Missing, "broken", "x"), None);
    }
}
