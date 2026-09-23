use anyhow::{Context, Result};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use tenx_core::skills::{skill_state, SkillState};

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

    // Offer the /tenx and /standup skills. Claude Code reads
    // `.claude/skills`; Codex and pi read `.agents/skills` and `AGENTS.md`, so
    // a portable copy of each is installed and an AGENTS.md generated. Asked
    // before anything is written, so creation is the one step `init_in`
    // (shared with the column's new-workspace form).
    let skills = prompt_yes_no("Install /tenx and /standup skills (Claude, Codex, pi) and AGENTS.md?")?;

    if !repos.is_empty() {
        eprintln!();
        eprintln!("Syncing repos:");
    }
    let ws = init_in(&ws_dir, &ws_name, repos, layout, skills, crate::progress::for_cli().as_ref())?;
    if skills {
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

/// Create a workspace from settled inputs — what `tenx init` does once its
/// questions are answered, and what the column's new-workspace form calls
/// directly: the directory with `config.toml` and `tasks/`, the registry
/// entry (every running column lists it on its next refresh), the repos
/// cloned, and the skills when asked for. The per-machine agent setup is
/// not part of it: `tenx` self-heals that on launch.
pub fn init_in(
    dir: &Path,
    name: &str,
    repos: Vec<crate::workspace::RepoConfig>,
    layout: String,
    skills: bool,
    rep: &dyn crate::progress::Reporter,
) -> Result<crate::workspace::Workspace> {
    let mut ws = crate::workspace::init(dir, name)?;
    ws.config.layout = layout;
    ws.config.repos = repos;
    ws.save_config()?;

    // Register in the global workspace list so the column can find it.
    crate::workspace::register_workspace(&ws.dir)?;

    // Clone all repos immediately, one reported step each, exactly as task
    // creation does. Where that lands — a line on the CLI's stdout, a panel in
    // the column — is the reporter's business, not this function's.
    if !ws.config.repos.is_empty() {
        use crate::progress::Event;
        let global = crate::workspace::load_global()?;
        let bare_dir = ws.bare_dir(&global);
        for (step, repo) in ws.config.repos.iter().enumerate() {
            let exists = crate::git::bare_repo_path(&bare_dir, &repo.name).exists();
            rep.emit(Event::Start {
                step,
                label: repo.name.clone(),
                verb: crate::git::Synced::verb(exists),
            });
            let _lock = crate::git::lock_repo(&bare_dir, &repo.name)?;
            let mut on = |snap| rep.emit(Event::Update { step, snap });
            match crate::git::ensure_synced(&repo.url, &bare_dir, &repo.name, &mut on) {
                Ok(synced) => rep.emit(Event::Done { step, note: synced.note().to_string() }),
                Err(e) => {
                    rep.emit(Event::Failed { step, err: e.to_string() });
                    return Err(e);
                }
            }
        }
    }

    if skills {
        install_skills(&ws.dir)?;
    }
    Ok(ws)
}

fn install_skills(ws_dir: &Path) -> Result<()> {
    for (path, content) in skill_files(ws_dir) {
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
    }
    Ok(())
}

/// Every file `install_skills` writes, with what this binary writes there:
/// the `/tenx` and `/standup` skills for Claude (`.claude/skills`) and in
/// the portable shape Codex and pi read (`.agents/skills`, see
/// [`portable_skill`]), and `AGENTS.md`. The one list installing,
/// refreshing and `doctor` all work from.
fn skill_files(ws_dir: &Path) -> Vec<(PathBuf, String)> {
    vec![
        (ws_dir.join(".claude/skills/tenx/SKILL.md"), TENX_SKILL_MD.to_string()),
        (ws_dir.join(".claude/skills/standup/SKILL.md"), STANDUP_SKILL_MD.to_string()),
        (ws_dir.join(".agents/skills/tenx/SKILL.md"), portable_skill("tenx", TENX_SKILL_MD)),
        (ws_dir.join(".agents/skills/standup/SKILL.md"), portable_skill("standup", STANDUP_SKILL_MD)),
        (ws_dir.join("AGENTS.md"), AGENTS_MD.to_string()),
    ]
}

/// Bring a workspace's installed skills up to date (`tenx_core::skills` has
/// the rule): only files that exist are looked at — installing them is
/// `tenx init`'s opt-in — a stale one is rewritten, an edited one is left
/// alone. Returns each file's state as found, for `doctor`. Best-effort: a
/// file that can't be rewritten stays `Stale` in the result.
pub fn refresh_skills(ws_dir: &Path) -> Vec<(PathBuf, SkillState, bool)> {
    let mut found = Vec::new();
    for (path, current) in skill_files(ws_dir) {
        let Ok(installed) = std::fs::read_to_string(&path) else { continue };
        let state = skill_state(&installed, &current, SHIPPED_SKILLS);
        let updated = state == SkillState::Stale && std::fs::write(&path, &current).is_ok();
        found.push((path, state, updated));
    }
    found
}

/// [`refresh_skills`] over every registered workspace — what launching
/// `tenx` does, so no workspace keeps instructions from an older tenx.
pub fn refresh_all_skills() {
    for ws in crate::workspace::registered_workspaces() {
        refresh_skills(&ws.dir);
    }
}

/// Replace edited skill files with the current version, keeping each
/// edited copy beside it as `<name>.orig` — `tenx doctor --reset-skills`,
/// the explicit way to take tenx's version when [`refresh_skills`] won't.
/// Returns the files replaced.
pub fn reset_skills(ws_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut replaced = Vec::new();
    for (path, current) in skill_files(ws_dir) {
        let Ok(installed) = std::fs::read_to_string(&path) else { continue };
        if skill_state(&installed, &current, SHIPPED_SKILLS) != SkillState::Edited {
            continue;
        }
        let mut orig = path.clone().into_os_string();
        orig.push(".orig");
        std::fs::write(&orig, &installed).with_context(|| format!("write {}", PathBuf::from(&orig).display()))?;
        std::fs::write(&path, &current).with_context(|| format!("write {}", path.display()))?;
        replaced.push(path);
    }
    Ok(replaced)
}

/// `tenx_core::skills::content_hash` of every rendering of a file in
/// [`skill_files`] that any tenx release has written, current ones included
/// — what lets [`refresh_skills`] tell tenx's own untouched words from a
/// user's edit. Append the new hash whenever a skill, `portable_skill` or
/// `AGENTS_MD` changes; `every_current_rendering_is_listed_as_shipped` fails
/// with the value to add. Never remove one.
const SHIPPED_SKILLS: &[u64] = &[
    0xda48c7c75191a270, // agentsmd 24fee45
    0x21c9ce83220c1e00, // standup 24fee45
    0x17bbde9e628b2a0e, // standup 24fee45 portable
    0x91bec1c8a7cfa9ba, // tenx 24fee45
    0x44966dc4a48f5483, // tenx 24fee45 portable
    0x0dfe5ed7bfd21fd0, // tenx 13ff6d5
    0x14641f697a0fdcb7, // tenx 13ff6d5 portable
    0x0415f72b19da1b80, // tenx 1ee4604
    0xf177b1144376d83b, // tenx 1ee4604 portable
    0x9deb20dda2c11565, // tenx 1cd5a06
    0xbaa310a7e30c139a, // tenx 1cd5a06 portable
    0x375325c5482f13e4, // tenx a39877a
    0x4f247643abb13a03, // tenx a39877a portable
    0xc5f80b81422f14db, // tenx a9dd131
    0xa2995fb62ec036cc, // tenx a9dd131 portable
    0xd58823fadaacde70, // tenx 83522c4
    0x67998069f4b8013d, // tenx 83522c4 portable
    0xc067dc7d58158936, // tenx 6cf43da
    0x1f8afba826ad6f99, // tenx 6cf43da portable
    0xb6dd288de89a9683, // tenx: `need --why`
    0x980d762777468b70, // tenx: `need --why` portable
    0x646c555ee28e1303, // agentsmd: `need`
];

fn prompt_yes_no(question: &str) -> Result<bool> {
    let answer = prompt(&format!("{question} [Y/n]"))?;
    Ok(!answer.eq_ignore_ascii_case("n"))
}

const TENX_SKILL_MD: &str = include_str!("skills/tenx.md");

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
    tenx secrets need <n> --why "…"   ask for a credential (safe to run; waits for a human)
    tenx secrets status        show sealed/unlocked/pending state

The `/tenx` skill (in `.agents/skills/tenx`) has the full detail on tasks,
tickets, and secrets.
"#;

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
        let default_name = crate::cli::repo::infer_name(&url);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tenx_core::skills::content_hash;

    #[test]
    fn every_current_rendering_is_listed_as_shipped() {
        // Without its hash here, the next release couldn't recognise today's
        // files as its own, and would leave them stale forever.
        let missing: Vec<String> = skill_files(Path::new("/ws"))
            .into_iter()
            .map(|(path, content)| (path, content_hash(content.as_bytes())))
            .filter(|(_, hash)| !SHIPPED_SKILLS.contains(hash))
            .map(|(path, hash)| format!("    0x{hash:016x}, // {}", path.display()))
            .collect();
        assert!(missing.is_empty(), "skills changed — add to SHIPPED_SKILLS:\n{}", missing.join("\n"));
    }

    #[test]
    fn refresh_replaces_stale_files_and_keeps_edited_ones() {
        let ws = std::env::temp_dir().join(format!("tenx-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        let files = skill_files(&ws);
        let (stale, _) = &files[0];
        let (edited, _) = &files[4];
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        // A rendering tenx shipped before: its hash is in the list.
        let old = "an old skill\n";
        assert!(!SHIPPED_SKILLS.contains(&content_hash(old.as_bytes())));
        std::fs::write(stale, old).unwrap();
        std::fs::write(edited, "my own AGENTS.md\n").unwrap();

        // Not shipped → both edited, nothing touched; missing files not created.
        let found = refresh_skills(&ws);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|(_, s, updated)| *s == SkillState::Edited && !updated));
        assert!(!files[1].0.exists());

        // Reset takes tenx's version and keeps the edited copy.
        let replaced = reset_skills(&ws).unwrap();
        assert_eq!(replaced.len(), 2);
        assert_eq!(std::fs::read_to_string(edited).unwrap(), files[4].1);
        assert_eq!(std::fs::read_to_string(ws.join("AGENTS.md.orig")).unwrap(), "my own AGENTS.md\n");
        assert!(refresh_skills(&ws).iter().all(|(_, s, _)| *s == SkillState::Current));
        std::fs::remove_dir_all(&ws).unwrap();
    }

    #[test]
    fn a_shipped_old_version_is_refreshed() {
        let ws = std::env::temp_dir().join(format!("tenx-skills-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        let (path, current) = skill_files(&ws).swap_remove(0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // The /tenx skill as it shipped before `need` existed.
        let old = current.replace("tenx secrets need", "tenx secrets decrypt");
        std::fs::write(&path, &old).unwrap();
        let shipped: Vec<u64> = SHIPPED_SKILLS.iter().copied().chain([content_hash(old.as_bytes())]).collect();
        assert_eq!(skill_state(&old, &current, &shipped), SkillState::Stale);
        std::fs::remove_dir_all(&ws).unwrap();
    }
}
