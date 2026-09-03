pub mod claude;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace config.toml not found (run: tenx init <name>)")]
    NotFound,
    #[error("workspace already exists")]
    AlreadyExists,
    #[error("task '{0}' already exists")]
    TaskExists(String),
    #[error("task '{0}' not found")]
    TaskNotFound(String),
    #[error("repo '{0}' already in workspace")]
    RepoExists(String),
    #[error("repo '{0}' not in workspace")]
    RepoNotFound(String),
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub bare_dir: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RepoConfig {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct WorkspaceConfig {
    pub name: String,
    #[serde(default)]
    pub layout: String,
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    /// Override the age identity used by `tenx secrets` for this workspace.
    /// Only needed for the edge case of a workspace-specific identity (e.g. a
    /// client's own key); the common case resolves it from the standard
    /// `SOPS_AGE_KEY_FILE` / `~/.config/sops/age` / `~/.config/age` chain
    /// instead and leaves this unset. See `cli::secrets::resolve_identity_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_identity: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub name: String,         // slug (directory name)
    pub display_name: String, // human-readable title from TASK.md
    pub path: PathBuf,
    pub repos: Vec<String>,
    pub branch: String,
    pub created_at: SystemTime,
}

pub struct Workspace {
    pub dir: PathBuf,
    pub config: WorkspaceConfig,
}

// ── Global config ─────────────────────────────────────────────────────────────

fn global_config_path() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(home.join(".config").join("tenx").join("config.toml"))
}

pub fn load_global() -> Result<GlobalConfig> {
    let path = global_config_path()?;
    if !path.exists() {
        return Ok(GlobalConfig::default());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read global config {}", path.display()))?;
    let cfg: GlobalConfig = toml::from_str(&text).context("parse global config")?;
    Ok(cfg)
}

// ── Workspace registry ────────────────────────────────────────────────────────
//
// A global list of all workspace directories, so tools that span workspaces
// (the overlay) can enumerate them without walking the filesystem. Written on
// `tenx init` and self-healed whenever a workspace is opened. Dead paths are
// pruned lazily on read.

/// One registry entry: a single file per workspace under `workspaces.d/`.
/// Registration is an atomic single-file write (no shared read-modify-write, so
/// concurrent tenx invocations can't lose each other's entries), and pruning is
/// a per-file delete. Room for per-workspace metadata later.
#[derive(Debug, Deserialize, Serialize)]
struct RegistryEntry {
    path: String,
}

fn registry_dir() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(home.join(".config").join("tenx").join("workspaces.d"))
}

/// Legacy single-file registry, imported into `workspaces.d/` on first read.
fn legacy_registry_path() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(home.join(".config").join("tenx").join("workspaces.toml"))
}

