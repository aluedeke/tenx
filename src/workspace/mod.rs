pub mod sessions;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};
use tenx_core::registry;
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
    /// Default coding agent across all workspaces (`claude`, `codex`, `pi`);
    /// empty = `claude`. A workspace's `agent` and a task's `.tenx-agent`
    /// override it (`agent::agent_for`).
    #[serde(default)]
    pub agent: String,
    /// Width of the task column beside the session, in cells; 0 =
    /// automatic (`tenx_core::column::width`).
    #[serde(default, alias = "sidebar_width")]
    pub column_width: u16,
    /// How task windows are laid out when a workspace doesn't say:
    /// `tabs` (empty) or `split`; see `WorkspaceConfig::layout`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
    /// Per-agent launch overrides, keyed by agent token (`claude`/`codex`/`pi`).
    /// A workspace's `[agents.<kind>]` overrides the same key here.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub agents: HashMap<String, AgentConfig>,
    /// Where the adhoc workspace lives (`~` expanded); empty =
    /// `~/.local/share/tenx/adhoc`. See [`adhoc_dir`]. Read under its old
    /// name, `detached_dir`, too.
    #[serde(default, alias = "detached_dir", skip_serializing_if = "String::is_empty")]
    pub adhoc_dir: String,
    /// `tenx web`'s microphone: the model `tenx-whisper` runs.
    #[serde(default, skip_serializing_if = "SpeechConfig::is_empty")]
    pub speech: SpeechConfig,
    /// `tenx web`'s speaker: the model and voice `tenx-tts` reads with.
    #[serde(default, skip_serializing_if = "ReadAloudConfig::is_empty")]
    pub read_aloud: ReadAloudConfig,
}

/// `[read_aloud]`: text to speech for `tenx web`'s speaker (`web::tts`).
/// Without a `model` the speaker says how to set one.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct ReadAloudConfig {
    /// A Kokoro model directory as sherpa-onnx packs it
    /// (`kokoro-multi-lang-v1_0`; `~` expanded).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// A Kokoro voice by name (`af_heart`, `bm_george`) or number; empty =
    /// `af_heart` (`tenx_core::tts::VOICES`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice: String,
    /// How fast it reads: 1.0 (or 0) is the voice's own pace.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub speed: f32,
    /// Minutes unused before the model is unloaded; 0 = 10.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub idle_minutes: u32,
}

impl ReadAloudConfig {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn is_zero_f32(n: &f32) -> bool {
    *n == 0.0
}

/// `[speech]`: speech to text for `tenx web` (`web::whisper`). Without a
/// `model` the microphone says how to set one.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct SpeechConfig {
    /// A whisper.cpp `ggml-*.bin` model (`~` expanded).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Words the model should expect — names, jargon — as Whisper's initial
    /// prompt, from this file (`~` expanded).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_file: String,
    /// The language spoken when the page doesn't say: a Whisper code (`en`,
    /// `de`) or `auto` (the default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// Minutes unused before the model is unloaded; 0 = 10.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub idle_minutes: u32,
}

impl SpeechConfig {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// How to launch one agent: an alternate binary and/or extra arguments. Lets a
/// user point at a wrapper or pin a model without tenx growing a flag per agent.
/// tenx still adds the per-agent session/resume args around this.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AgentConfig {
    /// Executable to run instead of the agent's default (`claude`/`codex`/`pi`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Extra arguments appended to the launch command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RepoConfig {
    pub name: String,
    pub url: String,
}

