use anyhow::{bail, Context, Result};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What to pre-fill in a new task's `TASK.md` beyond its title — a ticket
/// import passes the ticket's body and URL(s); the overlay passes nothing.
#[derive(Default)]
pub struct TaskMd<'a> {
    pub description: &'a str,
    /// `(label, value)` rows for `## Links`; see `tenx_core::taskmd::merge_links`.
    pub links: &'a [(String, String)],
}

pub fn new(name: &str, repos: Option<&[String]>, no_open: bool, md: &TaskMd) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    new_with(&ws, name, repos, no_open, md)
}

/// Create a task in an explicit workspace (no cwd dependency), with an empty
/// `TASK.md` body. Used by the overlay's create flow.
pub fn new_in(ws: &crate::workspace::Workspace, name: &str, repos: Option<&[String]>, no_open: bool) -> Result<()> {
    new_with(ws, name, repos, no_open, &TaskMd::default())
}

/// Create a task in an explicit workspace with a pre-filled `TASK.md`.
pub fn new_with(
    ws: &crate::workspace::Workspace,
    name: &str,
    repos: Option<&[String]>,
    no_open: bool,
    md: &TaskMd,
) -> Result<()> {
    let display_name = name.to_string();
    let slug = crate::workspace::slugify(name);
    let slug = slug.as_str();
    if slug.is_empty() {
        bail!("task name {name:?} has no letters or digits to make a slug from");
    }

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
    write_task_md(&task_dir, &display_name, md)?;
    write_claude_hooks(&task_dir)?;

    for repo_name in &repo_names {
        ensure_repo_worktree(ws, &bare_dir, &task_dir, repo_name, slug)?;
    }

    if !no_open {
        if !crate::tmux::server_running() {
            eprintln!("! the '{}' session isn't running — run: tenx", crate::tmux::SESSION);
            eprintln!("  to open later: tenx task open {}", slug);
            return Ok(());
        }
        let layout = ws.config.layout.as_str();
        let opts = crate::tmux::TaskWindow {
            // Name the window by the immutable slug, not the editable title.
            // Window names are never shown (tabless) and only correlate
            // task↔window; the slug can't drift or collide.
            slug,
            title: name,
            task_dir: &task_dir.to_string_lossy(),
            workspace_dir: &ws.dir.to_string_lossy(),
            layout_script: if layout.is_empty() { None } else { Some(layout) },
            resume: false, // brand-new task — no conversation to continue
        };
        let id = crate::tmux::open_task_window(&opts)?;
        std::fs::write(task_dir.join(crate::tmux::WINDOW_ID_FILE), &id)?;
    }
    Ok(())
}

/// Clone-or-fetch one repo and create the task's worktree for it, on a branch
/// named after the task slug.
///
/// Shared by task creation and `task add-repo`, so a repo that joins a task
/// later is set up exactly like one that was there from the start: a fresh
/// branch off that repo's freshly-fetched default branch. Branch namespaces are
/// per-repo, so joining late costs nothing.
fn ensure_repo_worktree(
    ws: &crate::workspace::Workspace,
    bare_dir: &Path,
    task_dir: &Path,
    repo_name: &str,
    slug: &str,
) -> Result<()> {
    let repo = ws
        .find_repo(repo_name)
        .ok_or_else(|| crate::workspace::WorkspaceError::RepoNotFound(repo_name.to_string()))?;

    let bare_path = crate::git::bare_repo_path(bare_dir, &repo.name);
    let verb = if bare_path.exists() { "fetching" } else { "cloning" };
    let spinner = crate::progress::Spinner::new(format!("{verb} {}", repo.name));
    match crate::git::ensure_synced(&repo.url, bare_dir, &repo.name) {
        Ok(_) => spinner.done(),
        Err(e) => { spinner.fail(&e.to_string()); return Err(e); }
    }

    let worktree_path = task_dir.join(&repo.name);
    let spinner = crate::progress::Spinner::new(format!("worktree {}", repo.name));
    match crate::git::add_worktree(&bare_path, &worktree_path, slug) {
        Ok(_) => spinner.done(),
        Err(e) => { spinner.fail(&e.to_string()); return Err(e); }
    }
    Ok(())
}

/// Add worktrees for `repos` to an existing task (cwd's workspace, or `ws_dir`).
pub fn add_repo(ws_dir: Option<&str>, task: &str, repos: &[String]) -> Result<()> {
    let (ws, slug) = resolve_task(ws_dir, task)?;
    add_repo_in(&ws, &slug, repos)
}

