mod mouse;
mod overlay;

pub fn run_overlay(home: bool) -> anyhow::Result<()> {
    overlay::run(home)
}

/// `tenx overlay --json`: dump every task across all registered workspaces as
/// JSON, sorted by last agent activity (newest first) — same ordering as the
/// TUI. For scripts and other front ends (the future native client reads the
/// same shape).
///
/// Two collections: `tasks` (what to list) and `workspaces` (which repos
/// exist, so a repo checklist can be built without reaching the workspace
/// dirs). Each task carries the repos it actually has worktrees for.
pub fn dump_json() -> anyhow::Result<()> {
    let global = crate::workspace::load_global().unwrap_or_default();
    // One registry read for the whole dump — every task's live state is resolved
    // against this same snapshot.
    let sessions = crate::workspace::claude::sessions();
    let signals = crate::tmux::signals();
    let mut entries = Vec::new();
    let mut workspaces = Vec::new();
    for ws in crate::workspace::registered_workspaces() {
        let bare_dir = ws.bare_dir(&global);
        let cloned = crate::workspace::cloned_repos(&bare_dir, &ws.config.repos);
        workspaces.push(serde_json::json!({
            "name": ws.config.name,
            "dir": ws.dir,
            "repos": ws
                .config
                .repos
                .iter()
                .map(|r| serde_json::json!({
                    "name": r.name,
                    "cloned": cloned.contains(&r.name),
                }))
                .collect::<Vec<_>>(),
        }));
        for task in ws.tasks().unwrap_or_default() {
            let state = crate::workspace::resolve_task_state(&task.path, &sessions, &signals);
            let activity = state.changed.unwrap_or(task.created_at);
            entries.push((activity, crate::workspace::task_json(&ws, &task, &state)));
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let tasks: Vec<_> = entries.into_iter().map(|(_, v)| v).collect();
    println!("{}", serde_json::json!({ "workspaces": workspaces, "tasks": tasks }));
    Ok(())
}