/// The config shape this binary writes. Bump it with a new entry in
/// `migrate_config` whenever a field's meaning changes, not just when one is
/// added (serde defaults cover additions).
pub const CURRENT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WorkspaceConfig {
    /// Which `CURRENT_SCHEMA` wrote this file; 0 = predates versioning.
    #[serde(default)]
    pub schema_version: u32,
    pub name: String,
    /// Empty for an ordinary workspace; [`ADHOC_KIND`] for the one tenx
    /// creates itself to hold sessions that belong to no repo (`tenx ask`,
    /// an orchestrator). See [`Workspace::is_adhoc`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// How a new task window is laid out (`tenx_core::tabs::layout`):
    /// `tabs` (the agent, with a shell and `TASK.md` as tabs), `split` (the
    /// agent beside nvim and a shell), or an executable that lays it out
    /// (see `tmux::TaskWindow`). Empty = the global `layout`, else `tabs`.
    #[serde(default)]
    pub layout: String,
    /// Default coding agent for this workspace's tasks (`claude`, `codex`,
    /// `pi`). Empty = `claude`. A task overrides it with a `.tenx-agent` file
    /// (`agent::agent_for`).
    #[serde(default)]
    pub agent: String,
    /// Per-agent launch overrides (`[agents.codex] command = …, args = […]`),
    /// keyed by agent token; overrides the global config's same key.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub agents: HashMap<String, AgentConfig>,
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

#[derive(Clone)]
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

// ── The adhoc workspace ───────────────────────────────────────────────────────
//
// Sessions that belong to no repo — a quick question, an orchestrator driving
// tasks in several workspaces — are ordinary tasks in one workspace tenx owns:
// no repos, a single-pane window, and slugs that never collide (a repeated
// title gets `-2`, since nothing here is a branch name). Being a real,
// registered workspace is the point: windows, status, bells, sweep, secrets
// and every front end work on it unchanged. `cli::adhoc::ensure` creates
// and registers it.
//
// Releases up to 0.2.3 called it the *detached* workspace: `kind` and `name`
// "detached", at `~/.local/share/tenx/detached`. `cli::adhoc::ensure` moves
// one made then to the new place, transcripts and all, at the first launch
// that finds none of its tasks open, and upgrades its config.

/// `WorkspaceConfig::kind` of the adhoc workspace.
pub const ADHOC_KIND: &str = "adhoc";
/// Its `config.toml` name — what the column shows where a workspace name goes.
pub const ADHOC_NAME: &str = "adhoc";
/// [`ADHOC_KIND`] and [`ADHOC_NAME`] as releases up to 0.2.3 wrote them.
pub const LEGACY_ADHOC_KIND: &str = "detached";

/// Where the adhoc workspace lives: the global `adhoc_dir`, else
/// `~/.local/share/tenx/adhoc` — or `~/.local/share/tenx/detached` when only
/// that one exists, made by an older release and not moved yet.
pub fn adhoc_dir(global: &GlobalConfig) -> Result<PathBuf> {
    if !global.adhoc_dir.is_empty() {
        return Ok(PathBuf::from(expand_home(&global.adhoc_dir)));
    }
    let base = home_dir()?.join(".local/share/tenx");
    let (dir, legacy) = (base.join("adhoc"), base.join("detached"));
    if !dir.join("config.toml").is_file() && legacy.join("config.toml").is_file() {
        return Ok(legacy);
    }
    Ok(dir)
}

// ── Workspace registry ────────────────────────────────────────────────────────
//
// A global list of all workspace directories, so tools that span workspaces
// (the column) can enumerate them without walking the filesystem. Written on
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

/// Record `dir` in the global workspace registry (idempotent: a directory
/// already registered, under any spelling of its path, is left alone, so
/// calling this on every command is a read, not a write).
pub fn register_workspace(dir: &Path) -> Result<()> {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let reg_dir = registry_dir()?;
    let file = reg_dir.join(format!("{}.toml", registry_key(&canon)));
    let entry = RegistryEntry {
        path: canon.to_string_lossy().into_owned(),
    };
    if registered_paths(&reg_dir).iter().any(|p| Path::new(p).canonicalize().is_ok_and(|p| p == canon)) {
        return Ok(());
    }
    fs::create_dir_all(&reg_dir).context("create tenx registry dir")?;
    atomic_write_toml(&file, &entry)
}

/// Every path the registry holds, as written.
fn registered_paths(reg_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(reg_dir) else { return vec![] };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|f| f.extension().is_some_and(|e| e == "toml"))
        .filter_map(|f| fs::read_to_string(f).ok())
        .filter_map(|t| toml::from_str::<RegistryEntry>(&t).ok())
        .map(|e| e.path)
        .collect()
}

