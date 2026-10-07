//! The adhoc workspace: where sessions that belong to no repo live — a
//! quick question (`tenx ask`), or an orchestrator that drives tasks in other
//! workspaces (`task send`/`wait`/`output`, the `/orchestrate` skill).
//!
//! It is an ordinary registered workspace with `kind = "adhoc"` and no
//! repos (see `workspace::ADHOC_KIND`), so the column, the watcher, sweep,
//! and secrets all treat its sessions as tasks without
//! knowing anything about it. What is special lives in three places: slugs
//! count up instead of colliding (`task::plan_slug`), the window is the agent
//! alone (`tmux::TaskWindow::agent_only`), and the agent may read every other
//! workspace (`task::readable_dirs`).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::workspace::{self, Workspace};

/// Create the adhoc workspace if it doesn't exist yet, register it, and
/// give it its skills and permission rules — idempotent, cheap when it's all
/// there, and run by every route into the session plus the commands that
/// create adhoc sessions, so it never needs a setup step.
pub fn ensure() -> Result<Workspace> {
    let global = workspace::load_global()?;
    if global.adhoc_dir.is_empty() {
        let base = workspace::home_dir()?.join(".local/share/tenx");
        if let Err(e) = move_legacy_dir(&base.join("detached"), &base.join("adhoc")) {
            eprintln!("tenx: couldn't move the old detached workspace: {e:#}");
        }
    }
    let dir = workspace::adhoc_dir(&global)?;
    let ws = if dir.join("config.toml").is_file() {
        let mut ws = workspace::load(&dir)?;
        upgrade_legacy(&mut ws)?;
        ws
    } else {
        let mut ws = workspace::init(&dir, workspace::ADHOC_NAME)?;
        ws.config.kind = workspace::ADHOC_KIND.to_string();
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
    if !ws.is_adhoc() {
        bail!(
            "{} holds a workspace that isn't tenx's adhoc one — set `adhoc_dir` in ~/.config/tenx/config.toml",
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

/// Rewrite an adhoc workspace an older release made (`kind`, and the name
/// it was given, "detached") as one this release makes — wherever it is:
/// `adhoc_dir` in the global config, or not moved yet ([`move_legacy_dir`]).
fn upgrade_legacy(ws: &mut Workspace) -> Result<()> {
    if ws.config.kind != workspace::LEGACY_ADHOC_KIND {
        return Ok(());
    }
    ws.config.kind = workspace::ADHOC_KIND.to_string();
    if ws.config.name == workspace::LEGACY_ADHOC_KIND {
        ws.config.name = workspace::ADHOC_NAME.to_string();
    }
    ws.save_config()
}

/// Move the adhoc workspace an older release made at `from` (the *detached*
/// one, `~/.local/share/tenx/detached`) to `to`, and each task's Claude and
/// pi transcripts with it — both agents file them under the task's path, so
/// left behind, a reopened task would start a new conversation instead of
/// resuming. Codex records the path inside its threads; those start fresh.
///
/// Not while any of its tasks has a window: the agent in it would be left
/// in a directory that moved, and tenx would lose track of the window. Until
/// a launch finds them all closed, `workspace::adhoc_dir` keeps using `from`.
/// The registry entry for `from` is pruned once the path is gone; [`ensure`]
/// registers `to`. A no-op once moved, or when there is nothing at `from`.
fn move_legacy_dir(from: &Path, to: &Path) -> Result<()> {
    if !from.join("config.toml").is_file() || to.exists() {
        return Ok(());
    }
    let from = from.canonicalize()?;
    let open = crate::tmux::list_windows()?
        .into_iter()
        .filter_map(|w| w.task_dir)
        .filter(|d| d.starts_with(&from))
        .count();
    if open > 0 {
        eprintln!("tenx: the old detached workspace moves to {} once its {open} open window(s) are closed", to.display());
        return Ok(());
    }
    std::fs::rename(&from, to).with_context(|| format!("move {} to {}", from.display(), to.display()))?;
    let to = to.canonicalize()?;
    let slugs: Vec<String> = std::fs::read_dir(to.join("tasks"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let home = workspace::home_dir()?;
    for (old, new) in transcript_moves(&from, &to, &slugs, &home) {
        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
    eprintln!("tenx: moved the detached workspace to {}", to.display());
    Ok(())
}

/// Where Claude (`~/.claude/projects`) and pi (`~/.pi/agent/sessions`) keep
/// the transcripts of the workspace at `from` and each of its tasks, paired
/// with where they belong once it is at `to`.
fn transcript_moves(from: &Path, to: &Path, slugs: &[String], home: &Path) -> Vec<(PathBuf, PathBuf)> {
    use tenx_core::transcript::{claude_project_dirname, pi_session_dirname};
    let dirs = std::iter::once(PathBuf::new()).chain(slugs.iter().map(|s| Path::new("tasks").join(s)));
    let mut moves = Vec::new();
    for rel in dirs {
        let (old, new) = (from.join(&rel), to.join(&rel));
        let (old, new) = (old.to_string_lossy(), new.to_string_lossy());
        let claude = home.join(".claude/projects");
        moves.push((claude.join(claude_project_dirname(&old)), claude.join(claude_project_dirname(&new))));
        let pi = home.join(".pi/agent/sessions");
        moves.push((pi.join(pi_session_dirname(&old)), pi.join(pi_session_dirname(&new))));
    }
    moves
}

/// [`ensure`] for a launch path that must not fail because of it (the client,
/// and `tenx web`): the adhoc workspace is a convenience, not a
/// precondition.
pub fn ensure_quiet() {
    if let Err(e) = ensure() {
        eprintln!("tenx: couldn't set up the adhoc workspace: {e:#}");
    }
}

/// The Bash commands an adhoc session may run without asking: reading
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
/// question as its first message. In the adhoc workspace unless `ws_dir`
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
    if !ws.is_adhoc() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One a release up to 0.2.3 made is rewritten as this release's; a
    /// name the user chose survives, and a current one is left alone.
    #[test]
    fn upgrades_a_legacy_detached_workspace() {
        let dir = std::env::temp_dir().join(format!("tenx-adhoc-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut ws = workspace::init(&dir, "detached").unwrap();
        ws.config.kind = workspace::LEGACY_ADHOC_KIND.to_string();
        ws.save_config().unwrap();

        let mut ws = workspace::load(&dir).unwrap();
        assert!(ws.is_adhoc(), "a legacy one counts before the upgrade");
        upgrade_legacy(&mut ws).unwrap();
        let ws = workspace::load(&dir).unwrap();
        assert_eq!((ws.config.kind.as_str(), ws.config.name.as_str()), (workspace::ADHOC_KIND, workspace::ADHOC_NAME));

        let mut ws = ws;
        ws.config.kind = workspace::LEGACY_ADHOC_KIND.to_string();
        ws.config.name = "scratch".into();
        upgrade_legacy(&mut ws).unwrap();
        let ws = workspace::load(&dir).unwrap();
        assert_eq!((ws.config.kind.as_str(), ws.config.name.as_str()), (workspace::ADHOC_KIND, "scratch"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each task's transcripts follow it, under both agents' encodings.
    #[test]
    fn transcripts_move_with_their_tasks() {
        let (from, to) = (Path::new("/h/.local/share/tenx/detached"), Path::new("/h/.local/share/tenx/adhoc"));
        let moves = transcript_moves(from, to, &["msal".into()], Path::new("/h"));
        assert_eq!(moves.len(), 4, "workspace and task, Claude and pi");
        assert!(moves.contains(&(
            PathBuf::from("/h/.claude/projects/-h--local-share-tenx-detached-tasks-msal"),
            PathBuf::from("/h/.claude/projects/-h--local-share-tenx-adhoc-tasks-msal"),
        )));
        assert!(moves.contains(&(
            PathBuf::from("/h/.pi/agent/sessions/--h-.local-share-tenx-detached-tasks-msal--"),
            PathBuf::from("/h/.pi/agent/sessions/--h-.local-share-tenx-adhoc-tasks-msal--"),
        )));
    }
}
