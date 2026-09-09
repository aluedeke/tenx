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

    // Register in the global workspace list so the column can find it.
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

    // Offer to install the /tenx and /standup skills. Claude Code reads
    // `.claude/skills`; Codex and pi read `.agents/skills` and `AGENTS.md`, so
    // install a portable copy of each and generate an AGENTS.md too.
    if prompt_yes_no("Install /tenx and /standup skills (Claude, Codex, pi) and AGENTS.md?")? {
        install_tenx_skill(&ws_dir)?;
        install_standup_skill(&ws_dir)?;
        install_agents_skill(&ws_dir, "tenx", TENX_SKILL_MD)?;
        install_agents_skill(&ws_dir, "standup", STANDUP_SKILL_MD)?;
        install_agents_md(&ws_dir)?;
        eprintln!("  ✓ skills installed (.claude/skills + .agents/skills) and AGENTS.md written");
    }

    // Offer to wire up session-state reporting for the agents on PATH, so tasks
    // show live status. This is the user-level, once-per-machine setup (the same
    // `tenx agent setup` does); tenx also self-heals it on launch.
    let found: Vec<crate::agent::AgentKind> = crate::agent::AgentKind::all()
        .into_iter()
        .filter(|k| crate::cli::session_event::agent_on_path(*k))
        .collect();
    if !found.is_empty() {
        let names: Vec<&str> = found.iter().map(|k| k.as_str()).collect();
        if prompt_yes_no(&format!("Set up tenx status reporting for {}?", names.join(", ")))? {
            for kind in found {
                let _ = crate::cli::session_event::setup(kind.as_str(), false);
            }
        }
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

/// Install a portable copy of a skill into `.agents/skills/<name>/SKILL.md` —
/// the location Codex and pi read (Claude uses `.claude/skills`). Portable
/// means an Agent-Skills `name:` header and no Claude-only dynamic command
/// injection; see [`portable_skill`].
fn install_agents_skill(ws_dir: &std::path::Path, name: &str, src: &str) -> Result<()> {
    let skill_dir = ws_dir.join(".agents").join("skills").join(name);
    std::fs::create_dir_all(&skill_dir)?;
    let skill_path = skill_dir.join("SKILL.md");
    if skill_path.exists() {
        return Ok(());
    }
    std::fs::write(&skill_path, portable_skill(name, src))?;
    Ok(())
}

/// Rewrite a Claude skill into the portable Agent-Skills shape: a `name:` +
/// `description:` header (dropping `allowed-tools`, which not every agent
/// honours) and no `` !`command` `` dynamic injection (a Claude-only feature —
/// replaced with a plain instruction to run the command).
fn portable_skill(name: &str, src: &str) -> String {
    let (description, body) = split_frontmatter(src);
    let mut out = format!("---\nname: {name}\ndescription: {description}\n---\n");
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("!`") {
            // The injected form is a shell one-liner (`… 2>/dev/null || echo …`);
            // keep just the command a person would run.
            let full = rest.split('`').next().unwrap_or(rest);
            let cmd = full.split(" 2>").next().unwrap_or(full).split(" ||").next().unwrap_or(full).trim();
            out.push_str(&format!("Run `{cmd}` to see the current list.\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Split a skill's YAML frontmatter from its body, returning `(description,
/// body)`. Tolerant: a file without frontmatter yields an empty description and
/// the whole text as body.
fn split_frontmatter(src: &str) -> (String, &str) {
    let rest = match src.strip_prefix("---\n") {
        Some(r) => r,
        None => return (String::new(), src),
    };
    let Some(end) = rest.find("\n---") else {
        return (String::new(), src);
    };
    let front = &rest[..end];
    let body = rest[end..].trim_start_matches('\n').trim_start_matches("---").trim_start_matches('\n');
    let description = front
        .lines()
        .find_map(|l| l.strip_prefix("description:").map(|d| d.trim().to_string()))
        .unwrap_or_default();
    (description, body)
}

/// Write the workspace `AGENTS.md` — the cross-agent context file Codex reads
/// from a task's cwd and pi walks up to find (Claude reads it too). Points at
/// the tenx skill and states the task-boundary rule up front.
fn install_agents_md(ws_dir: &std::path::Path) -> Result<()> {
    let path = ws_dir.join("AGENTS.md");
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, AGENTS_MD)?;
    Ok(())
}

const AGENTS_MD: &str = r#"# Working in this tenx workspace

This directory is a **tenx** workspace: one or more bare git repos, and a
`tasks/` directory where each subdirectory is a task with its own git worktrees,
a `TASK.md`, and a coding-agent window.

## Boundaries

Your working area is the current task directory (`tasks/<name>/`). Only modify
files inside it — the code in its `<repo>/` worktrees and its `TASK.md` —
without explicit user approval. Do **not** touch `config.toml`, `.bare/`, the
shared agent config directories, or any other task's directory. If a task needs
something outside these boundaries (adding a repo, deleting a task, changing
shared config), stop and ask the user.

## Keep TASK.md current

Check off `## Todo` items as you finish them; add the PR URL under `## Links`
after `gh pr create`; record decisions and gotchas under `## Notes`.

## Commands

    tenx task list             list all tasks and open windows
    tenx task new "<title>"    create a task (worktrees + TASK.md)
    tenx task open <name>      switch to a task's window
    tenx secrets decrypt <n>   ask for a credential (safe to run; enqueues a request)
    tenx secrets status        show sealed/unlocked/pending state

The `/tenx` skill (in `.agents/skills/tenx`) has the full detail on tasks,
tickets, and secrets.
"#;

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
