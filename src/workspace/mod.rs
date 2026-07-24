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
        Ok(content) => {
            let mut lines: Vec<&str> = content.lines().collect();
            let heading = format!("# {title}");
            if lines.first().map(|l| l.trim_start().starts_with('#')).unwrap_or(false) {
                lines[0] = heading.as_str();
                lines.join("\n") + "\n"
            } else {
                format!("{heading}\n{content}")
            }
        }
        Err(_) => format!("# {title}\n"),
    };
    fs::write(&path, new).with_context(|| format!("write {}", path.display()))
}

pub fn read_task_display_name(task_dir: &Path) -> String {
    if let Ok(content) = fs::read_to_string(task_dir.join("TASK.md")) {
        if let Some(first) = content.lines().next() {
            let title = first.trim_start_matches('#').trim().to_string();
            if !title.is_empty() {
                return title;
            }
        }
    }
    task_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The Claude-activity state of a task, mirrored from the zellij tab indicator.
/// Written by Claude Code hooks via `tenx tab event`, which maps each hook event
/// to one of these states (see `cli::tab::event`). Absent file → `Idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// A turn is in flight — Claude is working (UserPromptSubmit / tool calls).
    Working,
    /// Claude needs you: a permission prompt, idle prompt, or explicit
    /// needs-input notification (the 💬 indicator).
    Blocked,
    /// Claude finished a turn — the ball is in your court (Stop).
    Done,
    /// The turn ended on an API error (StopFailure).
    Failed,
    /// Session present but no active turn, or no signal yet.
    Idle,
}

impl TaskStatus {
    /// Parse the single token written to `.tenx-status`. Unknown/absent → `Idle`.
    fn from_token(token: &str) -> TaskStatus {
        match token {
            "working" => TaskStatus::Working,
            "blocked" => TaskStatus::Blocked,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            _ => TaskStatus::Idle,
        }
    }

    /// The wire token for this status (inverse of `from_token`); used by
    /// `tenx overlay --json` for the overlay plugin.
    pub fn token(self) -> &'static str {
        match self {
            TaskStatus::Working => "working",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Idle => "idle",
        }
    }
}

/// Read a task's `.tenx-status` file, returning the state and when it last
/// changed. Absent file → `Idle` with no timestamp.
pub fn read_task_status(task_dir: &Path) -> (TaskStatus, Option<SystemTime>) {
    let path = task_dir.join(".tenx-status");
    let modified = fs::metadata(&path).and_then(|m| m.modified()).ok();
    let status = match fs::read_to_string(&path) {
        Ok(s) => TaskStatus::from_token(s.trim()),
        Err(_) => TaskStatus::Idle,
    };
    (status, modified)
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

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME").map(PathBuf::from).context("$HOME not set")
}

/// Convert a user-supplied task name into a filesystem/branch-safe slug:
/// lowercase, spaces and underscores replaced with dashes.
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect()
}

pub fn format_age(t: SystemTime) -> String {
    let secs = t.elapsed().unwrap_or_default().as_secs();
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 86400 * 7 {
        format!("{}d", secs / 86400)
    } else if secs < 86400 * 30 {
        format!("{}w", secs / (86400 * 7))
    } else {
        format!("{}mo", secs / (86400 * 30))
    }
}

pub fn cloned_repos(bare_dir: &Path, repos: &[RepoConfig]) -> HashSet<String> {
    repos
        .iter()
        .filter(|r| bare_dir.join(format!("{}.git", r.name)).exists())
        .map(|r| r.name.clone())
        .collect()
}
