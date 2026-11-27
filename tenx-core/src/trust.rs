//! Claude Code's per-directory trust grant, as a transformation of its global
//! config (`~/.claude.json`).
//!
//! Every tenx task directory carries a `.claude` symlink to the workspace's
//! `.claude/`, whose `settings.json` pre-approves tool permissions. Claude Code
//! inherits *trust* from a parent directory's grant, but project-scoped
//! permission rules are gated behind trusting the exact directory — a task
//! folder trusted only through the workspace root therefore gets the "This
//! folder pre-approves N tool permissions … Yes, I trust this folder" dialog
//! on every first launch. Claude Code's own remedy, printed when it drops such
//! rules, is to set `projects[<dir>].hasTrustDialogAccepted: true` in the
//! global config; its cloud runner seeds exactly that before a session starts.
//! tenx does the same for a task it creates, since the task directory is
//! tenx's own, symlinked to a `.claude/` the user already trusted.
//!
//! The same grant also covers the directories a launch passes with
//! `--add-dir` (a task's worktrees, its workspace, or every workspace for an
//! adhoc session): Claude Code asks about each untrusted one at every start,
//! and keys one inside a git repository by that repository — for a worktree,
//! its bare repo. [`launch_dirs`] is that set.
//!
//! The key is the directory's absolute path as Claude Code computes it:
//! `path.resolve(cwd)` — no symlink resolution — so a workspace reached
//! through a symlink is keyed by the path the shell is in. Callers pass the
//! real path too when it differs; seeding both is what the runner does.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// The directories to trust before a Claude launch: the task directory, each
/// `--add-dir` directory, and the bare repo of each of the task's worktrees —
/// Claude Code names a worktree by its repository ("This directory is part of
/// the repository at …/.bare/<name>.git"). In that order, each once.
///
/// The bare repos are the ones tenx cloned, handed in by the caller from the
/// workspace's own layout — never a repository found by walking up from a
/// directory, which could be one enclosing a workspace that the user never
/// meant to trust.
pub fn launch_dirs(task_dir: &Path, add_dirs: &[PathBuf], bare_repos: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for d in std::iter::once(task_dir).chain(add_dirs.iter().map(PathBuf::as_path)).chain(bare_repos.iter().map(PathBuf::as_path)) {
        if !out.iter().any(|o| o == d) {
            out.push(d.to_path_buf());
        }
    }
    out
}

/// Mark every key in `dirs` as trusted, preserving everything else in the
/// config. Returns `true` if the config changed — a caller writes the file
/// only then, so an already-trusted task is a read, never a rewrite.
///
/// A missing `projects` map is created; a project entry that exists keeps its
/// other fields (`allowedTools`, MCP approvals, …). A `projects` value that
/// isn't an object is left alone and reported as unchanged rather than
/// clobbered: the file belongs to another program.
pub fn grant_trust(config: &mut Value, dirs: &[String]) -> bool {
    let Some(root) = config.as_object_mut() else {
        return false;
    };
    let projects = root.entry("projects").or_insert_with(|| Value::Object(Map::new()));
    let Some(projects) = projects.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    for dir in dirs {
        let entry = projects.entry(dir.as_str()).or_insert_with(|| Value::Object(Map::new()));
        let Some(entry) = entry.as_object_mut() else {
            continue;
        };
        if entry.get("hasTrustDialogAccepted").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        entry.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn launch_dirs_are_task_add_dirs_and_bare_repos_once_each() {
        let p = PathBuf::from;
        let got = launch_dirs(
            Path::new("/ws/tasks/a"),
            &[p("/ws/tasks/a/app"), p("/ws/tasks/a/lib"), p("/ws/tasks/a")],
            &[p("/ws/.bare/app.git"), p("/ws/.bare/lib.git")],
        );
        assert_eq!(got, vec![p("/ws/tasks/a"), p("/ws/tasks/a/app"), p("/ws/tasks/a/lib"), p("/ws/.bare/app.git"), p("/ws/.bare/lib.git")]);
    }

    #[test]
    fn launch_dirs_of_a_repo_less_task_add_only_the_workspace() {
        let got = launch_dirs(Path::new("/ws/tasks/a"), &[PathBuf::from("/ws")], &[]);
        assert_eq!(got, vec![PathBuf::from("/ws/tasks/a"), PathBuf::from("/ws")]);
    }

    #[test]
    fn adds_entry_and_map_when_missing() {
        let mut cfg = json!({"numStartups": 3});
        assert!(grant_trust(&mut cfg, &["/ws/tasks/a".into()]));
        assert_eq!(cfg, json!({"numStartups": 3, "projects": {"/ws/tasks/a": {"hasTrustDialogAccepted": true}}}));
    }

    #[test]
    fn keeps_other_project_fields_and_flips_false() {
        let mut cfg = json!({"projects": {"/ws/tasks/a": {"allowedTools": ["Bash"], "hasTrustDialogAccepted": false}}});
        assert!(grant_trust(&mut cfg, &["/ws/tasks/a".into()]));
        assert_eq!(cfg["projects"]["/ws/tasks/a"], json!({"allowedTools": ["Bash"], "hasTrustDialogAccepted": true}));
    }

    #[test]
    fn already_trusted_is_unchanged() {
        let mut cfg = json!({"projects": {"/ws/tasks/a": {"hasTrustDialogAccepted": true}}});
        let before = cfg.clone();
        assert!(!grant_trust(&mut cfg, &["/ws/tasks/a".into()]));
        assert_eq!(cfg, before);
    }

    #[test]
    fn seeds_every_key_given() {
        let mut cfg = json!({});
        assert!(grant_trust(&mut cfg, &["/link/tasks/a".into(), "/real/tasks/a".into()]));
        assert_eq!(cfg["projects"]["/link/tasks/a"]["hasTrustDialogAccepted"], json!(true));
        assert_eq!(cfg["projects"]["/real/tasks/a"]["hasTrustDialogAccepted"], json!(true));
    }

    #[test]
    fn refuses_foreign_shapes() {
        let mut cfg = json!({"projects": "what"});
        assert!(!grant_trust(&mut cfg, &["/ws/tasks/a".into()]));
        assert_eq!(cfg, json!({"projects": "what"}));
        let mut cfg = json!([]);
        assert!(!grant_trust(&mut cfg, &["/ws/tasks/a".into()]));
        let mut cfg = json!({"projects": {"/ws/tasks/a": 7}});
        assert!(!grant_trust(&mut cfg, &["/ws/tasks/a".into()]));
    }
}
