//! The detached workspace: where sessions that belong to no repo live — a
//! quick question (`tenx ask`), or an orchestrator that drives tasks in other
//! workspaces (`task send`/`wait`/`output`, the `/orchestrate` skill).
//!
//! It is an ordinary registered workspace with `kind = "detached"` and no
//! repos (see `workspace::DETACHED_KIND`), so the column, the watcher, sweep,
//! and secrets all treat its sessions as tasks without
//! knowing anything about it. What is special lives in three places: slugs
//! count up instead of colliding (`task::plan_slug`), the window is the agent
//! alone (`tmux::TaskWindow::agent_only`), and the agent may read every other
//! workspace (`task::readable_dirs`).

use anyhow::{bail, Result};
use std::path::Path;

use crate::workspace::{self, Workspace};

/// Create the detached workspace if it doesn't exist yet, register it, and
/// give it its skills and permission rules — idempotent, cheap when it's all
/// there, and run by every route into the session plus the commands that
/// create detached sessions, so it never needs a setup step.
pub fn ensure() -> Result<Workspace> {
    let global = workspace::load_global()?;
    let dir = workspace::detached_dir(&global)?;
    let ws = if dir.join("config.toml").is_file() {
        workspace::load(&dir)?
    } else {
        let mut ws = workspace::init(&dir, workspace::DETACHED_NAME)?;
        ws.config.kind = workspace::DETACHED_KIND.to_string();
        ws.save_config()?;
        ws
    };
    // The registry holds canonical paths; hand out the same one, or a task
    // created from here would carry a different path (and window stamp) than
    // the same task listed from the registry.
    let ws = match ws.dir.canonicalize() {
        Ok(canon) if canon != ws.dir => Workspace { dir: canon, config: ws.config },
        _ => ws,
    };
    if !ws.is_detached() {
        bail!(
            "{} holds a workspace that isn't tenx's detached one — set `detached_dir` in ~/.config/tenx/config.toml",
            dir.display()
        );
    }
    workspace::register_workspace(&ws.dir)?;
    // Skills are not opt-in here, unlike `tenx init`: the orchestrator is
    // half of what this workspace is for. Only missing files are written;
    // launch refreshes stale ones like any workspace's.
    crate::cli::init::install_skills(&ws.dir)?;
    write_settings(&ws.dir)?;
    // Trusting the root covers the trust dialog for every session under it;
    // `task new` still seeds each session directory for the permission rules.
    let _ = workspace::sessions::trust_dir(&ws.dir);
    Ok(ws)
}

/// [`ensure`] for a launch path that must not fail because of it (the client,
/// and `tenx web`): the detached workspace is a convenience, not a
/// precondition.
pub fn ensure_quiet() {
    if let Err(e) = ensure() {
        eprintln!("tenx: couldn't set up the detached workspace: {e:#}");
    }
}

/// The Bash commands a detached session may run without asking: reading
/// task state and driving other tasks' agents — what orchestrating *is* —
/// plus asking for a secret, which is always safe (it only enqueues).
/// Nothing that deletes: `task rm` still asks.
const ALLOWED: &[&str] = &[
    "Bash(tenx task list:*)",
    "Bash(tenx task new:*)",
    "Bash(tenx task send:*)",
    "Bash(tenx task wait:*)",
    "Bash(tenx task output:*)",
    "Bash(tenx ask:*)",
    "Bash(tenx secrets need:*)",
    "Bash(tenx secrets status:*)",
];

/// Seed the workspace's `.claude/settings.json` with [`ALLOWED`] — only when
/// the file doesn't exist, so a user's later edits are theirs to keep.
fn write_settings(ws_dir: &Path) -> Result<()> {
    let path = ws_dir.join(".claude/settings.json");
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(ws_dir.join(".claude"))?;
    let settings = serde_json::json!({ "permissions": { "allow": ALLOWED } });
    std::fs::write(&path, format!("{settings:#}\n"))?;
    Ok(())
}

/// `tenx ask <prompt>`: a new session titled after the question, with the
/// question as its first message. In the detached workspace unless `ws_dir`
/// names another, where it becomes a repo-less task that can read the whole
/// workspace. Prints the slug, which is what `task wait`/`output` take.
pub fn ask(prompt: &str, ws_dir: Option<&str>, agent: Option<crate::agent::AgentKind>, open: crate::cli::task::OpenMode) -> Result<()> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("ask what? — tenx ask \"<question>\"");
    }
    let ws = match ws_dir {
        Some(d) => crate::cli::task::load_ws_arg(d)?,
        None => ensure()?,
    };
    let mut title = tenx_core::orchestrate::ask_title(prompt);
    if workspace::slugify(&title).is_empty() {
        title = "question".to_string();
    }
    // An ordinary workspace refuses a repeated title (it is a branch name
    // there); a question asked twice shouldn't fail, so count up the title.
    if !ws.is_detached() {
        let base = title.clone();
        let mut n = 2;
        while crate::cli::task::plan_slug(&ws, &title).is_err() && n < 100 {
            title = format!("{base} {n}");
            n += 1;
        }
    }
    let md = crate::cli::task::TaskMd { prompt, ..Default::default() };
    let slug = crate::cli::task::new_with(&ws, &title, Some(&[]), open, &md, agent, &crate::progress::Silent)?;
    println!("{slug}");
    Ok(())
}
