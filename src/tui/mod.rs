mod mouse;
mod overlay;

pub fn run_overlay(home: bool) -> anyhow::Result<()> {
    overlay::run(home)
}

/// `tenx overlay --json`: dump every task across all registered workspaces as
/// JSON, sorted by last agent activity (newest first) — same ordering as the
/// TUI. Consumed by the tenx-zellij overlay plugin, which runs in zellij's
/// wasm sandbox and therefore cannot discover tasks from the filesystem.
pub fn dump_json() -> anyhow::Result<()> {
    let mut entries = Vec::new();
    for ws in crate::workspace::registered_workspaces() {
        for task in ws.tasks().unwrap_or_default() {
            let (status, changed) = crate::workspace::read_task_status(&task.path);
            let open = task.path.join(".tenx-tab-id").exists();
            let activity = changed.unwrap_or(task.created_at);
            entries.push((
                activity,
                serde_json::json!({
                    "ws": ws.config.name,
                    "ws_dir": ws.dir,
                    "slug": task.name,
                    "title": task.display_name,
                    "status": status.token(),
                    "age_secs": changed.and_then(|c| c.elapsed().ok()).map(|d| d.as_secs()),
                    "open": open,
                }),
            ));
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let tasks: Vec<_> = entries.into_iter().map(|(_, v)| v).collect();
    println!("{}", serde_json::json!({ "tasks": tasks }));
    Ok(())
}