/// Register the workspace enclosing `cwd`, if there is one. The registry is
/// pruned of paths that no longer resolve, so a workspace that was moved
/// (`mv`) drops out of every column; any command run from inside it puts it
/// back. Best-effort: a failure here never stops the command.
pub fn register_enclosing(cwd: &Path) {
    if let Ok(Some(ws)) = find_opt(cwd) {
        let _ = register_workspace(&ws.dir);
    }
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
    let mut seen = std::collections::HashSet::new();
    for entry in entries.flatten() {
        let file = entry.path();
        if file.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let entry = fs::read_to_string(&file)
            .ok()
            .and_then(|t| toml::from_str::<RegistryEntry>(&t).ok());
        let (ws, outcome) = match entry {
            None => (None, registry::Load::BadEntry),
            Some(e) => load_within(Path::new(&e.path), LOAD_LIMIT),
        };
        match ws {
            // Two entries for one directory (one written by hand, or by
            // another spelling of its path) list it once.
            Some(ws) if !seen.insert(ws.dir.canonicalize().unwrap_or_else(|_| ws.dir.clone())) => {}
            Some(ws) => workspaces.push(ws),
            None if registry::prune(outcome) => {
                let _ = fs::remove_file(&file);
            }
            None => {}
        }
    }
    workspaces.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    workspaces
}

/// How long `registered_workspaces` waits for one workspace's `config.toml`.
/// A local read takes well under a millisecond; this is for one that never
/// returns, so a single workspace can't hold up every list of tasks.
const LOAD_LIMIT: Duration = Duration::from_secs(1);

/// Workspace directories whose `load` is still running on a thread
/// `load_within` gave up waiting for.
static LOADING: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// `load(dir)`, given up after `limit`. macOS holds a background process's
/// `open` of a file in iCloud Drive until its privacy permission is answered
/// — for `tenx web` as a LaunchAgent, a dialog nobody may be looking at — so
/// the read runs on a thread of its own and is left to finish when it's
/// slow. While it hasn't, the directory is skipped at once, so a stuck read
/// costs one thread, not one per call.
fn load_within(dir: &Path, limit: Duration) -> (Option<Workspace>, registry::Load) {
    let dir = dir.to_path_buf();
    if !LOADING.lock().unwrap_or_else(|e| e.into_inner()).insert(dir.clone()) {
        return (None, registry::Load::Slow);
    }
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new().name("workspace-load".into()).spawn({
        let dir = dir.clone();
        move || {
            let result = load(&dir);
            LOADING.lock().unwrap_or_else(|e| e.into_inner()).remove(&dir);
            let _ = tx.send(result);
        }
    });
    if spawned.is_err() {
        LOADING.lock().unwrap_or_else(|e| e.into_inner()).remove(&dir);
        return (load(&dir).ok(), registry::Load::Unreadable);
    }
    match rx.recv_timeout(limit) {
        Ok(Ok(ws)) => (Some(ws), registry::Load::Loaded),
        Ok(Err(e)) if is_not_found(&e) => (None, registry::Load::Missing),
        Ok(Err(_)) => (None, registry::Load::Unreadable),
        Err(_) => (None, registry::Load::Slow),
    }
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<WorkspaceError>().is_some_and(|w| matches!(w, WorkspaceError::NotFound))
}

/// The registry's entry names, sorted — a cheap "did the registered set
/// change" probe for a long-running list (one `read_dir`, no file reads),
/// the workspace-level twin of `Workspace::task_dir_count`. The column
/// compares it with the snapshot it took when it last loaded the workspaces
/// and reloads on any difference, so a workspace registered by `tenx init`
/// (or pruned) while a client runs shows up without a restart. Snapshot it
/// *after* `registered_workspaces`, which prunes dead entries, so the two
/// agree.
pub fn registry_keys() -> Vec<String> {
    let Ok(dir) = registry_dir() else { return vec![] };
    let Ok(entries) = fs::read_dir(&dir) else { return vec![] };
    let mut keys: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    keys.sort();
    keys
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
/// path, which falls back to the global column when cwd isn't in a workspace.
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
    let mut config: WorkspaceConfig = toml::from_str(&text).context("parse workspace config")?;
    let ws_name = config.name.clone();
    if migrate_config(&mut config, &ws_name) {
        // Best-effort: a read-only checkout still loads, it just migrates
        // again next time.
        let _ = atomic_write_toml(&cfg_path, &config);
    }
    Ok(Workspace { dir: dir.to_path_buf(), config })
}

