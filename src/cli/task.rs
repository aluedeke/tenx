use anyhow::{bail, Context, Result};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::Path;

pub fn new(name: &str, repos: Option<&[String]>, no_open: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    new_in(&ws, name, repos, no_open)
}

/// Create a task in an explicit workspace (no cwd dependency). Used by `new` and
/// the overlay's create flow, which targets a workspace the user picks.
pub fn new_in(
    ws: &crate::workspace::Workspace,
    name: &str,
    repos: Option<&[String]>,
    no_open: bool,
) -> Result<()> {
    let display_name = name.to_string();
    let slug = crate::workspace::slugify(name);
    let slug = slug.as_str();

    let global = crate::workspace::load_global()?;

    ws.check_task_new(slug)?;

    // Determine which repos to include
    let repo_names: Vec<String> = match repos {
        Some(r) => r.to_vec(),
        None => ws.config.repos.iter().map(|r| r.name.clone()).collect(),
    };

    if repo_names.is_empty() {
        bail!("no repos in workspace — run: tenx repo add <url>");
    }

    let bare_dir = ws.bare_dir(&global);
    let task_dir = ws.tasks_dir().join(slug);
    std::fs::create_dir_all(&task_dir)?;
    write_task_md(&task_dir, &display_name)?;
    write_claude_hooks(&task_dir)?;

    for repo_name in &repo_names {
        let repo = ws.find_repo(repo_name).ok_or_else(|| {
            crate::workspace::WorkspaceError::RepoNotFound(repo_name.clone())
        })?;
        let bare_path = crate::git::bare_repo_path(&bare_dir, &repo.name);
        let verb = if bare_path.exists() { "fetching" } else { "cloning" };
        let spinner = crate::progress::Spinner::new(format!("{verb} {}", repo.name));
        match crate::git::ensure_synced(&repo.url, &bare_dir, &repo.name) {
            Ok(_) => spinner.done(),
            Err(e) => { spinner.fail(&e.to_string()); return Err(e); }
        }

        let worktree_path = task_dir.join(&repo.name);
        let spinner = crate::progress::Spinner::new(format!("worktree {}", repo.name));
        match crate::git::add_worktree(&bare_path, &worktree_path, slug) {
            Ok(_) => spinner.done(),
            Err(e) => { spinner.fail(&e.to_string()); return Err(e); }
        }
    }

    if !no_open {
        if crate::zellij::current_session().as_deref() != Some(crate::zellij::SESSION) {
            eprintln!("! not inside the '{}' session — run: tenx", crate::zellij::SESSION);
            eprintln!("  to open later: tenx task open {}", slug);
            return Ok(());
        }
        let layout = ws.config.layout.as_str();
        let opts = crate::zellij::TabOptions {
            name: &display_name,
            cwd: &task_dir.to_string_lossy(),
            workspace_dir: &ws.dir.to_string_lossy(),
            layout_file: if layout.is_empty() { None } else { Some(layout) },
            resume: false, // brand-new task — no conversation to continue
        };
        let tab_id = crate::zellij::open_or_switch(&opts)?;
        std::fs::write(task_dir.join(".tenx-tab-id"), tab_id.to_string())?;
    }
    Ok(())
}

pub fn open(name: &str) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    let slug = crate::workspace::slugify(name);
    open_in(&ws, &slug)
}

/// Open a task given an explicit workspace directory and exact slug. Used by
/// the overlay plugin (cross-workspace, no meaningful cwd, slug already known).
pub fn open_by_dir(ws_dir: &str, slug: &str) -> Result<()> {
    let ws = crate::workspace::load(Path::new(ws_dir))?;
    open_in(&ws, slug)
}

/// Create a task in an explicit workspace directory (all repos, opens a tab).
/// Used by the overlay plugin.
pub fn new_by_dir(ws_dir: &str, name: &str) -> Result<()> {
    let ws = crate::workspace::load(Path::new(ws_dir))?;
    new_in(&ws, name, None, false)
}

/// Delete a task by explicit workspace directory and exact slug (no prompt).
/// Used by the overlay plugin, which does its own confirmation.
pub fn rm_by_dir(ws_dir: &str, slug: &str) -> Result<()> {
    let ws = crate::workspace::load(Path::new(ws_dir))?;
    rm_in(&ws, slug, true)
}

/// Rename a task's display title. If the task's tab is open, its zellij tab is
/// renamed to match. `ws_dir` selects the workspace (cwd if None).
pub fn rename(ws_dir: Option<&str>, slug: &str, title: &str) -> Result<()> {
    let ws = match ws_dir {
        Some(dir) => crate::workspace::load(Path::new(dir))?,
        None => crate::workspace::find(&env::current_dir()?)?,
    };
    let task = ws.find_task(slug)?;
    crate::workspace::set_task_title(&task.path, title)?;
    // If the task's tab is open, keep its name in sync with the new title.
    if let Ok(id_str) = std::fs::read_to_string(task.path.join(".tenx-tab-id")) {
        if let Ok(id) = id_str.trim().parse::<u32>() {
            let _ = crate::zellij::rename_tab_by_id(id, title);
        }
    }
    Ok(())
}

