use anyhow::{Context, Result};
use std::env;
use std::io::{self, BufRead, Write};

pub fn run(name: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let (ws_dir, ws_name) = match name {
        None => {
            // No name: work in cwd, use cwd folder name as workspace name
            let n = cwd
                .file_name()
                .context("current directory has no name")?
                .to_string_lossy()
                .into_owned();
            (cwd.clone(), n)
        }
        Some(n) => {
            // Name given: create a new subdirectory
            (cwd.join(n), n.to_string())
        }
    };

    eprintln!("Initializing workspace '{ws_name}'");
    eprintln!();

    // Prompt for repos
    let repos = prompt_repos()?;

    // Prompt for layout file
    let layout = prompt_layout()?;

    // Create workspace and fill in config
    let mut ws = crate::workspace::init(&ws_dir, &ws_name)?;
    ws.config.layout = layout;
    for repo in repos {
        ws.config.repos.push(repo);
    }
    ws.save_config()?;

    // Register in the global workspace list so the overlay can find it.
    crate::workspace::register_workspace(&ws.dir)?;

    // Clone all repos immediately
    if !ws.config.repos.is_empty() {
        let global = crate::workspace::load_global()?;
        let bare_dir = ws.bare_dir(&global);
        eprintln!();
        eprintln!("Syncing repos:");
        for repo in &ws.config.repos {
            let verb = if crate::git::bare_repo_path(&bare_dir, &repo.name).exists() {
                "fetching"
            } else {
                "cloning"
            };
            let spinner = crate::progress::Spinner::new(format!("{verb} {}", repo.name));
            match crate::git::ensure_synced(&repo.url, &bare_dir, &repo.name) {
                Ok(_) => spinner.done(),
                Err(e) => {
                    spinner.fail(&e.to_string());
                    return Err(e);
                }
            }
        }
    }

    // Offer to install the /tenx skill into .claude/skills/
    if prompt_yes_no("Install /tenx skill for Claude Code sessions?")? {
        install_tenx_skill(&ws_dir)?;
        install_standup_skill(&ws_dir)?;
        eprintln!("  ✓ skills installed — type /tenx or /standup in any task session");
    }

    eprintln!();
    eprintln!("✓ workspace '{}' created at {}", ws.config.name, ws.dir.display());
    if ws.dir != cwd {
        eprintln!("  cd {}", ws.dir.display());
    }
    Ok(())
}

fn prompt_yes_no(question: &str) -> Result<bool> {
    let answer = prompt(&format!("{question} [Y/n]"))?;
    Ok(!answer.eq_ignore_ascii_case("n"))
}

fn install_tenx_skill(ws_dir: &std::path::Path) -> Result<()> {
    let skill_dir = ws_dir.join(".claude").join("skills").join("tenx");
    std::fs::create_dir_all(&skill_dir)?;
    let skill_path = skill_dir.join("SKILL.md");
    if skill_path.exists() {
        return Ok(());
    }
    std::fs::write(&skill_path, TENX_SKILL_MD)?;
    Ok(())
}

const TENX_SKILL_MD: &str = include_str!("skills/tenx.md");

fn install_standup_skill(ws_dir: &std::path::Path) -> Result<()> {
    let skill_dir = ws_dir.join(".claude").join("skills").join("standup");
    std::fs::create_dir_all(&skill_dir)?;
    let skill_path = skill_dir.join("SKILL.md");
    if !skill_path.exists() {
        std::fs::write(&skill_path, STANDUP_SKILL_MD)?;
    }
    Ok(())
}

const STANDUP_SKILL_MD: &str = r#"---
description: Generate a daily standup report from yesterday's Claude Code activity and workspace task files. Use when the user asks for a standup, daily summary, or what was done yesterday.
allowed-tools: Bash
---

Generate a standup report for yesterday.

## Step 1 — collect data

```bash
tenx standup
```

This outputs two sections: `=== TASK FILES ===` (authoritative source for PR links, Linear tickets, descriptions) and `=== ACTIVITY LOG (since <timestamp>) ===` (user prompts and git commits since the last standup).

To override the period: `tenx standup --since 2026-06-29T00:00:00Z`

## Step 2 — generate the standup

Using the output, produce this format:

**Achieved:**

For each task that had activity:
> #### <Task name>
> PR: <links from task file, or "none">
> Linear: <ticket from task file, or "none">
> 1-2 sentences only. What was completed or meaningfully progressed.

**Planned today:**
One sentence per task inferred from open todos, unresolved threads, or incomplete work.

**Blockers:**
One sentence per blocker. Failed commands, stuck design decisions, unanswered questions.

Rules:
- PR and Linear lines are mandatory for every task — write "none" if absent, never omit.
- Maximum 2 sentences per task in the achieved section.
- Skip tasks with no meaningful activity (tool noise only).

## Step 3 — log it

Prepend the standup to `daily.local.md`, directly after the `<!-- last-standup: ... -->` marker on line 1. Use this format:

```
<!-- last-standup: ... -->

---
## <YYYY-MM-DD HH:MM>

<standup content>

<existing content below>
```

Read the current `daily.local.md` first, then write the updated version with the new entry at the top.
"#;

fn prompt_repos() -> Result<Vec<crate::workspace::RepoConfig>> {
    let mut repos = Vec::new();
    eprintln!("Repos (enter a git URL per line, empty line to finish):");
    loop {
        let url = prompt("  URL")?;
        if url.is_empty() {
            break;
        }
        let default_name = infer_name(&url);
        let input = prompt(&format!("  Name [{default_name}]"))?;
        let name = if input.is_empty() { default_name } else { input };
        repos.push(crate::workspace::RepoConfig { name, url });
    }
    Ok(repos)
}

fn prompt_layout() -> Result<String> {
    eprintln!("Layout script for task windows (optional, enter for the built-in claude/nvim/shell layout).");
    eprintln!("It runs with TENX_WINDOW, TENX_SLUG, TENX_TASK_DIR, TENX_CLAUDE_CMD and TENX_TMUX set:");
    loop {
        let input = prompt("  Layout script path")?;
        if input.is_empty() {
            return Ok(input);
        }
        match crate::workspace::check_layout(&input) {
            Ok(()) => return Ok(input),
            Err(e) => eprintln!("  ! {e} — try again, or enter for the built-in layout"),
        }
    }
}

fn prompt(label: &str) -> Result<String> {
    let mut stdout = io::stdout();
    write!(stdout, "{label}: ")?;
    stdout.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn infer_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string()
}