/// Whether `layout` can lay out a task window: one of the built-in layouts
/// (`tabs`, `split`), or a script — an existing, executable file, and not a
/// zellij `.kdl` layout left over from v1.
pub fn check_layout(layout: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !matches!(tenx_core::tabs::layout(layout, ""), tenx_core::tabs::Layout::Script(_)) {
        return Ok(());
    }
    let path = PathBuf::from(expand_home(layout));
    if layout.ends_with(".kdl") {
        bail!("{layout} is a zellij layout; tmux layouts are executables (see `tmux::TaskWindow`)");
    }
    let meta = fs::metadata(&path).with_context(|| format!("layout script {layout} not found"))?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        bail!("layout script {layout} is not an executable file");
    }
    Ok(())
}

/// Bring a config up to `CURRENT_SCHEMA`, one numbered step at a time, and
/// drop a layout that can't run whatever the version says. Returns whether
/// anything changed (so the caller rewrites the file). Each step is the whole
/// story of what that version changed, so the next one has somewhere obvious
/// to go.
fn migrate_config(config: &mut WorkspaceConfig, ws_name: &str) -> bool {
    let mut changed = false;
    // v1: tmux replaced zellij, and `layout` went from a KDL layout file to
    // an executable. Checked regardless of version: a `.kdl` can arrive in a
    // v1 file too (edited by hand), and running it would only fail later,
    // half-way through building a window.
    if !config.layout.is_empty()
        && let Err(e) = check_layout(&config.layout)
    {
        eprintln!("tenx: workspace '{ws_name}': {e} — using the built-in layout");
        config.layout.clear();
        changed = true;
    }
    if config.schema_version < CURRENT_SCHEMA {
        config.schema_version = CURRENT_SCHEMA;
        changed = true;
    }
    changed
}

/// Create a workspace in `dir` with the given name.
/// If `dir` doesn't exist it is created; if it already has a config.toml, errors.
pub fn init(dir: &Path, name: &str) -> Result<Workspace> {
    if dir.join("config.toml").exists() {
        return Err(WorkspaceError::AlreadyExists.into());
    }
    fs::create_dir_all(dir.join("tasks")).context("create tasks dir")?;
    let config = WorkspaceConfig { schema_version: CURRENT_SCHEMA, name: name.to_string(), ..Default::default() };
    atomic_write_toml(&dir.join("config.toml"), &config)?;
    Ok(Workspace { dir: dir.to_path_buf(), config })
}

// ── Workspace methods ─────────────────────────────────────────────────────────

impl Workspace {
    /// The workspace tenx keeps for repo-less sessions (see [`ADHOC_KIND`]).
    /// It never has repos: adding one is refused, and the column leaves it
    /// off the Repos tab. One an older release made counts too, until
    /// `cli::adhoc::ensure` upgrades it.
    pub fn is_adhoc(&self) -> bool {
        self.config.kind == ADHOC_KIND || self.config.kind == LEGACY_ADHOC_KIND
    }

    pub fn save_config(&self) -> Result<()> {
        atomic_write_toml(&self.dir.join("config.toml"), &self.config)
    }

    pub fn add_repo(&mut self, repo: RepoConfig) -> Result<()> {
        if self.is_adhoc() {
            bail!("the adhoc workspace holds sessions without repos — add the repo to another workspace");
        }
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
        tasks.sort_by_key(|t| std::cmp::Reverse(t.created_at));
        Ok(tasks)
    }