/// Registry filename for a workspace: slugified directory name plus a short
/// hash of the canonical path, so same-named workspaces don't collide.
fn registry_key(canon: &Path) -> String {
    let name = canon
        .file_name()
        .map(|n| slugify(&n.to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "workspace".to_string());
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a
    for b in canon.to_string_lossy().as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{name}-{:08x}", (hash >> 32) as u32)
}

/// Record `dir` in the global workspace registry (idempotent).
pub fn register_workspace(dir: &Path) -> Result<()> {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let reg_dir = registry_dir()?;
    fs::create_dir_all(&reg_dir).context("create tenx registry dir")?;
    let file = reg_dir.join(format!("{}.toml", registry_key(&canon)));
    let entry = RegistryEntry {
        path: canon.to_string_lossy().into_owned(),
    };
    atomic_write_toml(&file, &entry)
}

/// Import the legacy single-file registry into `workspaces.d/`, then delete it.
/// Best-effort; a partial import self-heals on the next workspace open.
fn migrate_legacy_registry() {
    let Ok(legacy) = legacy_registry_path() else { return };
    let Ok(text) = fs::read_to_string(&legacy) else { return };
    #[derive(Deserialize)]
    struct Legacy {
        #[serde(default)]
        paths: Vec<String>,
    }
    if let Ok(reg) = toml::from_str::<Legacy>(&text) {
        for p in &reg.paths {
            let _ = register_workspace(Path::new(p));
        }
    }
    let _ = fs::remove_file(&legacy);
}

/// Load every registered workspace, pruning entries that no longer resolve.
pub fn registered_workspaces() -> Vec<Workspace> {
    migrate_legacy_registry();
    let dir = match registry_dir() {
        Ok(d) => d,
        Err(_) => return vec![],
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return vec![];
    };
    let mut workspaces = Vec::new();
    for entry in entries.flatten() {
        let file = entry.path();
        if file.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let ws = fs::read_to_string(&file)
            .ok()
            .and_then(|t| toml::from_str::<RegistryEntry>(&t).ok())
            .and_then(|e| load(Path::new(&e.path)).ok());
        match ws {
            Some(ws) => workspaces.push(ws),
            // Unparseable entry or dead workspace path — prune the file.
            None => {
                let _ = fs::remove_file(&file);
            }
        }
    }
    workspaces.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    workspaces
}

// ── Workspace discovery ───────────────────────────────────────────────────────

/// Walk up from `dir` until a directory containing config.toml is found.
pub fn find(dir: &Path) -> Result<Workspace> {
    let mut cur = dir.canonicalize().context("canonicalize cwd")?;
    loop {
        let candidate = cur.join("config.toml");
        if candidate.exists() {
            return load(&cur);
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return Err(WorkspaceError::NotFound.into()),
        }
    }
}

/// Like `find`, but distinguishes "no enclosing workspace" (`Ok(None)`) from a
/// real error (unreadable/malformed `config.toml`). Used by the no-arg launch
/// path, which falls back to the global overlay when cwd isn't in a workspace.
pub fn find_opt(dir: &Path) -> Result<Option<Workspace>> {
    match find(dir) {
        Ok(ws) => Ok(Some(ws)),
        Err(e)
            if e.downcast_ref::<WorkspaceError>()
                .is_some_and(|w| matches!(w, WorkspaceError::NotFound)) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Load a workspace from an explicit directory.
pub fn load(dir: &Path) -> Result<Workspace> {
    let cfg_path = dir.join("config.toml");
    if !cfg_path.exists() {
        return Err(WorkspaceError::NotFound.into());
    }
    let text = fs::read_to_string(&cfg_path)
        .with_context(|| format!("read {}", cfg_path.display()))?;
    let config: WorkspaceConfig = toml::from_str(&text).context("parse workspace config")?;
    Ok(Workspace { dir: dir.to_path_buf(), config })
}

/// Create a workspace in `dir` with the given name.
/// If `dir` doesn't exist it is created; if it already has a config.toml, errors.
pub fn init(dir: &Path, name: &str) -> Result<Workspace> {
    if dir.join("config.toml").exists() {
        return Err(WorkspaceError::AlreadyExists.into());
    }
    fs::create_dir_all(dir.join("tasks")).context("create tasks dir")?;
    let config = WorkspaceConfig { name: name.to_string(), ..Default::default() };
    atomic_write_toml(&dir.join("config.toml"), &config)?;
    Ok(Workspace { dir: dir.to_path_buf(), config })
}

// ── Workspace methods ─────────────────────────────────────────────────────────

impl Workspace {
    pub fn save_config(&self) -> Result<()> {
        atomic_write_toml(&self.dir.join("config.toml"), &self.config)
    }

    pub fn add_repo(&mut self, repo: RepoConfig) -> Result<()> {
        if self.config.repos.iter().any(|r| r.name == repo.name) {
            return Err(WorkspaceError::RepoExists(repo.name).into());
        }
        self.config.repos.push(repo);
        self.save_config()
    }

    pub fn find_repo(&self, name: &str) -> Option<&RepoConfig> {
        self.config.repos.iter().find(|r| r.name == name)
    }

    pub fn tasks_dir(&self) -> PathBuf {
        self.dir.join("tasks")
    }

    /// Returns the effective bare directory.
    pub fn bare_dir(&self, global: &GlobalConfig) -> PathBuf {
        if !global.bare_dir.is_empty() {
            return PathBuf::from(expand_home(&global.bare_dir));
        }
        self.dir.join(".bare")
    }

    /// Discover all tasks from the filesystem.
    pub fn tasks(&self) -> Result<Vec<Task>> {
        let tasks_dir = self.tasks_dir();
        if !tasks_dir.exists() {
            return Ok(vec![]);
        }
        let mut tasks = Vec::new();
        for entry in fs::read_dir(&tasks_dir).context("read tasks dir")? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Ok(task) = discover_task(&entry.path()) {
                tasks.push(task);
            }
        }
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(tasks)
    }

    pub fn find_task(&self, name: &str) -> Result<Task> {
        let task_dir = self.tasks_dir().join(name);
        if !task_dir.is_dir() {
            return Err(WorkspaceError::TaskNotFound(name.to_string()).into());
        }
        discover_task(&task_dir)
    }

    pub fn check_task_new(&self, name: &str) -> Result<()> {
        if self.tasks_dir().join(name).exists() {
            bail!(WorkspaceError::TaskExists(name.to_string()));
        }
        Ok(())
    }
}

// ── Task discovery ────────────────────────────────────────────────────────────

fn discover_task(task_dir: &Path) -> Result<Task> {
    let name = task_dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let display_name = read_task_display_name(task_dir);
    let meta = fs::metadata(task_dir)?;
    let created_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    let mut repos = Vec::new();
    let mut branch = String::new();

    if let Ok(entries) = fs::read_dir(task_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if entry.path().join(".git").is_file() {
                if branch.is_empty() {
                    branch = read_branch(&entry.path()).unwrap_or_default();
                }
                repos.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    repos.sort();

    Ok(Task { name, display_name, path: task_dir.to_path_buf(), repos, branch, created_at })
}

/// Rewrite a task's display name — the first `# ` heading line of TASK.md.
/// Creates the file with just the heading if it doesn't exist.
pub fn set_task_title(task_dir: &Path, title: &str) -> Result<()> {
    let path = task_dir.join("TASK.md");
    let new = match fs::read_to_string(&path) {
        Ok(content) => tenx_core::taskmd::with_title(&content, title),
        Err(_) => format!("# {title}\n"),
    };
    fs::write(&path, new).with_context(|| format!("write {}", path.display()))
}

pub fn read_task_display_name(task_dir: &Path) -> String {
    fs::read_to_string(task_dir.join("TASK.md"))
        .ok()
        .and_then(|content| tenx_core::taskmd::display_name(&content))
        .unwrap_or_else(|| task_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

// The status model — `TaskStatus`, `TaskGroup`, `TaskState`, `Signal` and the
// resolution rule — is pure logic and lives in `tenx_core::status`
// (unit-tested there). Re-exported so call sites keep the `workspace::` path.
pub use tenx_core::status::{Signal, TaskGroup, TaskState, TaskStatus};

/// Per-task window signals keyed by slug, from `tmux::signals()`. Empty when
/// the server is down, which reads as "no bells" — correct, since no window
/// exists to ring one in.
pub type Signals = std::collections::HashMap<String, Signal>;

/// A task's state: Claude Code's sessions plus its window's bell, resolved by
/// `tenx_core::status::resolve_task_state`. Pass `sessions` and `signals`
/// gathered once per refresh, not per task.
pub fn resolve_task_state(task_dir: &Path, sessions: &[claude::Session], signals: &Signals) -> TaskState {
    let slug = task_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let signal = signals.get(&slug).copied().unwrap_or_default();
    tenx_core::status::resolve_task_state(task_dir, sessions, signal)
}

/// One task as the JSON shape every out-of-process consumer reads: the `tasks`
/// entries of `tenx overlay --json` (the overlay plugin) and the payload of the
/// `tenx::status` pipe (the status bar).
///
/// Shared rather than written out at each call site on purpose — two hand-kept
/// copies of a wire format is exactly how the overlay and its plugin drift, and
/// here the consumers are in another process and another language runtime, where
/// a silent mismatch surfaces as a missing field at runtime, not a compile error.
pub fn task_json(ws: &Workspace, task: &Task, state: &TaskState) -> serde_json::Value {
    let live = crate::live::read(&task.path);
    serde_json::json!({
        "ports": live.ports,
        "prs": live.prs.iter().map(|p| serde_json::json!({
            "repo": p.repo, "number": p.number, "state": p.state, "url": p.url,
            "draft": p.draft, "review": p.review, "checks": p.checks, "chip": p.chip(),
        })).collect::<Vec<_>>(),
        "ws": ws.config.name,
        "ws_dir": ws.dir,
        "slug": task.name,
        "title": task.display_name,
        "status": state.status.token(),
        "waiting_for": state.waiting_for,
        "sessions": state.sessions,
        "agents": state.agents,
        "age_secs": state.changed.and_then(|c| c.elapsed().ok()).map(|d| d.as_secs()),
        "repos": task.repos,
        "secrets_pending": secrets_pending(&task.path),
        "secrets_pending_set": secrets_pending_set(&task.path),
    })
}

/// Filename of a task's pending-secrets-*release* marker (see
/// `cli::secrets::enqueue_pending`, `decrypt`'s non-interactive fallback).
/// Defined here, not only in `cli::secrets`, because `task_json` — the wire
/// shape shared by the overlay and the status bar pipe — needs to read it
/// too, and `workspace` is the lower layer both depend on. Distinct from
/// `SECRETS_PENDING_SET_FILE` below: this one means "release something
/// already sealed"; that one means "someone needs to type in a value for
/// something that doesn't exist yet" — different fulfillment actions, so
/// they can't share one queue (see `cli::secrets::set`).
pub const SECRETS_PENDING_FILE: &str = ".secrets-pending";

/// Secret names currently pending unlock for `task_dir` (see
/// `cli::secrets::enqueue_pending`, `decrypt`'s non-interactive fallback),
/// newline-delimited in `SECRETS_PENDING_FILE`. Empty if there's no marker —
/// most tasks, most of the time.
pub fn secrets_pending(task_dir: &Path) -> Vec<String> {
    read_name_list(&task_dir.join(SECRETS_PENDING_FILE))
}

/// Filename of a task's pending-secrets-*set* marker — `set`'s non-interactive
/// fallback (an agent's Bash tool asking a human to supply a value for a
/// secret that doesn't exist yet). See `SECRETS_PENDING_FILE`'s doc comment
/// for why this is a separate file rather than folded into that one.
pub const SECRETS_PENDING_SET_FILE: &str = ".secrets-pending-set";

/// Secret names a human needs to supply a value for, newline-delimited in
/// `SECRETS_PENDING_SET_FILE`. Empty if there's no marker — most tasks, most
/// of the time.
pub fn secrets_pending_set(task_dir: &Path) -> Vec<String> {
    read_name_list(&task_dir.join(SECRETS_PENDING_SET_FILE))
}

fn read_name_list(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn read_branch(worktree_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(worktree_dir.join(".git")).ok()?;
    let gitdir_path = content.strip_prefix("gitdir: ")?.trim();
    let gitdir = if Path::new(gitdir_path).is_absolute() {
        PathBuf::from(gitdir_path)
    } else {
        worktree_dir.join(gitdir_path)
    };
    let head = fs::read_to_string(gitdir.join("HEAD")).ok()?;
    head.strip_prefix("ref: refs/heads/").map(|s| s.trim().to_string())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn atomic_write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).context("serialize toml")?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, &text).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// `pub(crate)`: also used by `cli::secrets` to resolve identity paths that may
/// be given with a `~/` prefix (e.g. a workspace's `age_identity` override).
pub(crate) fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// `pub(crate)`: also used by `cli::secrets` to locate the default
/// `~/.config/sops/age` / `~/.config/age` identity paths.
pub(crate) fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME").map(PathBuf::from).context("$HOME not set")
}

pub use tenx_core::slug::slugify;
pub use tenx_core::time::format_age;

pub fn cloned_repos(bare_dir: &Path, repos: &[RepoConfig]) -> HashSet<String> {
    repos
        .iter()
        .filter(|r| bare_dir.join(format!("{}.git", r.name)).exists())
        .map(|r| r.name.clone())
        .collect()
}
