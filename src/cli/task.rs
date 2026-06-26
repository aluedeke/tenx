use anyhow::{bail, Context, Result};
use std::env;
use std::io::{self, BufRead, Write};

pub fn new(name: &str, repos: Option<&[String]>, no_open: bool) -> Result<()> {
    let name = crate::workspace::slugify(name);
    let name = name.as_str();

    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    let global = crate::workspace::load_global()?;

    ws.check_task_new(name)?;

    // Determine which repos to include
    let repo_names: Vec<String> = match repos {
        Some(r) => r.to_vec(),
        None => ws.config.repos.iter().map(|r| r.name.clone()).collect(),
    };

    if repo_names.is_empty() {
        bail!("no repos in workspace — run: tenx repo add <url>");
    }

    let bare_dir = ws.bare_dir(&global);
    let task_dir = ws.tasks_dir().join(name);
    std::fs::create_dir_all(&task_dir)?;

    for repo_name in &repo_names {
        let repo = ws.find_repo(repo_name).ok_or_else(|| {
            crate::workspace::WorkspaceError::RepoNotFound(repo_name.clone())
        })?;
        let bare_path = crate::git::bare_repo_path(&bare_dir, &repo.name);
        crate::git::ensure_synced(&repo.url, &bare_dir, &repo.name)?;

        let worktree_path = task_dir.join(&repo.name);
        crate::git::add_worktree(&bare_path, &worktree_path, name)?;
    }

    if !no_open {
        if !crate::zellij::is_inside_session() {
            eprintln!("! not inside a zellij session — run: zellij");
            eprintln!("  to open later: tenx task open {}", name);
            return Ok(());
        }
        let layout = ws.config.layout.as_str();
        let opts = crate::zellij::TabOptions {
            name,
            cwd: &task_dir.to_string_lossy(),
            layout_file: if layout.is_empty() { None } else { Some(layout) },
        };
        crate::zellij::open_or_switch(&opts)?;
    }
    Ok(())
}

pub fn open(name: &str) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    let task = ws.find_task(name)?;

    if !crate::zellij::is_inside_session() {
        bail!("not inside a zellij session — start zellij first");
    }

    let layout = ws.config.layout.as_str();
    let opts = crate::zellij::TabOptions {
        name,
        cwd: &task.path.to_string_lossy(),
        layout_file: if layout.is_empty() { None } else { Some(layout) },
    };
    crate::zellij::open_or_switch(&opts)?;
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
