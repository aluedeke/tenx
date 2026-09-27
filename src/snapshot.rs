//! The task list as live state: every task across every registered
//! workspace, resolved against one read of the session registry and one
//! `tmux list-windows`. The column (`tui::column`) and the web front end
//! (`tenx web`) both list tasks from here, so the two can't disagree about
//! what a task is doing; what a row *means* — its section, its order, whether
//! it needs you, whether a filter keeps it — is `tenx_core::column`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::workspace::sessions::{Session, Subagent};
use crate::workspace::{self, Signals, TaskGroup, TaskStatus, Workspace};

/// One task, flattened across all workspaces.
pub(crate) struct Row {
    pub(crate) ws_idx: usize,
    pub(crate) ws_name: String,
    pub(crate) slug: String,
    pub(crate) title: String,
    pub(crate) path: PathBuf,
    pub(crate) status: TaskStatus,
    /// The status group this row was filed under, fixed when the rows are
    /// built. `status` keeps updating on the idle tick (the glyph stays
    /// honest) but the row never migrates to another section while the list
    /// is open — sections would tear in two and rows would jump out from
    /// under the cursor.
    pub(crate) group: TaskStatus,
    pub(crate) changed: Option<SystemTime>,
    /// The agent's own reason for waiting, shown next to a blocked row.
    pub(crate) waiting_for: Option<String>,
    /// Sort key within a status group: last status change, or creation time for
    /// a task no agent has touched.
    pub(crate) activity: SystemTime,
    /// The task's tmux window id (`@12`) if its window is open — from
    /// `list-windows`, refreshed on the slow tick.
    pub(crate) window_id: Option<String>,
    /// The pane its agent session runs in (`%40`), from the session registry
    /// — what `A`/`D` answer. Refreshed on the tick with the status.
    pub(crate) pane: Option<String>,
    /// PR chips and listening ports from `.tenx-live.json` (written by
    /// `tenx watch`), refreshed on the slow tick.
    pub(crate) live: crate::live::Live,
    /// Repos this task currently has worktrees for (what the repo editor diffs
    /// against). Read when the rows are built, not on the idle tick.
    pub(crate) repos: Vec<String>,
    /// Secret names pending decrypt (`cli::secrets::enqueue_pending`,
    /// `decrypt`'s non-interactive fallback) — release something already
    /// sealed. Like `group`, fixed when the rows are built and NOT touched by
    /// [`Row::refresh`] — `section` below is derived from it once, and
    /// letting it drift on the idle tick would desync a row's section from
    /// its actual (frozen) position in the list, producing a stray header in
    /// the wrong place.
    pub(crate) secrets_pending: Vec<String>,
    /// Secret names a human needs to supply a value for
    /// (`cli::secrets::enqueue_pending_set`, `set`'s non-interactive
    /// fallback) — distinct from `secrets_pending` above: nothing sealed to
    /// release yet, someone has to type a value in first. Same
    /// frozen-when-built treatment.
    pub(crate) secrets_pending_set: Vec<String>,
    /// `(name, why)` the agent gave with `need --why`, for the pending names
    /// above (`workspace::secrets_why`); read only when something is pending.
    /// For front ends that show why before you answer (the web view).
    pub(crate) secrets_why: Vec<(String, String)>,
    /// The task's coding agent (`.tenx-agent` override, else workspace default,
    /// else claude). Shown as a tag when it isn't the default; read when the
    /// rows are built (an agent change is rare and needs a reopen anyway).
    pub(crate) agent: crate::agent::AgentKind,
    /// The section this row is grouped under (`tenx_core::column::section`):
    /// normally `status.group()`, but a pending secrets request (either kind)
    /// forces `TaskGroup::SecretsPending`. Separate from `group: TaskStatus`
    /// (which stays a pure fact about the agent's state, used for its
    /// glyph/rank) so this override doesn't have to invent a fake
    /// `TaskStatus` variant to express "wants you but idle".
    pub(crate) section: TaskGroup,
    /// A task a running job is still building: its directory and worktrees do
    /// not exist yet. Listed from the moment you hit ⏎ so the task is visibly
    /// *there* while its repos clone, rather than appearing minutes later.
    /// Cleared when the job lands and the rows are read off disk again. A
    /// pending row is not openable — it has no window and no worktree.
    pub(crate) pending: bool,
    /// The subagents of the task's sessions (`TaskState::subagents`), listed
    /// under the task as child lines — waiting first, then running, then the
    /// few that finished recently. Refreshed on the tick with the status.
    pub(crate) subagents: Vec<Subagent>,
}

impl Row {
    /// Where the row sorts (`tenx_core::column::compare`).
    pub(crate) fn sort_key(&self) -> tenx_core::column::SortKey {
        tenx_core::column::SortKey { section: self.section, status: self.group, activity: self.activity }
    }

    /// Whether the row wants something from you right now.
    pub(crate) fn needs_you(&self) -> bool {
        tenx_core::column::needs_you(self.section, self.status)
    }