/// Add worktrees for `repos` to an existing task. Idempotent: repos the task
/// already has are skipped, so callers can pass a whole desired set without
/// diffing it first.
pub fn add_repo_in(ws: &crate::workspace::Workspace, slug: &str, repos: &[String]) -> Result<()> {
    let global = crate::workspace::load_global()?;
    let task = ws.find_task(slug)?;
    let bare_dir = ws.bare_dir(&global);
    for name in repos {
        if task.repos.iter().any(|r| r == name) {
            continue;
        }
        // task.name is the slug, i.e. the branch name the other worktrees use.
        ensure_repo_worktree(ws, &bare_dir, &task.path, name, &task.name)?;
    }
    Ok(())
}

/// Detach `repos` from an existing task (cwd's workspace, or `ws_dir`).
pub fn rm_repo(ws_dir: Option<&str>, task: &str, repos: &[String], force: bool) -> Result<()> {
    let (ws, slug) = resolve_task(ws_dir, task)?;
    rm_repo_in(&ws, &slug, repos, force)
}

/// Remove `repos`' worktrees from an existing task, along with their task
/// branch — the same cleanup `task rm` does per repo, so a repo that leaves a
/// task doesn't leave a stale branch behind (and a later re-add starts fresh
/// off the default branch, which is what `add_worktree -B` would force anyway).
///
/// Without `force`, git refuses to remove a worktree with uncommitted changes;
/// that refusal is the safety net, so the overlay never passes force.
/// Idempotent: repos the task doesn't have are skipped.
pub fn rm_repo_in(
    ws: &crate::workspace::Workspace,
    slug: &str,
    repos: &[String],
    force: bool,
) -> Result<()> {
    let global = crate::workspace::load_global()?;
    let task = ws.find_task(slug)?;
    let bare_dir = ws.bare_dir(&global);
    for name in repos {
        if !task.repos.iter().any(|r| r == name) {
            continue;
        }
        let bare_path = crate::git::bare_repo_path(&bare_dir, name);
        let worktree_path = task.path.join(name);
        let spinner = crate::progress::Spinner::new(format!("detaching {name}"));
        let res = crate::git::remove_worktree(&bare_path, &worktree_path, force)
            .and_then(|()| crate::git::delete_branch(&bare_path, &task.name));
        match res {
            Ok(()) => spinner.done(),
            Err(e) => { spinner.fail(&e.to_string()); return Err(e); }
        }
    }
    Ok(())
}

/// Reconcile a task's worktrees to exactly `repos`: add what's missing, detach
/// what's extra. One command for a whole desired state, so the overlay can
/// apply a checklist without sequencing two async invocations.
///
/// Additions run first: if one fails (a clone can), the repos the task already
/// had are still intact rather than half-detached.
pub fn set_repos(ws_dir: Option<&str>, task: &str, repos: &[String], force: bool) -> Result<()> {
    let (ws, slug) = resolve_task(ws_dir, task)?;
    set_repos_in(&ws, &slug, repos, force)
}

pub fn set_repos_in(
    ws: &crate::workspace::Workspace,
    slug: &str,
    repos: &[String],
    force: bool,
) -> Result<()> {
    if repos.is_empty() {
        bail!("a task must keep at least one repo — delete the task instead");
    }
    let task = ws.find_task(slug)?;
    let add: Vec<String> = repos
        .iter()
        .filter(|r| !task.repos.contains(r))
        .cloned()
        .collect();
    let remove: Vec<String> = task
        .repos
        .iter()
        .filter(|r| !repos.contains(r))
        .cloned()
        .collect();
    add_repo_in(ws, slug, &add)?;
    rm_repo_in(ws, slug, &remove, force)
}

