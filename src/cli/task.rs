use anyhow::{bail, Context, Result};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::Path;

pub fn new(name: &str, repos: Option<&[String]>, no_open: bool) -> Result<()> {
    let display_name = name.to_string();
    let slug = crate::workspace::slugify(name);
    let slug = slug.as_str();

    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
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
        if !crate::zellij::is_inside_session() {
            eprintln!("! not inside a zellij session — run: zellij");
            eprintln!("  to open later: tenx task open {}", slug);
            return Ok(());
        }
        let layout = ws.config.layout.as_str();
        let opts = crate::zellij::TabOptions {
            name: &display_name,
            cwd: &task_dir.to_string_lossy(),
            workspace_dir: &ws.dir.to_string_lossy(),
            layout_file: if layout.is_empty() { None } else { Some(layout) },
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
    let task = ws.find_task(&slug)?;
    let display_name = read_display_name(&task.path);

    if !crate::zellij::is_inside_session() {
        bail!("not inside a zellij session — start zellij first");
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
    };
    let tab_id = crate::zellij::open_or_switch(&opts)?;
    std::fs::write(&tab_id_file, tab_id.to_string())?;
    Ok(())
}

pub fn list() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    let tasks = ws.tasks()?;

    // Try to get open tabs (silently ignore if not in session)
    let open_tabs: std::collections::HashSet<String> = if crate::zellij::is_inside_session() {
        crate::zellij::list_tabs()
            .map(|tabs| tabs.into_iter().map(|t| t.name).collect())
            .unwrap_or_default()
    } else {
        Default::default()
    };

    println!("{:<20} {:<25} {:<20} {:<6} {}", "NAME", "REPOS", "BRANCH", "AGE", "OPEN");
    println!("{}", "-".repeat(80));
    for task in &tasks {
        let repos = task.repos.join(", ");
        let age = crate::workspace::format_age(task.created_at);
        let open = if open_tabs.contains(&task.name) { "●" } else { "" };
        println!(
            "{:<20} {:<25} {:<20} {:<6} {}",
            task.name, repos, task.branch, age, open
        );
    }
    Ok(())
}

pub fn rm(name: &str, force: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
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
        if bare_path.exists() && worktree_path.exists() {
            // Best-effort: if the worktree was never registered (e.g. creation
            // failed mid-way), git remove will error but we still clean the dir.
            let _ = crate::git::remove_worktree(&bare_path, &worktree_path);
        }
    }

    std::fs::remove_dir_all(&task.path)
        .with_context(|| format!("remove task directory {}", task.path.display()))?;
    Ok(())
}

fn read_display_name(task_dir: &Path) -> String {
    if let Ok(content) = std::fs::read_to_string(task_dir.join("TASK.md")) {
        if let Some(first) = content.lines().next() {
            let title = first.trim_start_matches('#').trim().to_string();
            if !title.is_empty() {
                return title;
            }
        }
    }
    task_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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
    let claude_dir = workspace_dir.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let settings_path = claude_dir.join("settings.json");
    if !settings_path.exists() {
        let content = r#"{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/notify.sh"
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/notify-clear.sh"
          }
        ]
      }
    ]
  }
}
"#;
        std::fs::write(&settings_path, content)?;
    }

    write_hook_script(
        &hooks_dir.join("notify.sh"),
        "#!/bin/sh\n\
         [ -n \"$ZELLIJ\" ] || exit 0\n\
         TASK=$(head -1 \"$CLAUDE_PROJECT_DIR/TASK.md\" 2>/dev/null | sed 's/^# *//')\n\
         TASK=\"${TASK:-$(basename \"$CLAUDE_PROJECT_DIR\")}\"\n\
         [ -n \"$TASK\" ] || exit 0\n\
         ZELLIJ_BIN=$(command -v zellij 2>/dev/null)\n\
         if [ -z \"$ZELLIJ_BIN\" ]; then\n\
         \tfor p in \"$HOME/.local/bin\" /opt/homebrew/bin /usr/local/bin; do\n\
         \t\t[ -x \"$p/zellij\" ] && ZELLIJ_BIN=\"$p/zellij\" && break\n\
         \tdone\n\
         fi\n\
         [ -n \"$ZELLIJ_BIN\" ] || exit 0\n\
         TAB_ID=$(\"$ZELLIJ_BIN\" action list-tabs --json 2>/dev/null \\\n\
         \t| jq -r --arg n \"$TASK\" '.[] | select(.name == $n) | .tab_id' 2>/dev/null)\n\
         [ -n \"$TAB_ID\" ] || exit 0\n\
         \"$ZELLIJ_BIN\" action rename-tab --tab-id \"$TAB_ID\" \"\u{1F4AC} $TASK\" 2>/dev/null\n",
    )?;

    write_hook_script(
        &hooks_dir.join("notify-clear.sh"),
        "#!/bin/sh\n\
         [ -n \"$ZELLIJ\" ] || exit 0\n\
         TASK=$(head -1 \"$CLAUDE_PROJECT_DIR/TASK.md\" 2>/dev/null | sed 's/^# *//')\n\
         TASK=\"${TASK:-$(basename \"$CLAUDE_PROJECT_DIR\")}\"\n\
         [ -n \"$TASK\" ] || exit 0\n\
         ZELLIJ_BIN=$(command -v zellij 2>/dev/null)\n\
         if [ -z \"$ZELLIJ_BIN\" ]; then\n\
         \tfor p in \"$HOME/.local/bin\" /opt/homebrew/bin /usr/local/bin; do\n\
         \t\t[ -x \"$p/zellij\" ] && ZELLIJ_BIN=\"$p/zellij\" && break\n\
         \tdone\n\
         fi\n\
         [ -n \"$ZELLIJ_BIN\" ] || exit 0\n\
         TAB_ID=$(\"$ZELLIJ_BIN\" action list-tabs --json 2>/dev/null \\\n\
         \t| jq -r --arg n \"\u{1F4AC} $TASK\" '.[] | select(.name == $n) | .tab_id' 2>/dev/null)\n\
         [ -n \"$TAB_ID\" ] || exit 0\n\
         \"$ZELLIJ_BIN\" action rename-tab --tab-id \"$TAB_ID\" \"$TASK\" 2>/dev/null\n",
    )?;

    Ok(())
}

fn write_hook_script(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
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