/// Focus a task's zellij tab within the current session (creating it if needed),
/// given an explicit workspace and slug. Used by `open` and the overlay, neither
/// of which can rely on cwd matching the task.
pub fn open_in(ws: &crate::workspace::Workspace, slug: &str) -> Result<()> {
    let task = ws.find_task(slug)?;
    let display_name = crate::workspace::read_task_display_name(&task.path);

    if crate::zellij::current_session().as_deref() != Some(crate::zellij::SESSION) {
        bail!(
            "not inside the '{}' session — run 'tenx' to attach first",
            crate::zellij::SESSION
        );
    }

    // Try to find the tab by its stored id first.
    let tab_id_file = task.path.join(".tenx-tab-id");
    let existing_id = std::fs::read_to_string(&tab_id_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());

    if let Some(id) = existing_id {
        if let Some(tab) = crate::zellij::find_tab_by_id(id)? {
            // Tab is still open — rename if TASK.md title changed, then switch.
            if tab.name != display_name {
                crate::zellij::rename_tab_by_id(id, &display_name)?;
            }
            crate::zellij::go_to_tab_position(tab.position)?;
            return Ok(());
        }
    }

    // Tab not open — create it and record the new id.
    let layout = ws.config.layout.as_str();
    let opts = crate::zellij::TabOptions {
        name: &display_name,
        cwd: &task.path.to_string_lossy(),
        workspace_dir: &ws.dir.to_string_lossy(),
        layout_file: if layout.is_empty() { None } else { Some(layout) },
        // Only `--continue` if claude actually has a conversation for this cwd;
        // otherwise it exits 1 and the close_on_exit pane vanishes.
        resume: has_claude_conversation(&task.path),
    };
    let tab_id = crate::zellij::open_or_switch(&opts)?;
    std::fs::write(&tab_id_file, tab_id.to_string())?;
    Ok(())
}

/// Whether claude has stored a conversation for `cwd` (so `--continue` will
/// resume instead of exiting 1). Claude encodes each project dir as its path
/// with `/` → `-` under `~/.claude/projects/`.
fn has_claude_conversation(cwd: &Path) -> bool {
    let Some(home) = env::var_os("HOME") else {
        return false;
    };
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = Path::new(&home).join(".claude/projects").join(encoded);
    match std::fs::read_dir(&project_dir) {
        Ok(entries) => entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|ext| ext == "jsonl")),
        Err(_) => false,
    }
}

pub fn list() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    let tasks = ws.tasks()?;

    // Try to get open tabs from the tenx session (silently ignore if it's not
    // running). Works from anywhere via the cross-session listing.
    let open_tabs: std::collections::HashSet<String> = crate::zellij::list_tabs_in(crate::zellij::SESSION)
        .map(|tabs| tabs.into_iter().map(|t| t.name).collect())
        .unwrap_or_default();

    println!("{:<20} {:<25} {:<20} {:<6} {}", "NAME", "REPOS", "BRANCH", "AGE", "OPEN");
    println!("{}", "-".repeat(80));
    for task in &tasks {
        let repos = task.repos.join(", ");
        let age = crate::workspace::format_age(task.created_at);
        let open = if open_tabs.contains(&task.display_name) { "●" } else { "" };
        println!(
            "{:<20} {:<25} {:<20} {:<6} {}",
            task.display_name, repos, task.branch, age, open
        );
    }
    Ok(())
}

pub fn rm(name: &str, force: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    rm_in(&ws, name, force)
}

/// Delete a task (worktrees, branches, directory) in an explicit workspace.
/// The overlay calls this with `force = true` and does its own confirmation.
pub fn rm_in(ws: &crate::workspace::Workspace, name: &str, force: bool) -> Result<()> {
    let global = crate::workspace::load_global()?;
    let task = ws.find_task(name)?;
    let bare_dir = ws.bare_dir(&global);

    if !force {
        eprint!("delete task '{}'? [y/N] ", name);
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            eprintln!("aborted");
            return Ok(());
        }
    }

    for repo_name in &task.repos {
        let bare_path = crate::git::bare_repo_path(&bare_dir, repo_name);
        let worktree_path = task.path.join(repo_name);
        if bare_path.exists() {
            // Best-effort: if the worktree was never registered (e.g. creation
            // failed mid-way), git remove will error but we still clean the dir.
            if worktree_path.exists() {
                let _ = crate::git::remove_worktree(&bare_path, &worktree_path);
            }
            // Remove the task branch too so it doesn't linger and shadow a
            // future task of the same name with a stale tip. (task.name is the
            // slug, i.e. the branch name used at creation.)
            let _ = crate::git::delete_branch(&bare_path, &task.name);
        }
    }

    std::fs::remove_dir_all(&task.path)
        .with_context(|| format!("remove task directory {}", task.path.display()))?;
    Ok(())
}