/// Resolve a workspace (explicit dir, else cwd) and a task slug. An explicit
/// `ws_dir` means the caller is a UI passing an exact slug; from cwd the name is
/// slugified, matching how `task open` treats it.
fn resolve_task(ws_dir: Option<&str>, task: &str) -> Result<(crate::workspace::Workspace, String)> {
    match ws_dir {
        Some(dir) => Ok((crate::workspace::load(Path::new(dir))?, task.to_string())),
        None => Ok((
            crate::workspace::find(&env::current_dir()?)?,
            crate::workspace::slugify(task),
        )),
    }
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

/// Create a task in an explicit workspace directory and open a window for it
/// (for scripts and front ends). `repos` is the picked subset (None = all).
pub fn new_by_dir(ws_dir: &str, name: &str, repos: Option<&[String]>, md: &TaskMd) -> Result<()> {
    let ws = crate::workspace::load(Path::new(ws_dir))?;
    new_with(&ws, name, repos, false, md)
}

/// Delete a task by explicit workspace directory and exact slug (no prompt).
/// Used by the overlay plugin, which does its own confirmation.
pub fn rm_by_dir(ws_dir: &str, slug: &str) -> Result<()> {
    let ws = crate::workspace::load(Path::new(ws_dir))?;
    rm_in(&ws, slug, true)
}

/// Rename a task's display title. Only rewrites TASK.md — the zellij tab is
/// named by the immutable slug (not the title), so there's nothing to keep in
/// sync. `ws_dir` selects the workspace (cwd if None). The header pane and the
/// overlay both read the title from TASK.md, so the new title shows up at once.
pub fn rename(ws_dir: Option<&str>, slug: &str, title: &str) -> Result<()> {
    let ws = match ws_dir {
        Some(dir) => crate::workspace::load(Path::new(dir))?,
        None => crate::workspace::find(&env::current_dir()?)?,
    };
    let task = ws.find_task(slug)?;
    crate::workspace::set_task_title(&task.path, title)?;
    Ok(())
}

/// Focus a task's window in the tenx session (creating it if needed), given an
/// explicit workspace and slug. Used by `open` and the overlay, neither of
/// which can rely on cwd matching the task. Works from any client of the tenx
/// server, and from outside it as long as the server is up — `select-window`
/// changes the *session's* current window, which is what an attaching client
/// lands on.
pub fn open_in(ws: &crate::workspace::Workspace, slug: &str) -> Result<()> {
    let task = ws.find_task(slug)?;

    if !crate::tmux::server_running() {
        bail!("the '{}' session isn't running — run 'tenx' to start it first", crate::tmux::SESSION);
    }

    // Correlate to a live window by its name == the task SLUG. Slugs are
    // immutable and unique, so this never drifts (unlike the title). Window
    // names aren't shown anywhere (tabless), so using the slug costs nothing.
    let id_file = task.path.join(crate::tmux::WINDOW_ID_FILE);
    if let Some(w) = crate::tmux::find_window(slug)? {
        crate::tmux::select_window(&w.id)?;
        // Refresh the cached id to the live one.
        let _ = std::fs::write(&id_file, &w.id);
        return Ok(());
    }

    // Not open — create it (named by slug) and record the new id.
    let layout = ws.config.layout.as_str();
    let opts = crate::tmux::TaskWindow {
        slug,
        title: &task.display_name,
        task_dir: &task.path.to_string_lossy(),
        workspace_dir: &ws.dir.to_string_lossy(),
        layout_script: if layout.is_empty() { None } else { Some(layout) },
        // Only `--continue` if claude actually has a conversation for this cwd;
        // otherwise it exits 1 and the pane vanishes.
        resume: has_claude_conversation(&task.path),
    };
    let id = crate::tmux::open_task_window(&opts)?;
    std::fs::write(&id_file, &id)?;
    Ok(())
}

/// Whether claude has stored a conversation for `cwd` (so `--continue` will
/// resume instead of exiting 1). Claude encodes each project dir as its path
/// with `/` → `-` under `~/.claude/projects/`.
fn has_claude_conversation(cwd: &Path) -> bool {
    let Some(project_dir) = crate::workspace::claude::project_dir(cwd) else {
        return false;
    };
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

    // Open windows in the tenx session (empty if it isn't running). Works from
    // anywhere: it's a server query, not a client one.
    let open_tabs: std::collections::HashSet<String> = crate::tmux::list_windows()
        .map(|ws| ws.into_iter().map(|w| w.name).collect())
        .unwrap_or_default();

    println!("{:<20} {:<25} {:<20} {:<6} OPEN", "NAME", "REPOS", "BRANCH", "AGE");
    println!("{}", "-".repeat(80));
    for task in &tasks {
        let repos = task.repos.join(", ");
        let age = crate::workspace::format_age(task.created_at);
        // Tabs are named by slug (task.name), not the display title.
        let open = if open_tabs.contains(&task.name) { "●" } else { "" };
        println!(
            "{:<20} {:<25} {:<20} {:<6} {}",
            task.display_name, repos, task.branch, age, open
        );
    }
    Ok(())
}

// ── Pin / sweep ───────────────────────────────────────────────────────────────
//
// A task's window (a running `claude` process plus its panes) stays resident
// forever once opened — nothing ever closes it automatically, so a workspace
// touched over months accumulates a window per task ever opened, most of them
// long since abandoned. `sweep` reclaims that: close what's safe to close,
// leave everything else exactly as it was. Reopening (`task open`, or the
// overlay) is unaffected — `open_in`'s `find_window` just finds none and
// creates a fresh one, and `has_claude_conversation` still finds the prior
// transcript, so `--continue` picks the conversation back up.
//
// Deliberately writes no new per-task state beyond `PINNED_FILE` (an explicit,
// user-requested opt-out): "is this window safe to close" is answered
// entirely from *live* sources — `resolve_task_state` (Claude Code's own
// session registry) for whether a task is mid-turn or waiting on a prompt, and
// the tmux server itself for whether a window is open and which one is current.
// The rule itself is `tenx_core::sweep::sweep_reason`.

/// Marker file: this task's tab is never auto-swept, no matter how long it's
/// idle. Plain and empty, same style as `.tenx-tab-id`.
const PINNED_FILE: &str = ".tenx-pinned";

fn is_pinned(task_dir: &Path) -> bool {
    task_dir.join(PINNED_FILE).is_file()
}

/// Exempt a task from `sweep`.
pub fn pin(ws_dir: Option<&str>, task: &str) -> Result<()> {
    let (ws, slug) = resolve_task(ws_dir, task)?;
    let task = ws.find_task(&slug)?;
    std::fs::write(task.path.join(PINNED_FILE), "")?;
    println!("pinned '{}' — sweep will never close its tab", task.display_name);
    Ok(())
}

/// Undo `pin`.
pub fn unpin(ws_dir: Option<&str>, task: &str) -> Result<()> {
    let (ws, slug) = resolve_task(ws_dir, task)?;
    let task = ws.find_task(&slug)?;
    let _ = std::fs::remove_file(task.path.join(PINNED_FILE));
    println!("unpinned '{}'", task.display_name);
    Ok(())
}

/// Parse a plain "<N><unit>" duration — "30m", "4h", "2d". The parser lives in
/// `tenx_core::time` (unit-tested); this only maps its error into anyhow.
pub fn parse_duration(s: &str) -> Result<Duration> {
    tenx_core::time::parse_duration(s).map_err(|e| anyhow::anyhow!(e))
}

pub use tenx_core::sweep::DEFAULT_SWEEP_AFTER;

/// A task tab `sweep` (or the overlay's background sweep) has decided is safe
/// to close, and why — computed once, then either printed+closed (`sweep`) or
/// just closed (`sweep_quiet`, which can't print without corrupting the
/// overlay's alternate screen).
pub struct SweepAction {
    pub ws_name: String,
    pub title: String,
    pub window_id: String,
    pub task_dir: PathBuf,
    pub reason: String,
}

/// Every task window, across every registered workspace, that's safe to close
/// right now. Never includes: a window that isn't open, the session's current
/// window, a pinned task, or a task that's `Blocked`/`Working` — those are
/// exactly the windows a prompt or an agent is waiting on. Doesn't close
/// anything itself, so a caller can dry-run, summarize, or act on the list.
pub fn sweep_candidates(after: Duration) -> Vec<SweepAction> {
    let mut out = Vec::new();
    let live = crate::tmux::list_windows().unwrap_or_default();
    if live.is_empty() {
        return out; // session isn't running — nothing open to sweep
    }
    let sessions = crate::workspace::claude::sessions();
    let signals: crate::workspace::Signals = live
        .iter()
        .map(|w| (w.name.clone(), crate::workspace::Signal { bell: w.bell, activity: w.activity }))
        .collect();
    for ws in crate::workspace::registered_workspaces() {
        for task in ws.tasks().unwrap_or_default() {
            // Windows are named by slug, which is only unique *within* a
            // workspace (see `Workspace::check_task_new`) — a name collision
            // across two workspaces is a pre-existing ambiguity `open_in`'s
            // `find_window` shares, not something sweep introduces.
            let Some(w) = live.iter().find(|w| w.name == task.name) else {
                continue; // not open
            };
            let state = crate::workspace::resolve_task_state(&task.path, &sessions, &signals);
            let input = tenx_core::sweep::SweepInput {
                status: state.status,
                changed: state.changed,
                active: w.active,
                pinned: is_pinned(&task.path),
            };
            let Some(reason) = tenx_core::sweep::sweep_reason(&input, after, std::time::SystemTime::now()) else {
                continue;
            };
            out.push(SweepAction {
                ws_name: ws.config.name.clone(),
                title: task.display_name.clone(),
                window_id: w.id.clone(),
                task_dir: task.path.clone(),
                reason,
            });
        }
    }
    out
}

/// `tenx task sweep`: close every current sweep candidate (or, with
/// `dry_run`, just report them), printing one line per task.
pub fn sweep(after: Option<Duration>, dry_run: bool) -> Result<()> {
    let candidates = sweep_candidates(after.unwrap_or(DEFAULT_SWEEP_AFTER));
    if candidates.is_empty() {
        println!("nothing to sweep");
        return Ok(());
    }
    for c in candidates {
        if dry_run {
            println!("would close {}/{} — {}", c.ws_name, c.title, c.reason);
            continue;
        }
        match crate::tmux::kill_window(&c.window_id) {
            Ok(()) => {
                let _ = std::fs::remove_file(c.task_dir.join(crate::tmux::WINDOW_ID_FILE));
                println!("closed {}/{} — {}", c.ws_name, c.title, c.reason);
            }
            Err(e) => eprintln!("! {}/{}: {e}", c.ws_name, c.title),
        }
    }
    Ok(())
}

/// Close every current sweep candidate without printing anything — for the
/// overlay's background sweep on focus-gained, which runs inside the
/// alternate screen and would corrupt it by writing to stdout. Returns how
/// many windows it actually closed.
pub fn sweep_quiet(after: Duration) -> usize {
    let mut n = 0;
    for c in sweep_candidates(after) {
        if crate::tmux::kill_window(&c.window_id).is_ok() {
            let _ = std::fs::remove_file(c.task_dir.join(crate::tmux::WINDOW_ID_FILE));
            n += 1;
        }
    }
    n
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
                let _ = crate::git::remove_worktree(&bare_path, &worktree_path, true);
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

fn write_task_md(task_dir: &Path, name: &str, md: &TaskMd) -> Result<()> {
    let path = task_dir.join("TASK.md");
    if path.exists() {
        return Ok(());
    }
    let links = tenx_core::taskmd::merge_links(md.links);
    let content = tenx_core::taskmd::render_task_md(name, md.description, &links);
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
    remove_tenx_hooks(workspace_dir)
}

/// Strip tenx's Claude Code hooks from a workspace.
///
/// tenx installs **no hooks at all** any more. Every task state it shows is read
/// live from Claude Code's own session registry (`workspace::claude`), so the
/// `.claude/hooks/event.sh` → `tenx tab event` → `.tenx-status` pipeline has no
/// remaining job. This function is what retires it, and it runs from both
/// `task new` and `tenx hooks install` so a workspace converges whichever it
/// meets first.
///
/// It removes only what tenx wrote:
///
/// - hook entries in `.claude/settings.json` whose command points at our
///   `event.sh` — foreign hooks the user added are left in place, and the
///   `hooks` key is dropped entirely only when nothing survives the filter.
/// - our own scripts under `.claude/hooks/`.
/// - stale `.tenx-status` files in the workspace's tasks. Nothing reads them
///   now; leaving them would be litter that looks meaningful.
///
/// The `.claude` directory itself stays — `tenx init` also installs skills
/// there, and the per-task symlink to it is still how Claude Code finds them.
fn remove_tenx_hooks(workspace_dir: &Path) -> Result<()> {
    let claude_dir = workspace_dir.join(".claude");
    let hooks_dir = claude_dir.join("hooks");

    let settings_path = claude_dir.join("settings.json");
    if let Some(mut settings) = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        && let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut())
    {
        hooks.retain(|_, matchers| !mentions_tenx_hook(matchers));
        let empty = hooks.is_empty();
        if empty {
            settings.as_object_mut().map(|o| o.remove("hooks"));
        }
        std::fs::write(&settings_path, format!("{:#}\n", settings))?;
    }

    for script in ["event.sh", "notify.sh", "notify-clear.sh"] {
        let _ = std::fs::remove_file(hooks_dir.join(script));
    }
    // Only if we emptied it — a user script in there is not ours to delete.
    let _ = std::fs::remove_dir(&hooks_dir);

    if let Ok(entries) = std::fs::read_dir(workspace_dir.join("tasks")) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path().join(".tenx-status"));
        }
    }
    Ok(())
}

/// Whether a settings.json hook entry runs tenx's `event.sh`.
fn mentions_tenx_hook(matchers: &serde_json::Value) -> bool {
    matchers.to_string().contains("hooks/event.sh")
}

/// Retire tenx's Claude Code hooks in the given workspace (see
/// `remove_tenx_hooks`). Kept under the `hooks install` command name so an
/// existing workspace has one obvious thing to run after upgrading.
pub fn install_hooks(workspace_dir: &Path, _force: bool) -> Result<()> {
    remove_tenx_hooks(workspace_dir)
}