    /// How many task directories exist — a cheap "did the task set change"
    /// probe for a long-running list (one `read_dir`, no per-task reads).
    pub fn task_dir_count(&self) -> usize {
        fs::read_dir(self.tasks_dir())
            .map(|d| d.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).count())
            .unwrap_or(0)
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

/// A task as found on disk. Fails only when `task_dir` can't be read.
pub fn discover_task(task_dir: &Path) -> Result<Task> {
    let name = task_dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let display_name = read_task_display_name(task_dir);
    let meta = fs::metadata(task_dir)?;
    // Birth time where the filesystem has one (macOS, Linux with statx). The
    // directory's mtime is not a substitute: tenx's own cache files inside
    // the task bump it on every open and every watcher tick, which used to
    // reshuffle the idle tasks under the cursor.
    let created_at = meta.created().or_else(|_| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH);

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

/// Per-task window signals from `tmux::signals()`, keyed by the window's task
/// directory — or, for a window opened before tenx stamped one on
/// (`tmux::TASK_DIR_OPTION`), by its slug. Empty when the server is down,
/// which reads as "no bells" — correct, since no window exists to ring one in.
///
/// Keyed by directory rather than slug because a slug is only unique within a
/// workspace: two workspaces holding a `claude-design` collapsed into one
/// entry, and whichever window tmux listed last supplied the bell for both.
pub type Signals = std::collections::HashMap<String, Signal>;

/// A task's state: Claude Code's sessions plus its window's bell, resolved by
/// `tenx_core::status::resolve_task_state`. Pass `sessions` and `signals`
/// gathered once per refresh, not per task.
pub fn resolve_task_state(task_dir: &Path, sessions: &[sessions::Session], signals: &Signals) -> TaskState {
    // By directory first; by slug only for a window opened before the option
    // existed, which is exactly the old behaviour for exactly those windows.
    let signal = signals
        .get(&*task_dir.to_string_lossy())
        .or_else(|| {
            let slug = task_dir.file_name()?.to_string_lossy().into_owned();
            signals.get(&slug)
        })
        .copied()
        .unwrap_or_default();
    tenx_core::status::resolve_task_state(task_dir, sessions, signal)
}

/// One task as the JSON shape every out-of-process consumer reads: the `tasks`
/// entries of `tenx task list --json` (scripts and front ends) and the payload of the
/// `tenx::status` pipe (the status bar).
///
/// Shared rather than written out at each call site on purpose — two hand-kept
/// copies of a wire format is exactly how the column and its plugin drift, and
/// here the consumers are in another process and another language runtime, where
/// a silent mismatch surfaces as a missing field at runtime, not a compile error.
pub fn task_json(ws: &Workspace, task: &Task, state: &TaskState) -> serde_json::Value {
    let live = crate::live::read(&task.path);
    serde_json::json!({
        "ports": live.ports,
        "prs": live.prs.iter().map(|p| serde_json::json!({
            "repo": p.repo, "number": p.number, "state": p.state, "url": p.url,
            "draft": p.draft, "review": p.review, "checks": p.checks, "chip": live.chip(p),
        })).collect::<Vec<_>>(),
        "watching": live.watching,
        "ws": ws.config.name,
        "ws_dir": ws.dir,
        "slug": task.name,
        "title": task.display_name,
        "agent": crate::agent::agent_for(ws, &task.path).as_str(),
        "pinned": crate::cli::task::is_pinned(&task.path),
        // The schedule a scheduled run belongs to; null for any other task.
        "schedule": crate::cli::schedule::read_run(&task.path).map(|r| r.schedule),
        "status": state.status.token(),
        "waiting_for": state.waiting_for,
        "background": state.background,
        "sessions": state.sessions,
        "agents": state.agents,
        "subagents": state.subagents.iter().map(|a| serde_json::json!({
            "id": a.id,
            "type": a.agent_type,
            "description": a.description,
            "status": a.status.token(),
            "waiting_for": a.waiting_for,
            "background": a.background,
            "session_pid": a.session_pid,
            "transcript": a.transcript_path,
            "age_secs": a.updated_at.and_then(|c| c.elapsed().ok()).map(|d| d.as_secs()),
        })).collect::<Vec<_>>(),
        "age_secs": state.changed.and_then(|c| c.elapsed().ok()).map(|d| d.as_secs()),
        "repos": task.repos,
        "secrets_pending": secrets_pending(&task.path),
        "secrets_pending_set": secrets_pending_set(&task.path),
        "secrets_why": secrets_why(&task.path).into_iter().map(|(n, w)| (n, w.into())).collect::<serde_json::Map<_, _>>(),
    })
}

/// Filename of a task's pending-secrets-*release* marker (see
/// `cli::secrets::enqueue_pending`, `decrypt`'s non-interactive fallback).
/// Defined here, not only in `cli::secrets`, because `task_json` — the wire
/// shape shared by the column and the status bar pipe — needs to read it
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

/// Filename of the reasons an agent gave for its pending requests
/// (`tenx secrets need --why`), `NAME<TAB>why` per line. Informational only —
/// shown to the human who answers and in the notification; entries for names
/// no longer queued are pruned whenever a queue changes.
pub const SECRETS_WHY_FILE: &str = ".secrets-why";

/// `(name, why)` for every request that came with a reason.
pub fn secrets_why(task_dir: &Path) -> Vec<(String, String)> {
    tenx_core::secrets::parse_notes(&fs::read_to_string(task_dir.join(SECRETS_WHY_FILE)).unwrap_or_default())
}

/// Filename of the requests whose value should come from the clipboard
/// (`need --clipboard`), one `NAME` per line: whoever answers is shown what
/// the clipboard holds and approves it instead of typing a value. Only the
/// name, never the value — the clipboard is read when the human answers.
pub const SECRETS_CLIPBOARD_FILE: &str = ".secrets-clipboard";

/// The names asked for from the clipboard.
pub fn secrets_clipboard(task_dir: &Path) -> Vec<String> {
    tenx_core::secrets::parse_notes(&fs::read_to_string(task_dir.join(SECRETS_CLIPBOARD_FILE)).unwrap_or_default())
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// Filename of the human's denials, `NAME<TAB>note` per line — the one
/// outcome of a request the disk can't otherwise tell apart from a
/// withdrawal (see `tenx_core::secrets`). Cleared per name when that name is
/// requested again.
pub const SECRETS_DENIED_FILE: &str = ".secrets-denied";

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_that_never_opens_is_skipped_not_waited_for() {
        // A FIFO with no writer blocks `open` the way an iCloud Drive file
        // does for a LaunchAgent without the privacy permission.
        let dir = std::env::temp_dir().join(format!("tenx-slow-load-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("config.toml");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());

        let started = std::time::Instant::now();
        let (ws, outcome) = load_within(&dir, Duration::from_millis(100));
        assert!(ws.is_none());
        assert_eq!(outcome, registry::Load::Slow);
        // The read still hangs: the next call doesn't wait, or start another.
        let (_, outcome) = load_within(&dir, Duration::from_secs(10));
        assert_eq!(outcome, registry::Load::Slow);
        assert!(started.elapsed() < Duration::from_secs(5));

        // Let the stuck read end (empty file → parse error), then clean up.
        drop(fs::OpenOptions::new().write(true).open(&fifo).unwrap());
        while LOADING.lock().unwrap().contains(&dir) {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = fs::remove_dir_all(&dir);

        let (_, outcome) = load_within(&dir, Duration::from_secs(1));
        assert_eq!(outcome, registry::Load::Missing);
    }

    #[test]
    fn migration_stamps_version_and_drops_unrunnable_layouts() {
        let mut c = WorkspaceConfig { layout: "/nonexistent/task.kdl".into(), ..Default::default() };
        assert!(migrate_config(&mut c, "ws"));
        assert_eq!(c.schema_version, CURRENT_SCHEMA);
        assert!(c.layout.is_empty());

        // A current-version file with a hand-edited .kdl is caught too.
        let mut c = WorkspaceConfig { schema_version: CURRENT_SCHEMA, layout: "x.kdl".into(), ..Default::default() };
        assert!(migrate_config(&mut c, "ws"));
        assert!(c.layout.is_empty());

        // Already current with no layout: nothing to do, no rewrite.
        let mut c = WorkspaceConfig { schema_version: CURRENT_SCHEMA, ..Default::default() };
        assert!(!migrate_config(&mut c, "ws"));

        // The built-in layouts by name survive.
        let mut c = WorkspaceConfig { schema_version: CURRENT_SCHEMA, layout: "split".into(), ..Default::default() };
        assert!(!migrate_config(&mut c, "ws"));
        assert_eq!(c.layout, "split");

        // An executable layout survives.
        let mut c = WorkspaceConfig { schema_version: CURRENT_SCHEMA, layout: "/bin/sh".into(), ..Default::default() };
        assert!(!migrate_config(&mut c, "ws"));
        assert_eq!(c.layout, "/bin/sh");
    }
}