fn write_task_md(task_dir: &Path, name: &str) -> Result<()> {
    let path = task_dir.join("TASK.md");
    if path.exists() {
        return Ok(());
    }
    let content = format!(
        "# {name}\n\
         \n\
         ## Description\n\
         \n\
         \n\
         ## Todo\n\
         \n\
         - [ ] \n\
         \n\
         ## Links\n\
         \n\
         - Linear Project:\n\
         - Linear Milestone:\n\
         - Linear:\n\
         - PR:\n\
         \n\
         ## Notes\n\
         \n"
    );
    std::fs::write(&path, content)?;
    Ok(())
}

fn write_claude_hooks(task_dir: &Path) -> Result<()> {
    let workspace_dir = task_dir
        .parent()  // tasks/
        .and_then(|p| p.parent())  // workspace/
        .with_context(|| format!("cannot determine workspace dir from {}", task_dir.display()))?;
    ensure_workspace_claude_settings(workspace_dir)?;

    // Symlink task_dir/.claude -> ../../.claude so Claude Code discovers the
    // workspace settings without a per-task copy.
    let link = task_dir.join(".claude");
    if !link.exists() && !link.is_symlink() {
        std::os::unix::fs::symlink("../../.claude", &link)
            .with_context(|| format!("symlink .claude in {}", task_dir.display()))?;
    }
    Ok(())
}

fn ensure_workspace_claude_settings(workspace_dir: &Path) -> Result<()> {
    ensure_workspace_claude_settings_inner(workspace_dir, false)
}

fn ensure_workspace_claude_settings_inner(workspace_dir: &Path, force: bool) -> Result<()> {
    let claude_dir = workspace_dir.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    // All phase-1 hooks funnel their JSON into a single `event.sh`, which pipes
    // it to `tenx tab event`; the event→state mapping lives in Rust, so adding
    // events is a code change, not a settings.json edit.
    //
    // Fresh workspace → write settings.json with the canonical hook set. On
    // `hooks install` (force) an existing settings.json gets its "hooks" key
    // replaced in place, preserving any other keys the user added — otherwise
    // upgraded workspaces would keep registering the old (deleted) scripts.
    let hook_events =
        ["SessionStart", "SessionEnd", "UserPromptSubmit", "Notification", "Stop", "StopFailure"];
    let event_hook = serde_json::json!([
        { "hooks": [ { "type": "command", "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/event.sh" } ] }
    ]);
    let hooks: serde_json::Map<String, serde_json::Value> = hook_events
        .iter()
        .map(|ev| (ev.to_string(), event_hook.clone()))
        .collect();

    let settings_path = claude_dir.join("settings.json");
    let existing: Option<serde_json::Value> = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    match existing {
        None => {
            // Missing (or unparseable — treat as tenx-owned and rewrite).
            let settings = serde_json::json!({ "hooks": hooks });
            std::fs::write(&settings_path, format!("{:#}\n", settings))?;
        }
        Some(mut settings) if force => {
            settings["hooks"] = serde_json::Value::Object(hooks);
            std::fs::write(&settings_path, format!("{:#}\n", settings))?;
        }
        Some(_) => {} // present and not forcing — leave alone
    }

    let tenx = std::env::current_exe().context("cannot determine tenx binary path")?;
    let tenx = tenx.display();

    // The hook payload arrives on stdin; forward it verbatim to `tenx tab event`.
    write_hook_script(
        &hooks_dir.join("event.sh"),
        &format!("#!/bin/sh\ncd \"$CLAUDE_PROJECT_DIR\" || exit 0\nexec \"{tenx}\" tab event\n"),
        force,
    )?;

    // Remove the pre-event.sh scripts so upgraded workspaces don't keep dead
    // hooks around (the old settings.json referencing them is left alone if the
    // user customized it; a fresh workspace never gets them).
    for old in ["notify.sh", "notify-clear.sh"] {
        let _ = std::fs::remove_file(hooks_dir.join(old));
    }

    Ok(())
}

fn write_hook_script(path: &Path, content: &str, force: bool) -> Result<()> {
    if !force && path.exists() {
        return Ok(());
    }
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// (Re)write the Claude hook scripts for the given workspace directory.
/// Pass `force = true` to overwrite existing scripts (e.g. after a tenx upgrade).
pub fn install_hooks(workspace_dir: &Path, force: bool) -> Result<()> {
    ensure_workspace_claude_settings_inner(workspace_dir, force)
}
