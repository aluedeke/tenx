mod mouse;
mod overlay;

pub fn run_overlay(home: bool) -> anyhow::Result<()> {
    overlay::run(home)
}

/// `tenx overlay --json`: dump every task across all registered workspaces as
/// JSON, sorted by last agent activity (newest first) — same ordering as the
/// TUI. Consumed by the tenx-zellij overlay plugin, which runs in zellij's
/// wasm sandbox and therefore cannot discover tasks from the filesystem.
///
/// Two collections, because the plugin needs both axes: `tasks` (what to list)
/// and `workspaces` (which repos exist, so the create/edit repo checklists can
/// be built without filesystem access). Each task carries the repos it actually
/// has worktrees for, which is what the edit checklist diffs against.
pub fn dump_json() -> anyhow::Result<()> {
    let global = crate::workspace::load_global().unwrap_or_default();
    // One registry read for the whole dump — every task's live state is resolved
    // against this same snapshot.
    let sessions = crate::workspace::claude::sessions();
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
            let state = crate::workspace::resolve_task_state(&task.path, &sessions);
            let activity = state.changed.unwrap_or(task.created_at);
            entries.push((
                activity,
                serde_json::json!({
                    "ws": ws.config.name,
                    "ws_dir": ws.dir,
                    "slug": task.name,
                    "title": task.display_name,
                    "status": state.status.token(),
                    "waiting_for": state.waiting_for,
                    "sessions": state.sessions,
                    "agents": state.agents,
                    "age_secs": state.changed.and_then(|c| c.elapsed().ok()).map(|d| d.as_secs()),
                    "repos": task.repos,
                }),
            ));
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let tasks: Vec<_> = entries.into_iter().map(|(_, v)| v).collect();
    println!("{}", serde_json::json!({ "workspaces": workspaces, "tasks": tasks }));
    Ok(())
}