    /// The section its *live* state belongs in — differs from `section` once
    /// the list's grouping is stale.
    pub(crate) fn live_section(&self) -> TaskGroup {
        let secrets = !self.secrets_pending.is_empty() || !self.secrets_pending_set.is_empty();
        tenx_core::column::section(self.status, secrets)
    }

    /// What the filter matches against: the workspace and the title.
    pub(crate) fn matches(&self, needle: &str) -> bool {
        needle.is_empty() || tenx_core::column::filter_matches(needle, &format!("{} {}", self.ws_name, self.title))
    }

    /// The idle tick: re-resolve status, age, pane and subagents in place —
    /// never the section, never the order. With `windows` (the slow tick),
    /// also the window and the live cache. A pending row has nothing to
    /// resolve: its status is "a job is building it", which the job owns.
    pub(crate) fn refresh(&mut self, sessions: &[Session], signals: &Signals, windows: Option<&Windows>) {
        if self.pending {
            return;
        }
        let state = workspace::resolve_task_state(&self.path, sessions, signals);
        self.status = state.status;
        self.changed = state.changed;
        self.waiting_for = state.waiting_for;
        self.pane = state.pane;
        self.subagents = state.subagents;
        self.activity = state.changed.unwrap_or(self.activity);
        if let Some(w) = windows {
            self.window_id = w.window_of(&self.slug, &self.path);
            self.live = crate::live::read(&self.path);
        }
    }
}

/// One `list-windows`, read for two things: the bell signals and the open
/// windows (with the panes' paths when an untagged window needs them) —
/// what [`Windows::window_of`] matches a task against.
///
/// The live window list, not the per-task cache file: that outlives a closed
/// window and a restarted server, and a row that only looks open makes the
/// arrows stop on it for nothing.
#[derive(Default)]
pub(crate) struct Windows {
    pub(crate) signals: Signals,
    pub(crate) windows: Vec<crate::tmux::Window>,
    pub(crate) pane_paths: HashMap<String, Vec<PathBuf>>,
}

impl Windows {
    /// The tenx session's windows, as of now. An unreachable server reads as
    /// no windows.
    pub(crate) fn read() -> Self {
        Self::from_windows(&crate::tmux::list_windows().unwrap_or_default())
    }

    pub(crate) fn from_windows(windows: &[crate::tmux::Window]) -> Self {
        // Only a window opened before `TASK_DIR_OPTION` needs its panes'
        // paths to say whose it is; after one reopen, none do.
        let pane_paths = if windows.iter().any(|w| w.task_dir.is_none()) {
            crate::tmux::pane_paths_by_window().unwrap_or_default()
        } else {
            HashMap::new()
        };
        Windows { signals: crate::tmux::signals_from(windows), windows: windows.to_vec(), pane_paths }
    }

    /// The id of the open window that belongs to the task at `path` —
    /// narrowed by name (a task's window is named by its slug), settled by
    /// directory (`tmux::window_owned_by`), the same rule as
    /// `tmux::find_task_window`. Never by name alone: a slug is unique only
    /// within a workspace, so a namesake in another workspace would read as
    /// open, and as current.
    pub(crate) fn window_of(&self, slug: &str, path: &std::path::Path) -> Option<String> {
        self.windows
            .iter()
            .find(|w| w.name == slug && crate::tmux::window_owned_by(w, &self.pane_paths, path))
            .map(|w| w.id.clone())
    }
}

/// Every task of `workspaces` as a row, resolved against one read of the
/// session registry (`sessions`) and one of the windows, in list order.
/// All file reads: the task tree, the secrets queues and the live cache.
pub(crate) fn rows(workspaces: &[Workspace], sessions: &[Session], windows: &Windows) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (ws_idx, ws) in workspaces.iter().enumerate() {
        for task in ws.tasks().unwrap_or_default() {
            let state = workspace::resolve_task_state(&task.path, sessions, &windows.signals);
            let secrets_pending = workspace::secrets_pending(&task.path);
            let secrets_pending_set = workspace::secrets_pending_set(&task.path);
            let any_secrets = !secrets_pending.is_empty() || !secrets_pending_set.is_empty();
            let secrets_why = if any_secrets { workspace::secrets_why(&task.path) } else { Vec::new() };
            let section = tenx_core::column::section(state.status, any_secrets);
            rows.push(Row {
                pending: false,
                ws_idx,
                ws_name: ws.config.name.clone(),
                slug: task.name.clone(),
                title: task.display_name.clone(),
                path: task.path.clone(),
                status: state.status,
                group: state.status,
                changed: state.changed,
                waiting_for: state.waiting_for,
                activity: state.changed.unwrap_or(task.created_at),
                window_id: windows.window_of(&task.name, &task.path),
                pane: state.pane,
                live: crate::live::read(&task.path),
                repos: task.repos.clone(),
                agent: crate::agent::agent_for(ws, &task.path),
                secrets_pending,
                secrets_pending_set,
                secrets_why,
                section,
                subagents: state.subagents,
            });
        }
    }
    sort(&mut rows);
    rows
}

/// Put rows in list order: section, then status, then newest activity first.
pub(crate) fn sort(rows: &mut [Row]) {
    rows.sort_by(|a, b| tenx_core::column::compare(&a.sort_key(), &b.sort_key()));
}
