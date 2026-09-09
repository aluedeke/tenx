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
//! The key is the directory's absolute path as Claude Code computes it:
//! `path.resolve(cwd)` — no symlink resolution — so a workspace reached
//! through a symlink is keyed by the path the shell is in. Callers pass the
//! real path too when it differs; seeding both is what the runner does.

use serde_json::{Map, Value};

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
