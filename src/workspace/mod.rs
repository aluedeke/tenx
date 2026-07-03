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

pub fn save_global(cfg: &GlobalConfig) -> Result<()> {
    let path = global_config_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    atomic_write_toml(&path, cfg)
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
