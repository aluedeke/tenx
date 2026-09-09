//! The column: every task across every registered workspace in one list,
//! sectioned by attention (secrets pending → waiting for input → working →
//! inactive), fuzzy-filtered, with each task's Claude-activity status. It is
//! both a switcher and a task manager — type to filter + Enter to open, plus
//! Telescope-style create/delete/rename/close bindings on a selected row
//! (see `Focus`/`InputMode`: the search field is Insert, plain typing
//! filters; a list row is Normal, plain letters act — `n` new, `d`d delete).
//!
//! It is drawn by the client (`tui::client`) beside the embedded tmux
//! session, in the same process: a jump is the window switch (the terminal
//! shows it), quit keys hand focus to the task through `ClientRequest`, and
//! the selection survives switching because nothing restarts.

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph, Tabs},
    Terminal,
};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use unicode_width::UnicodeWidthStr;

use super::mouse;

#[cfg(test)]
mod demo;
#[cfg(test)]
mod screenshot;
use crate::palette;
use crate::workspace::{self, TaskStatus, Workspace};

/// One selectable task row, flattened across all workspaces.
struct Row {
    ws_idx: usize,
    ws_name: String,
    slug: String,
    title: String,
    path: PathBuf,
    status: TaskStatus,
    /// The status group this row was filed under, fixed at `rebuild_rows` time.
    /// `status` keeps updating on the idle tick (the glyph stays honest) but the
    /// row never migrates to another section while the list is open — sections
    /// would tear in two and rows would jump out from under the cursor.
    group: TaskStatus,
    changed: Option<SystemTime>,
    /// Claude Code's own reason for waiting, shown next to a blocked row.
    waiting_for: Option<String>,
    /// Sort key within a status group: last status change, or creation time for
    /// a task Claude has never touched.
    activity: SystemTime,
    /// The task's tmux window id (`@12`) if its window is open — from the
    /// per-task cache, refreshed on the tick.
    window_id: Option<String>,
    /// The pane its Claude session runs in (`%40`), from the session registry
    /// — what `y`/`N` answer. Refreshed on the
    /// tick with the status.
    pane: Option<String>,
    /// PR chips and listening ports from `.tenx-live.json` (written by
    /// `tenx watch`), refreshed on the tick.
    live: crate::live::Live,
    /// Repos this task currently has worktrees for (what the repo editor diffs
    /// against). Refreshed on `rebuild_rows`, not on the idle tick.
    repos: Vec<String>,
    /// Secret names pending decrypt (`cli::secrets::enqueue_pending`,
    /// `decrypt`'s non-interactive fallback) — release something already
    /// sealed. Like `group`, fixed at `rebuild_rows` time and NOT touched by
    /// `refresh_statuses` — `section` below is derived from it once, and
    /// letting it drift on the idle tick would desync a row's section from
    /// its actual (frozen) position in `rows`, producing a stray header in
    /// the wrong place.
    secrets_pending: Vec<String>,
    /// Secret names a human needs to supply a value for
    /// (`cli::secrets::enqueue_pending_set`, `set`'s non-interactive
    /// fallback) — distinct from `secrets_pending` above: nothing sealed to
    /// release yet, someone has to type a value in first. Same
    /// frozen-at-`rebuild_rows` treatment.
    secrets_pending_set: Vec<String>,
    /// The task's coding agent (`.tenx-agent` override, else workspace default,
    /// else claude). Shown as a tag when it isn't the default; set at
    /// `rebuild_rows` time (an agent change is rare and needs a reopen anyway).
    agent: crate::agent::AgentKind,
    /// The section this row is grouped under — normally `status.group()`, but
    /// a pending secrets request (either kind) forces `TaskGroup::SecretsPending`
    /// regardless of Claude session state, since it needs a specific action
    /// from you (unlocking, or supplying a value) even when the task is
    /// otherwise idle. Separate from `group: TaskStatus` (which stays a pure
    /// fact about Claude session state, used for its glyph/rank) so this
    /// override doesn't have to invent a fake `TaskStatus` variant to express
    /// "wants you but idle".
    section: workspace::TaskGroup,
}

/// Create-task form (workspace already chosen). `focus`: 0 = name,
/// 1.. = repo checkboxes.
struct CreateForm {
    ws_idx: usize,
    name: String,
    repos: Vec<(String, bool)>,
    focus: usize,
}

impl CreateForm {
    fn field_count(&self) -> usize {
        1 + self.repos.len()
    }
    fn focus_next(&mut self) {
        self.focus = (self.focus + 1) % self.field_count();
    }
    fn focus_prev(&mut self) {
        self.focus = if self.focus == 0 {
            self.field_count() - 1
        } else {
            self.focus - 1
        };
    }
}

/// One line of a repo checklist: the workspace repo, whether it's ticked, and
/// whether the task already has a worktree for it. `checked != present` is the
/// pending change — added when ticked, detached when unticked.
#[derive(Clone)]
struct RepoPick {
    name: String,
    checked: bool,
    present: bool,
}

/// Edit which repos an existing task has worktrees for. Detaching is
/// destructive (worktree + task branch), so an apply that removes anything goes
/// through `confirm` first.
struct EditReposForm {
    ws_idx: usize,
    slug: String,
    title: String,
    picks: Vec<RepoPick>,
    focus: usize,
    confirm: bool,
}

impl EditReposForm {
    fn added(&self) -> Vec<String> {
        self.picks.iter().filter(|p| p.checked && !p.present).map(|p| p.name.clone()).collect()
    }
    fn removed(&self) -> Vec<String> {
        self.picks.iter().filter(|p| !p.checked && p.present).map(|p| p.name.clone()).collect()
    }
    fn desired(&self) -> Vec<String> {
        self.picks.iter().filter(|p| p.checked).map(|p| p.name.clone()).collect()
    }
}

/// Add-repo form. `focus`: 0 = url, 1 = name.
struct AddRepoForm {
    ws_idx: usize,
    url: String,
    name: String,
    focus: usize,
}

/// Pending delete confirmation.
struct Confirm {
    ws_idx: usize,
    slug: String,
    title: String,
}

/// Rename-title form.
struct RenameForm {
    slug: String,
    path: PathBuf,
    buffer: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Tasks,
    Repos,
}

/// Telescope-style input mode for the list view. Insert = type filters (default,
/// fast switch); Normal = vim keys (`j/k`, `dd`, `gt`, …).
#[derive(Debug, Clone, Copy, PartialEq)]
enum InputMode {
    Insert,
    Normal,
}

/// Where the single cursor lives: the search field, or a list row. Invariant:
/// `Search` implies Insert mode (you can only type while focused on search).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Focus {
    Search,
    List,
}

/// One repo row in the Repos tab (lean: status + last commit, synchronous).
struct RepoRow {
    ws_idx: usize,
    ws_name: String,
    name: String,
    cloned: bool,
    commit: Option<String>,
}

enum Mode {
    List,
    /// Vim-style `:` command line; the String is the buffer after the colon.
    Command(String),
    /// New-task form (workspace derived from the current selection).
    Create(CreateForm),
    /// Add-repo form (Repos tab), workspace from the selected repo.
    AddRepo(AddRepoForm),
    /// Repo checklist for the selected task (add/detach worktrees).
    EditRepos(EditReposForm),
    Confirm(Confirm),
    Rename(RenameForm),
}

/// What the column asks the client to do, since it cannot move focus or
/// hide itself: the terminal is in the same process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientRequest {
    /// Put the keyboard in the embedded terminal (a jump landed, or a quit
    /// key: the column stays).
    FocusTerminal,
    /// `:hide` — take the column away.
    Hide,
    /// `:q` — leave the client altogether.
    Quit,
}

pub(super) struct Column {
    client_request: Option<ClientRequest>,
    /// No tmux, no registry: a switch or an answer only updates this
    /// struct. The README demo's mode; never set by the client.
    pub(super) offline: bool,
    workspaces: Vec<Workspace>,
    tab: Tab,
    input_mode: InputMode,
    focus: Focus,
    /// First key of a pending 2-key normal-mode sequence (`g`, `d`).
    pending: Option<char>,
    rows: Vec<Row>,
    filter: String,
    /// Indices into `rows` that pass the current filter, in display order.
    filtered: Vec<usize>,
    /// Position within `filtered`.
    selected: usize,
    repo_rows: Vec<RepoRow>,
    repo_filtered: Vec<usize>,
    repo_selected: usize,
    status_msg: Option<String>,
    mode: Mode,
    /// Set by `start_unlock` (the `u` key / `:unlock`) to (workspace index,
    /// slug). `run_loop` checks this after every event and, when set,
    /// suspends the TUI (leaves raw mode/alt screen) to run the real
    /// interactive `cli::secrets::decrypt_in` — the identity's passphrase
    /// prompt needs a real controlling terminal, which the alternate screen
    /// isn't. Not handled inside `Column` itself because only `run_loop` has
    /// the `Terminal` handle needed to leave and re-enter raw mode.
    pending_unlock: Option<(usize, String)>,

    // Mouse support (list view only; the modal forms stay keyboard-only).
    // `list_state` persists the scroll offset so a click's row maps to a list
    // line; `line_to_pos` maps each rendered line back to its filtered position
    // (None for workspace-group headers and blank separators). The three areas
    // are the tab bar, search box, and list, recorded during render.
    list_state: ListState,
    line_to_pos: Vec<Option<usize>>,
    /// Rendered height of each list item, in the same order as
    /// `line_to_pos` — the column's rows are two lines tall, headers one,
    /// so a click's row is walked through these from the scroll offset.
    item_heights: Vec<u16>,
    tabs_area: Rect,
    search_area: Rect,
    list_area: Rect,

    /// Last time the background idle-tab sweep ran (`maybe_sweep`), so a
    /// bouncy window manager sending repeated `FocusGained` events can't fire
    /// it more than once per `SWEEP_INTERVAL`. `None` until the first one.
    last_swept: Option<Instant>,

    /// Window signals as of the last slow refresh (see `refresh_statuses`).
    signals: workspace::Signals,
    /// Open task windows by slug, from the same `list-windows` as `signals`.
    /// The per-task cache file is *not* used here: it outlives a closed
    /// window and a restarted server, and a row that only looks open makes
    /// the arrows stop on it for nothing.
    window_ids: std::collections::HashMap<String, String>,
    /// Slug of the session's current window, if it's a task — gets the
    /// "current" chip.
    current: Option<String>,
    /// When the slow inputs (tmux, per-task cache files) were last re-read.
    slow_refreshed: Option<Instant>,
}

/// How often the idle tick re-reads the *slow* inputs — `tmux list-windows`
/// (a subprocess) and each row's cache files. The watcher only changes them
/// every 2 s, so asking more often than that is spawn churn for nothing; the
/// session registry (a directory read) is still checked every tick.
const SLOW_REFRESH: Duration = Duration::from_secs(2);

/// Minimum spacing between the home pane's automatic idle-tab sweeps — see
/// `Column::maybe_sweep`. `FocusGained` already only triggers a rescan, which
/// is cheap; this just keeps the sweep itself (a `zellij` subprocess call per
/// candidate) from re-running on every glance back at the terminal.
const SWEEP_INTERVAL: Duration = Duration::from_secs(300);

impl Column {
    pub(super) fn new() -> Self {
        let mut o = Self::empty();
        o.workspaces = workspace::registered_workspaces();
        o.rebuild_rows();
        o
    }

    /// A row whose status moved it to another section since the rows were
    /// last built — the list's grouping is stale.
    pub(super) fn sections_stale(&self) -> bool {
        self.rows.iter().any(|r| {
            let now = if r.secrets_pending.is_empty() && r.secrets_pending_set.is_empty() {
                r.status.group()
            } else {
                workspace::TaskGroup::SecretsPending
            };
            now != r.section
        })
    }

    /// Rebuild (re-group and re-sort) while keeping the selection on the
    /// same task. The column calls this while the keyboard is elsewhere,
    /// so rows move only when nobody is moving through them.
    pub(super) fn tidy(&mut self) {
        let keep = self.selected_row().map(|r| r.slug.clone());
        self.rebuild_rows();
        if let Some(slug) = keep
            && let Some(pos) = self.filtered.iter().position(|&i| self.rows[i].slug == slug)
        {
            self.selected = pos;
        }
    }

    /// One line of state for the client's trace log: mode, selection (with
    /// its slug and whether its window is open), the current task, filter,
    /// and the slugs on screen in order.
    pub(super) fn trace_state(&self) -> String {
        let sel = self
            .selected_row()
            .map(|r| format!("{}:{}{}", self.selected, r.slug, if r.window_id.is_some() { "" } else { "(closed)" }))
            .unwrap_or_else(|| "-".into());
        let order: Vec<String> = self
            .filtered
            .iter()
            .map(|&i| {
                let r = &self.rows[i];
                format!("{}{}", r.slug, if r.window_id.is_some() { "" } else { "~" })
            })
            .collect();
        format!(
            "{:?}/{:?} sel={sel} current={:?} filter={:?} rows=[{}]",
            self.focus,
            self.input_mode,
            self.current,
            self.filter,
            order.join(" ")
        )
    }

    /// The client's pending request, if the last event made one.
    pub(super) fn take_request(&mut self) -> Option<ClientRequest> {
        self.client_request.take()
    }

    /// Whether the list (not a form or the command line) is showing — when
    /// the idle tick may refresh rows.
    pub(super) fn in_list_mode(&self) -> bool {
        matches!(self.mode, Mode::List)
    }

    pub(super) fn take_unlock(&mut self) -> Option<(usize, String)> {
        self.pending_unlock.take()
    }

    /// A column with no workspaces and no rows, touching nothing outside the
    /// process — `new` fills it from the registry; the screenshot test fills
    /// it with fixtures.
    fn empty() -> Self {
        Column {
            client_request: None,
            offline: false,
            workspaces: vec![],
            tab: Tab::Tasks,
            input_mode: InputMode::Insert,
            focus: Focus::Search,
            pending: None,
            rows: vec![],
            filter: String::new(),
            filtered: vec![],
            selected: 0,
            repo_rows: vec![],
            repo_filtered: vec![],
            repo_selected: 0,
            status_msg: None,
            mode: Mode::List,
            pending_unlock: None,
            list_state: ListState::default(),
            line_to_pos: Vec::new(),
            item_heights: Vec::new(),
            tabs_area: Rect::default(),
            search_area: Rect::default(),
            list_area: Rect::default(),
            last_swept: None,
            signals: workspace::Signals::new(),
            window_ids: std::collections::HashMap::new(),
            current: None,
            slow_refreshed: None,
        }
    }

    // ── Tabs ──────────────────────────────────────────────────────────────────

    fn toggle_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Tasks => Tab::Repos,
            Tab::Repos => Tab::Tasks,
        };
        if self.tab == Tab::Repos && self.repo_rows.is_empty() {
            self.rebuild_repo_rows();
        }
    }

    fn select_repos_tab(&mut self) {
        self.tab = Tab::Repos;
        if self.repo_rows.is_empty() {
            self.rebuild_repo_rows();
        }
    }

    /// Scan every workspace's repos for clone status + last commit. Synchronous
    /// (local git only); cached until the column is reopened.
    fn rebuild_repo_rows(&mut self) {
        let global = crate::workspace::load_global().unwrap_or_default();
        let mut rows = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            let bare_dir = ws.bare_dir(&global);
            for repo in &ws.config.repos {
                let bare = crate::git::bare_repo_path(&bare_dir, &repo.name);
                let cloned = bare.exists();
                let commit = if cloned { crate::git::last_commit(&bare) } else { None };
                rows.push(RepoRow {
                    ws_idx,
                    ws_name: ws.config.name.clone(),
                    name: repo.name.clone(),
                    cloned,
                    commit,
                });
            }
        }
        self.repo_rows = rows;
        self.apply_filter();
    }

    /// Rescan all workspaces for tasks + status. All file reads: the task tree,
    /// plus one snapshot of Claude Code's session registry that every row
    /// resolves against (`workspace::resolve_task_state`).
    pub(super) fn rebuild_rows(&mut self) {
        // One flat list across all workspaces, grouped by agent status
        // (`TaskStatus::rank` — needs-input first, idle last) and, within a
        // group, by last status change newest first. Tasks with no agent
        // activity yet fall back to creation time.
        let sessions = workspace::sessions::sessions();
        self.refresh_windows();
        let signals = &self.signals;
        let mut rows: Vec<Row> = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for task in ws.tasks().unwrap_or_default() {
                let state = workspace::resolve_task_state(&task.path, &sessions, signals);
                let window_id = self.window_ids.get(&task.name).cloned();
                let secrets_pending = workspace::secrets_pending(&task.path);
                let secrets_pending_set = workspace::secrets_pending_set(&task.path);
                let section = if !secrets_pending.is_empty() || !secrets_pending_set.is_empty() {
                    workspace::TaskGroup::SecretsPending
                } else {
                    state.status.group()
                };
                rows.push(Row {
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
                    window_id,
                    pane: state.pane,
                    live: crate::live::read(&task.path),
                    repos: task.repos.clone(),
                    agent: crate::agent::agent_for(ws, &task.path),
                    secrets_pending,
                    secrets_pending_set,
                    section,
                });
            }
        }
        self.rows = rows;
        self.sort_rows();
        self.apply_filter();
        self.current = crate::tmux::current_task();
    }

    fn sort_rows(&mut self) {
        self.rows.sort_by(|a, b| {
            a.section
                .rank()
                .cmp(&b.section.rank())
                .then(a.group.rank().cmp(&b.group.rank()))
                .then(b.activity.cmp(&a.activity))
        });
    }

    /// Idle-tick refresh: re-read each row's status/age/tab-id in place,
    /// WITHOUT re-sorting or re-discovering tasks. The list order is frozen
    /// while the column is showing (no rows shuffling under the cursor) and
    /// only recomputed when the list is (re)opened: floating column spawn,
    /// home-pane startup, regaining focus, returning after a jump, or a
    /// mutating action (create/delete/rename).
    pub(super) fn refresh_statuses(&mut self) {
        let sessions = workspace::sessions::sessions();
        let slow = self.slow_refreshed.is_none_or(|t| t.elapsed() >= SLOW_REFRESH);
        if slow {
            self.refresh_windows();
            // A task created or removed from outside (the CLI, the skill,
            // another client) is not a row yet: rebuild, keeping the
            // selection on its task. One `read_dir` per workspace.
            let on_disk: usize = self.workspaces.iter().map(|ws| ws.task_dir_count()).sum();
            if on_disk != self.rows.len() {
                self.tidy();
                return;
            }
        }
        let signals = &self.signals;
        let mut rows = std::mem::take(&mut self.rows);
        for r in rows.iter_mut() {
            let state = workspace::resolve_task_state(&r.path, &sessions, signals);
            r.status = state.status;
            r.changed = state.changed;
            r.waiting_for = state.waiting_for;
            r.pane = state.pane;
            r.activity = state.changed.unwrap_or(r.activity);
            if slow {
                r.window_id = self.window_ids.get(&r.slug).cloned();
                r.live = crate::live::read(&r.path);
            }
        }
        self.rows = rows;
        // Fresher than the slow refresh's window list: the task beside the
        // column is what ↓/↑ start from.
        self.current = crate::tmux::current_task();
    }

    /// One `list-windows` for both the bell signals and the current window.
    fn refresh_windows(&mut self) {
        let windows = crate::tmux::list_windows().unwrap_or_default();
        self.signals = crate::tmux::signals_from(&windows);
        self.window_ids = windows.iter().map(|w| (w.name.clone(), w.id.clone())).collect();
        self.current = windows
            .iter()
            .find(|w| w.active && w.name != crate::tmux::HOME_WINDOW)
            .map(|w| w.name.clone());
        self.slow_refreshed = Some(Instant::now());
    }

    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                needle.is_empty()
                    || subseq_match(&needle, &format!("{} {}", r.ws_name, r.title).to_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        self.repo_filtered = self
            .repo_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                needle.is_empty()
                    || subseq_match(&needle, &format!("{} {}", r.ws_name, r.name).to_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        if self.repo_selected >= self.repo_filtered.len() {
            self.repo_selected = self.repo_filtered.len().saturating_sub(1);
        }
    }

    fn cur_len(&self) -> usize {
        match self.tab {
            Tab::Tasks => self.filtered.len(),
            Tab::Repos => self.repo_filtered.len(),
        }
    }

    fn cur_sel(&self) -> usize {
        match self.tab {
            Tab::Tasks => self.selected,
            Tab::Repos => self.repo_selected,
        }
    }

    fn set_cur_sel(&mut self, i: usize) {
        match self.tab {
            Tab::Tasks => self.selected = i,
            Tab::Repos => self.repo_selected = i,
        }
    }

    /// Focus and input mode are unified: the search field is Insert (type to
    /// filter), the list is Normal (vim keys). Moving between them switches mode
    /// automatically — so no Esc is needed (important on an iPad keyboard).
    fn focus_search(&mut self) {
        self.focus = Focus::Search;
        self.input_mode = InputMode::Insert;
    }

    fn focus_list(&mut self) {
        self.focus = Focus::List;
        self.input_mode = InputMode::Normal;
    }

    /// Down: from Search, enter the list at the top (→ Normal); within the list,
    /// move down clamped at the bottom (no wraparound).
    /// Every row, closed tasks included: in the column, landing on an open
    /// task switches to its window (`follow_selection`); landing on a
    /// closed one shows the client's "⏎ to open" screen instead.
    fn nav_down(&mut self) {
        self.step_down();
        self.follow_selection();
    }

    /// Up: within the list, move up; at the top, return to the search field
    /// (→ Insert). In Search, stay put — except in the column, where the
    /// list is entered at the task you are sitting in (`own_row`), so the
    /// first Up goes to the task above it.
    fn nav_up(&mut self) {
        self.step_up();
        self.follow_selection();
    }

    /// Put the cursor on the task you are in (Normal mode, row highlighted):
    /// what Ctrl+w lands on, so the list reads "you are here" and the next
    /// ↓ is the task below. `/` or `i` from there types a filter.
    pub(super) fn select_current(&mut self) {
        if let Some(p) = self.own_row() {
            self.selected = p;
            self.focus_list();
        }
    }

    /// The keyboard left the column: drop the row highlight so the list
    /// shows no cursor while the task has it. Ctrl+w brings it back on the
    /// current task (`select_current`).
    pub(super) fn blur(&mut self) {
        if matches!(self.mode, Mode::List) {
            self.focus_search();
        }
    }

    /// The selected task's title when it has no open window (and the list
    /// has the cursor) — what the client shows an empty screen for.
    pub(super) fn selected_closed(&self) -> Option<String> {
        if self.tab != Tab::Tasks || self.focus != Focus::List {
            return None;
        }
        self.selected_row().filter(|r| r.window_id.is_none()).map(|r| r.title.clone())
    }

    /// The movement of `nav_down` without the column's window switch — the
    /// mouse wheel browses without switching.
    fn step_down(&mut self) {
        match self.focus {
            Focus::Search => {
                if self.cur_len() > 0 {
                    let from = self.own_row().map(|p| p + 1).unwrap_or(0);
                    self.focus_list();
                    self.set_cur_sel(from.min(self.cur_len() - 1));
                }
            }
            Focus::List => {
                let len = self.cur_len();
                if len > 0 {
                    self.set_cur_sel((self.cur_sel() + 1).min(len - 1));
                }
            }
        }
    }

    fn step_up(&mut self) {
        match self.focus {
            Focus::Search => {
                if let Some(p) = self.own_row() {
                    self.focus_list();
                    self.set_cur_sel(p.saturating_sub(1));
                }
            }
            Focus::List => {
                if self.cur_sel() == 0 {
                    self.focus_search();
                } else {
                    self.set_cur_sel(self.cur_sel() - 1);
                }
            }
        }
    }

    fn move_top(&mut self) {
        self.focus_list();
        self.set_cur_sel(0);
        self.follow_selection();
    }

    fn move_bottom(&mut self) {
        self.focus_list();
        self.set_cur_sel(self.cur_len().saturating_sub(1));
        self.follow_selection();
    }

    /// Position (in `filtered`) of the task the column sits beside; `None`
    /// on the other surfaces, or when the filter hides it.
    fn own_row(&self) -> Option<usize> {
        if self.tab != Tab::Tasks {
            return None;
        }
        self.filtered.iter().position(|&i| Some(self.rows[i].slug.as_str()) == self.current.as_deref())
    }

    /// The column follows its selection: moving onto a task whose window is
    /// open switches to that window, cmux-style — the embedded terminal
    /// shows it, and the column is the same process, so the selection
    /// simply carries on. A task with no window is only selected (the
    /// client shows an empty screen for it; ⏎ opens it). Never fires from
    /// the search field or off the Tasks tab.
    fn follow_selection(&mut self) {
        if self.tab != Tab::Tasks || self.focus != Focus::List {
            return;
        }
        let Some(row) = self.selected_row() else { return };
        if row.window_id.is_none() || Some(row.slug.as_str()) == self.current.as_deref() {
            return;
        }
        let slug = row.slug.clone();
        if self.offline {
            self.current = Some(slug);
            return;
        }
        let Some(w) = crate::tmux::find_window(&slug).ok().flatten() else { return };
        if crate::tmux::select_window(&w.id).is_ok() {
            self.current = Some(slug);
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        self.filtered.get(self.selected).and_then(|&i| self.rows.get(i))
    }

    // ── Mouse dispatch ────────────────────────────────────────────────────────

    /// Scroll the list by `delta` items without touching the selection.
    /// While a row is selected ratatui keeps it in view, so the view can't
    /// leave the selection behind; from the search field it scrolls freely.
    fn scroll_view(&mut self, delta: i32) {
        let items = self.item_heights.len().max(1);
        let cur = self.list_state.offset() as i32;
        let next = (cur + delta).clamp(0, items as i32 - 1) as usize;
        *self.list_state.offset_mut() = next;
    }

    /// Handle a mouse event in the list view (the modal forms stay
    /// keyboard-only). Wheel scrolls the selection; clicking a tab header, the
    /// search box, or a task/repo row focuses it. Deliberately NO click-to-jump:
    /// jumping runs `zellij action go-to-tab`, which zellij applies to the last
    /// client that pressed a *key* — mouse events don't update that, so a
    /// tap-triggered jump from a phone (with a desktop client also attached)
    /// would switch the desktop's tab instead of the phone's. Requiring ⏎ to
    /// jump guarantees the jumping client just sent a keystroke and is
    /// therefore the one zellij switches. Returns `Ok(true)` when the column
    /// should close (a jump completed).
    pub(super) fn handle_mouse(&mut self, m: MouseEvent) -> Result<bool> {
        if !matches!(self.mode, Mode::List) {
            return Ok(false);
        }
        match m.kind {
            // The wheel scrolls the view, never the selection: a trackpad
            // brushing the column must not change what ↓ does next.
            MouseEventKind::ScrollDown => self.scroll_view(1),
            MouseEventKind::ScrollUp => self.scroll_view(-1),
            MouseEventKind::Down(MouseButton::Left) => {
                if mouse::hit(self.tabs_area, m.column, m.row) {
                    // Two tabs split the bar width; left half = Tasks, right = Repos.
                    let rel = m.column.saturating_sub(self.tabs_area.x);
                    let want_repos = rel >= self.tabs_area.width / 2;
                    match (want_repos, self.tab) {
                        (true, Tab::Tasks) => self.select_repos_tab(),
                        (false, Tab::Repos) => self.tab = Tab::Tasks,
                        _ => {}
                    }
                } else if mouse::hit(self.search_area, m.column, m.row) {
                    self.focus_search();
                } else if let Some(line) = mouse::item_at_heights(
                    self.list_area,
                    1,
                    self.list_state.offset(),
                    &self.item_heights,
                    m.column,
                    m.row,
                ) && let Some(Some(pos)) = self.line_to_pos.get(line).copied()
                {
                    self.focus_list();
                    self.set_cur_sel(pos);
                    self.follow_selection();
                }
            }
            _ => {}
        }
        Ok(false)
    }

    // ── Key dispatch ──────────────────────────────────────────────────────────

    /// Returns `Ok(true)` when the column should close.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        enum Kind {
            List,
            Command,
            Create,
            AddRepo,
            EditRepos,
            Confirm,
            Rename,
        }
        let kind = match self.mode {
            Mode::List => Kind::List,
            Mode::Command(_) => Kind::Command,
            Mode::Create(_) => Kind::Create,
            Mode::AddRepo(_) => Kind::AddRepo,
            Mode::EditRepos(_) => Kind::EditRepos,
            Mode::Confirm(_) => Kind::Confirm,
            Mode::Rename(_) => Kind::Rename,
        };
        let close = match kind {
            Kind::List => self.handle_list_key(key),
            Kind::Command => self.handle_command_key(key),
            Kind::Create => self.handle_create_key(key),
            Kind::AddRepo => self.handle_addrepo_key(key),
            Kind::EditRepos => self.handle_editrepos_key(key),
            Kind::Confirm => {
                self.handle_confirm_key(key);
                Ok(false)
            }
            Kind::Rename => self.handle_rename_key(key),
        }?;
        // The column stays; a quit key means "back to the task".
        if close {
            self.client_request = Some(ClientRequest::FocusTerminal);
        }
        Ok(false)
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> Result<bool> {
        match self.input_mode {
            InputMode::Insert => self.handle_insert_key(key),
            InputMode::Normal => self.handle_normal_key(key),
        }
    }

    /// Insert mode: type to filter (the fast switch path). `Esc` → Normal.
    fn handle_insert_key(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.focus_list(), // → Normal (also reachable via ↓)
            KeyCode::Char('c') if ctrl => return Ok(true),
            KeyCode::Tab | KeyCode::BackTab => self.toggle_tab(),
            KeyCode::Enter => {
                if self.tab == Tab::Tasks {
                    // Enter from the search field opens the top match.
                    if self.focus == Focus::Search {
                        self.set_cur_sel(0);
                    }
                    return self.jump();
                }
            }
            KeyCode::Down => self.nav_down(),
            KeyCode::Up => self.nav_up(),
            KeyCode::Char('j') if ctrl => self.nav_down(),
            KeyCode::Char('k') if ctrl => self.nav_up(),
            // `:` reaches the pane (zellij doesn't grab it), unlike Ctrl/Alt.
            KeyCode::Char(':') if !ctrl => {
                self.status_msg = None;
                self.mode = Mode::Command(String::new());
            }
            KeyCode::Backspace => {
                self.status_msg = None;
                self.focus_search();
                self.filter.pop();
                self.apply_filter();
            }
            KeyCode::Char(c) if !ctrl => {
                self.status_msg = None;
                self.focus_search();
                self.filter.push(c);
                self.apply_filter();
            }
            _ => {}
        }
        Ok(false)
    }

    /// Normal mode: vim keys. `i`/`/` → Insert, `q`/`Esc` → close.
    fn handle_normal_key(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Second key of a 2-key sequence (gg, gt, gT, dd).
        if let Some(p) = self.pending.take() {
            match (p, key.code) {
                ('g', KeyCode::Char('g')) => self.move_top(),
                ('g', KeyCode::Char('t' | 'T')) => self.toggle_tab(),
                ('d', KeyCode::Char('d')) if self.require_tasks() => self.start_delete(),
                _ => {} // incomplete/unknown sequence — cancel
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc => return Ok(true),
            KeyCode::Char('c') if ctrl => return Ok(true),
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('i') | KeyCode::Char('/') => self.focus_search(),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char('G') => self.move_bottom(),
            KeyCode::Char('j') | KeyCode::Down => self.nav_down(),
            KeyCode::Char('k') | KeyCode::Up => self.nav_up(),
            KeyCode::Tab | KeyCode::BackTab => self.toggle_tab(),
            // `n` for a new task (matches the `:n`/`:new` command below),
            // `a` to add a repo — distinct verbs, distinct letters.
            KeyCode::Char('n') if self.tab == Tab::Tasks => self.start_create(),
            KeyCode::Char('a') if self.tab == Tab::Repos => self.start_add_repo(),
            KeyCode::Char('r') => {
                if self.require_tasks() {
                    self.start_rename();
                }
            }
            KeyCode::Char('e') => {
                if self.require_tasks() {
                    self.start_edit_repos();
                }
            }
            KeyCode::Char('x') => {
                if self.require_tasks() {
                    self.close_selected_tab();
                }
            }
            KeyCode::Char('y') if self.require_tasks() => self.answer(tenx_core::dialog::Answer::Yes),
            KeyCode::Char('N') if self.require_tasks() => self.answer(tenx_core::dialog::Answer::No),
            KeyCode::Char('u') => {
                if self.require_tasks() {
                    self.start_unlock();
                }
            }
            KeyCode::Enter | KeyCode::Char('o') | KeyCode::Char('l') => {
                if self.tab == Tab::Tasks {
                    return self.jump();
                }
            }
            KeyCode::Char(':') => {
                self.status_msg = None;
                self.mode = Mode::Command(String::new());
            }
            _ => {}
        }
        Ok(false)
    }

    fn require_tasks(&mut self) -> bool {
        if self.tab == Tab::Tasks {
            true
        } else {
            self.status_msg = Some("switch to Tasks (gt) for that".into());
            false
        }
    }

    // ── Command line (`:n`, `:d`, `:o`, `:r`, `:x`, `:q`) ──────────────────────

    fn handle_command_key(&mut self, key: KeyEvent) -> Result<bool> {
        let mut buffer = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::Command(b) => b,
            other => {
                self.mode = other;
                return Ok(false);
            }
        };
        match key.code {
            KeyCode::Esc => return Ok(false), // back to list
            KeyCode::Enter => return self.run_command(buffer.trim()),
            KeyCode::Backspace => {
                buffer.pop();
                if buffer.is_empty() {
                    return Ok(false); // backspacing past `:` returns to the list
                }
            }
            KeyCode::Char(c) => buffer.push(c),
            _ => {}
        }
        self.mode = Mode::Command(buffer);
        Ok(false)
    }

    /// Run a `:` command against the selected task. Returns `Ok(true)` to close
    /// the column. Commands that open a sub-view set `self.mode` themselves.
    fn run_command(&mut self, cmd: &str) -> Result<bool> {
        // Tab switches and quit work from either tab.
        match cmd {
            "tasks" => {
                self.tab = Tab::Tasks;
                return Ok(false);
            }
            "repos" => {
                self.select_repos_tab();
                return Ok(false);
            }
            "q" | "quit" => {
                self.client_request = Some(ClientRequest::Quit);
                return Ok(false);
            }
            // `:n` works from either tab — it uses the selected item's workspace.
            "n" | "new" => {
                self.start_create();
                return Ok(false);
            }
            "" => return Ok(false),
            _ => {}
        }
        // The remaining actions operate on the selected task.
        if self.tab != Tab::Tasks {
            self.status_msg = Some("switch to Tasks (:tasks) for that".into());
            return Ok(false);
        }
        match cmd {
            "d" | "del" | "delete" | "rm" => self.start_delete(),
            "r" | "rename" => self.start_rename(),
            // NB: not `:repos` — that's taken above by the Repos tab switch.
            "e" | "edit" | "edit-repos" => self.start_edit_repos(),
            "x" | "close" => self.close_selected_tab(),
            "u" | "unlock" => self.start_unlock(),
            "y" | "approve" | "allow" => self.answer(tenx_core::dialog::Answer::Yes),
            "deny" => self.answer(tenx_core::dialog::Answer::No),
            "cancel" => self.cancel_secrets(),
            "hide" => self.client_request = Some(ClientRequest::Hide),
            "o" | "open" => return self.jump(),
            other => self.status_msg = Some(format!("unknown command: :{other}")),
        }
        Ok(false)
    }

    // ── Answering a permission prompt ─────────────────────────────────────────

    /// `y` / `N` on a blocked row: answer Claude Code's permission dialog in
    /// the task's own pane without visiting it. The row's status may be up to
    /// a tick old, so the truth is re-read right before the key goes out: the
    /// session must still be waiting on a *permission prompt* (not an
    /// elicitation, which `Enter` would answer wrongly) and the dialog must
    /// still be on screen (`tenx_core::dialog::permission_dialog_visible`).
    /// Anything else is refused with a message, never sent.
    fn answer(&mut self, answer: tenx_core::dialog::Answer) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if self.offline {
            // The demo: the dialog is answered by fiat, and the task is
            // working again.
            let title = row.title.clone();
            let i = self.filtered[self.selected];
            self.rows[i].status = TaskStatus::Working;
            self.rows[i].waiting_for = None;
            self.status_msg = Some(format!("{} '{title}'", answer.verb()));
            return;
        }
        let (title, path) = (row.title.clone(), row.path.clone());
        let sessions = workspace::sessions::sessions();
        let state = workspace::resolve_task_state(&path, &sessions, &self.signals);
        if state.status != TaskStatus::Blocked {
            self.status_msg = Some(format!("'{title}' is not waiting on a prompt"));
            return;
        }
        if state.waiting_for.as_deref() != Some(tenx_core::dialog::PERMISSION_PROMPT) {
            let reason = state.waiting_for.unwrap_or_default();
            self.status_msg = Some(format!("'{title}' is waiting on {reason} — open it to answer (⏎)"));
            return;
        }
        let Some(pane) = state.pane else {
            self.status_msg = Some(format!("'{title}': no pane known for its session"));
            return;
        };
        let capture = match crate::tmux::capture_pane(&pane) {
            Ok(c) => c,
            Err(e) => {
                self.status_msg = Some(e.to_string());
                return;
            }
        };
        if !tenx_core::dialog::permission_dialog_visible(&capture) {
            self.status_msg = Some(format!("'{title}': no permission dialog on screen — open it to check (⏎)"));
            return;
        }
        match crate::tmux::send_keys(&pane, answer.key()) {
            Ok(()) => self.status_msg = Some(format!("{} '{title}'", answer.verb())),
            Err(e) => self.status_msg = Some(e.to_string()),
        }
        self.refresh_statuses();
    }

    /// The selected row has a permission dialog the column can answer.
    fn selected_answerable(&self) -> bool {
        self.selected_row().is_some_and(|r| {
            r.status == TaskStatus::Blocked && r.waiting_for.as_deref() == Some(tenx_core::dialog::PERMISSION_PROMPT)
        })
    }

    // ── Jump ──────────────────────────────────────────────────────────────────

    /// Open the selected task — `open_in` selects (or creates) its window,
    /// which the embedded terminal shows — and hand the keyboard to it. The
    /// column stays in view: the rows keep the order they are on screen and
    /// the selection stays on the task just opened, so the next ↓ is the
    /// task below it. Only the filter goes, and the selection follows its
    /// row into the full list.
    fn jump(&mut self) -> Result<bool> {
        let Some(row) = self.selected_row() else {
            return Ok(false);
        };
        let ws_idx = row.ws_idx;
        let slug = row.slug.clone();
        if !self.offline {
            let ws = &self.workspaces[ws_idx];
            if let Err(e) = crate::cli::task::open_in(ws, &slug) {
                self.status_msg = Some(e.to_string());
                return Ok(false);
            }
        }
        self.current = Some(slug.clone());
        self.client_request = Some(ClientRequest::FocusTerminal);
        self.filter.clear();
        self.apply_filter();
        if let Some(pos) = self.filtered.iter().position(|&i| self.rows[i].slug == slug) {
            self.selected = pos;
        }
        self.focus_search();
        self.status_msg = None;
        Ok(false)
    }

    // ── Create ────────────────────────────────────────────────────────────────

    /// Workspace index of whatever is currently highlighted (task or repo).
    fn selected_ws_idx(&self) -> Option<usize> {
        match self.tab {
            Tab::Tasks => self.selected_row().map(|r| r.ws_idx),
            Tab::Repos => self
                .repo_filtered
                .get(self.repo_selected)
                .and_then(|&i| self.repo_rows.get(i))
                .map(|r| r.ws_idx),
        }
    }

    /// `:n` — open the new-task form in the selected item's workspace.
    fn start_create(&mut self) {
        let Some(ws_idx) = self.selected_ws_idx() else {
            self.status_msg = Some("select a task or repo first".into());
            return;
        };
        let repos = self.ws_repos(ws_idx);
        self.status_msg = None;
        self.mode = Mode::Create(CreateForm {
            ws_idx,
            name: String::new(),
            repos,
            focus: 0,
        });
    }

    fn ws_repos(&self, ws_idx: usize) -> Vec<(String, bool)> {
        self.workspaces
            .get(ws_idx)
            .map(|ws| ws.config.repos.iter().map(|r| (r.name.clone(), true)).collect())
            .unwrap_or_default()
    }

    fn handle_create_key(&mut self, key: KeyEvent) -> Result<bool> {
        let mut form = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::Create(f) => f,
            other => {
                self.mode = other;
                return Ok(false);
            }
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Ok(false), // cancel; mode is already List
            KeyCode::Enter => match self.submit_create(&form) {
                // Created and selected — go straight to it. `jump` handles
                // every surface (home pane, popup, plain terminal) and, when
                // it can't switch (a foreign tmux client), leaves the row
                // selected with a hint so Enter still gets you there.
                Ok(true) => return self.jump(),
                Ok(false) => return Ok(false), // created but not listed; stay in List
                Err(e) => self.status_msg = Some(e),
            },
            KeyCode::Tab | KeyCode::Down => form.focus_next(),
            KeyCode::BackTab | KeyCode::Up => form.focus_prev(),
            KeyCode::Char(' ') => {
                if form.focus >= 1 {
                    let i = form.focus - 1;
                    if i < form.repos.len() {
                        form.repos[i].1 = !form.repos[i].1;
                    }
                } else {
                    form.name.push(' ');
                }
            }
            KeyCode::Backspace => {
                if form.focus == 0 {
                    form.name.pop();
                }
            }
            KeyCode::Char(c) if !ctrl && form.focus == 0 => form.name.push(c),
            _ => {}
        }
        self.mode = Mode::Create(form);
        Ok(false)
    }

    /// Create the task and select its row. `Ok(true)` when the new row is now
    /// the selection (so a `jump` lands on it), `Ok(false)` if it couldn't be
    /// found in the rebuilt list.
    fn submit_create(&mut self, form: &CreateForm) -> Result<bool, String> {
        let name = form.name.trim().to_string();
        if name.is_empty() {
            return Err("task name cannot be empty".into());
        }
        let repos: Vec<String> = form
            .repos
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect();
        if repos.is_empty() {
            return Err("select at least one repo".into());
        }
        let ws_idx = form.ws_idx;
        let slug = crate::workspace::slugify(&name);
        if self.offline {
            // The demo: the task appears as a row, its agent already at work.
            let ws = &self.workspaces[ws_idx];
            let now = SystemTime::now();
            self.rows.push(Row {
                ws_idx,
                ws_name: ws.config.name.clone(),
                path: ws.dir.join("tasks").join(&slug),
                slug: slug.clone(),
                title: name.clone(),
                status: TaskStatus::Working,
                group: TaskStatus::Working,
                changed: Some(now),
                waiting_for: None,
                activity: now,
                window_id: Some("@new".into()),
                pane: None,
                live: crate::live::Live::default(),
                repos,
                agent: crate::agent::agent_for(ws, &ws.dir.join("tasks").join(&slug)),
                secrets_pending: vec![],
                secrets_pending_set: vec![],
                section: TaskStatus::Working.group(),
            });
            self.filter.clear();
            self.sort_rows();
            self.apply_filter();
        } else {
            // no_open=true: the window is opened by the `jump` the caller
            // runs right after this.
            let ws = &self.workspaces[ws_idx];
            crate::cli::task::new_in(ws, &name, Some(&repos), true).map_err(|e| e.to_string())?;
            self.filter.clear();
            self.rebuild_rows();
        }
        let selected = match self
            .filtered
            .iter()
            .position(|&i| self.rows[i].ws_idx == ws_idx && self.rows[i].slug == slug)
        {
            Some(pos) => {
                self.tab = Tab::Tasks;
                self.selected = pos;
                true
            }
            None => false,
        };
        self.status_msg = Some(format!("created '{name}'"));
        Ok(selected)
    }

    // ── Add repo (Repos tab) ──────────────────────────────────────────────────

    fn start_add_repo(&mut self) {
        let Some(ws_idx) = self.selected_ws_idx() else {
            self.status_msg = Some("select a repo first".into());
            return;
        };
        self.status_msg = None;
        self.mode = Mode::AddRepo(AddRepoForm {
            ws_idx,
            url: String::new(),
            name: String::new(),
            focus: 0,
        });
    }

    fn handle_addrepo_key(&mut self, key: KeyEvent) -> Result<bool> {
        let mut form = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::AddRepo(f) => f,
            other => {
                self.mode = other;
                return Ok(false);
            }
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Ok(false), // cancel; mode already List
            KeyCode::Enter => match self.submit_add_repo(&form) {
                Ok(()) => return Ok(false),
                Err(e) => self.status_msg = Some(e),
            },
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % 2,
            KeyCode::BackTab | KeyCode::Up => form.focus = if form.focus == 0 { 1 } else { 0 },
            KeyCode::Backspace => {
                if form.focus == 0 {
                    form.url.pop();
                } else {
                    form.name.pop();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if form.focus == 0 {
                    form.url.push(c);
                } else {
                    form.name.push(c);
                }
            }
            _ => {}
        }
        self.mode = Mode::AddRepo(form);
        Ok(false)
    }

    fn submit_add_repo(&mut self, form: &AddRepoForm) -> Result<(), String> {
        let url = form.url.trim().to_string();
        if url.is_empty() {
            return Err("git URL cannot be empty".into());
        }
        let name = form.name.trim();
        let name_opt = if name.is_empty() { None } else { Some(name) };
        {
            let ws = &mut self.workspaces[form.ws_idx];
            crate::cli::repo::add_in(ws, &url, name_opt).map_err(|e| e.to_string())?;
        }
        self.rebuild_repo_rows();
        self.status_msg = Some("repo added".into());
        Ok(())
    }

    // ── Edit repos (Tasks tab) ────────────────────────────────────────────────

    /// `e` / `:e` — open the repo checklist for the selected task, prefilled
    /// with the worktrees it already has.
    fn start_edit_repos(&mut self) {
        let Some(row) = self.selected_row() else {
            self.status_msg = Some("select a task first".into());
            return;
        };
        let (ws_idx, slug, title, have) =
            (row.ws_idx, row.slug.clone(), row.title.clone(), row.repos.clone());
        let mut picks: Vec<RepoPick> = self
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                ws.config
                    .repos
                    .iter()
                    .map(|r| RepoPick {
                        name: r.name.clone(),
                        checked: have.contains(&r.name),
                        present: have.contains(&r.name),
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A worktree whose repo has since left the workspace config still needs
        // a row, otherwise it could never be detached from here.
        for name in &have {
            if !picks.iter().any(|p| &p.name == name) {
                picks.push(RepoPick { name: name.clone(), checked: true, present: true });
            }
        }
        if picks.is_empty() {
            self.status_msg = Some("no repos in workspace — add one on the Repos tab".into());
            return;
        }
        self.status_msg = None;
        self.mode = Mode::EditRepos(EditReposForm {
            ws_idx,
            slug,
            title,
            picks,
            focus: 0,
            confirm: false,
        });
    }

    fn handle_editrepos_key(&mut self, key: KeyEvent) -> Result<bool> {
        let mut form = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::EditRepos(f) => f,
            other => {
                self.mode = other;
                return Ok(false);
            }
        };
        let n = form.picks.len();
        // Awaiting the destructive-change confirmation: only y/⏎ goes through.
        if form.confirm {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter) {
                self.apply_repo_changes(&form);
                return Ok(false);
            }
            form.confirm = false;
            self.mode = Mode::EditRepos(form);
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc => return Ok(false), // cancel; mode is already List
            KeyCode::Enter => {
                if form.added().is_empty() && form.removed().is_empty() {
                    self.status_msg = Some("no repo changes".into());
                    return Ok(false);
                }
                if form.desired().is_empty() {
                    self.status_msg = Some("a task must keep at least one repo".into());
                    self.mode = Mode::EditRepos(form);
                    return Ok(false);
                }
                // Detaching drops a worktree and its branch — confirm first.
                if form.removed().is_empty() {
                    self.apply_repo_changes(&form);
                } else {
                    form.confirm = true;
                    self.mode = Mode::EditRepos(form);
                }
                return Ok(false);
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => form.focus = (form.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                form.focus = if form.focus == 0 { n - 1 } else { form.focus - 1 }
            }
            KeyCode::Char(' ') | KeyCode::Char('x') => {
                form.picks[form.focus].checked = !form.picks[form.focus].checked;
            }
            KeyCode::Char('a') => form.picks.iter_mut().for_each(|p| p.checked = true),
            KeyCode::Char('n') => form.picks.iter_mut().for_each(|p| p.checked = false),
            _ => {}
        }
        self.mode = Mode::EditRepos(form);
        Ok(false)
    }

    /// Reconcile the task's worktrees to the checklist. `set_repos_in` does the
    /// diff again natively (it's the source of truth for what's on disk), so
    /// this just hands over the desired set.
    fn apply_repo_changes(&mut self, form: &EditReposForm) {
        let (added, removed) = (form.added().len(), form.removed().len());
        let res = {
            let ws = &self.workspaces[form.ws_idx];
            crate::cli::task::set_repos_in(ws, &form.slug, &form.desired(), false)
        };
        match res {
            Ok(()) => {
                let keep = form.slug.clone();
                self.rebuild_rows();
                if let Some(pos) = self.filtered.iter().position(|&i| self.rows[i].slug == keep) {
                    self.selected = pos;
                }
                self.status_msg = Some(match (added, removed) {
                    (a, 0) => format!("added {a} repo(s) to '{}'", form.title),
                    (0, r) => format!("detached {r} repo(s) from '{}'", form.title),
                    (a, r) => format!("added {a}, detached {r} in '{}'", form.title),
                });
            }
            Err(e) => self.status_msg = Some(e.to_string()),
        }
    }

    // ── Delete ────────────────────────────────────────────────────────────────

    fn start_delete(&mut self) {
        if let Some(r) = self.selected_row() {
            self.mode = Mode::Confirm(Confirm {
                ws_idx: r.ws_idx,
                slug: r.slug.clone(),
                title: r.title.clone(),
            });
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) {
        let confirm = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::Confirm(c) => c,
            other => {
                self.mode = other;
                return;
            }
        };
        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter) {
            self.status_msg = None; // cancelled
            return;
        }
        // Close the window first (best-effort) so it doesn't linger after the
        // dir goes. By live slug lookup only — never the cached id: tmux
        // reuses `@N` after a server restart, so a stale cache can name some
        // *other* task's window.
        if let Some(w) = crate::tmux::find_window(&confirm.slug).ok().flatten() {
            let _ = crate::tmux::kill_window(&w.id);
        }
        let res = {
            let ws = &self.workspaces[confirm.ws_idx];
            crate::cli::task::rm_in(ws, &confirm.slug, true)
        };
        match res {
            Ok(()) => {
                self.rebuild_rows();
                self.status_msg = Some(format!("deleted '{}'", confirm.title));
            }
            Err(e) => self.status_msg = Some(e.to_string()),
        }
    }

    // ── Close tab ─────────────────────────────────────────────────────────────

    fn close_selected_tab(&mut self) {
        let Some(r) = self.selected_row() else {
            return;
        };
        let path = r.path.clone();
        let slug = r.slug.clone();
        // The live window by slug is the only truth for a kill — a cached id
        // can belong to another task after a server restart.
        let id = crate::tmux::find_window(&slug).ok().flatten().map(|w| w.id);
        match id {
            Some(id) => match crate::tmux::kill_window(&id) {
                Ok(()) => {
                    let _ = std::fs::remove_file(path.join(crate::tmux::WINDOW_ID_FILE));
                    self.rebuild_rows();
                    self.status_msg = Some("closed window".into());
                }
                Err(e) => self.status_msg = Some(e.to_string()),
            },
            None => self.status_msg = Some("no open window".into()),
        }
    }

    /// Background idle-window sweep (`cli::task::sweep_quiet`), run when the
    /// client's terminal regains focus, at most once per `SWEEP_INTERVAL`.
    /// Silent on stdout by design (this runs inside the alternate screen); a
    /// nonzero result gets a status line instead.
    pub(super) fn maybe_sweep(&mut self) {
        if self.last_swept.is_some_and(|t| t.elapsed() < SWEEP_INTERVAL) {
            return;
        }
        self.last_swept = Some(Instant::now());
        let n = crate::cli::task::sweep_quiet(crate::cli::task::DEFAULT_SWEEP_AFTER);
        if n > 0 {
            self.status_msg = Some(format!("swept {n} idle tab{}", if n == 1 { "" } else { "s" }));
            self.rebuild_rows();
        }
    }

    // ── Secrets ───────────────────────────────────────────────────────────────

    /// Queue an unlock for the selected row, if it has a pending secrets
    /// request. Doesn't do the unlock itself — only `run_loop` has the
    /// `Terminal` handle needed to leave raw mode/the alternate screen, which
    /// the identity's passphrase prompt needs (a real controlling terminal,
    /// not a TUI's alternate screen buffer).
    fn start_unlock(&mut self) {
        let Some(r) = self.selected_row() else {
            return;
        };
        if r.secrets_pending.is_empty() && r.secrets_pending_set.is_empty() {
            self.status_msg = Some("no pending secrets for this task".into());
            return;
        }
        self.pending_unlock = Some((r.ws_idx, r.slug.clone()));
    }

    /// `:cancel` — withdraw every pending secrets request for the selected
    /// row, the human-side counterpart of `tenx secrets cancel --all`. Needs
    /// no terminal handoff (unlike `start_unlock`): it only edits the two
    /// queue files, so it runs inline and the row leaves SECRETS PENDING at
    /// once.
    fn cancel_secrets(&mut self) {
        let Some(r) = self.selected_row() else {
            return;
        };
        if r.secrets_pending.is_empty() && r.secrets_pending_set.is_empty() {
            self.status_msg = Some("no pending secrets for this task".into());
            return;
        }
        let (ws_idx, slug) = (r.ws_idx, r.slug.clone());
        let result = self
            .workspaces
            .get(ws_idx)
            .context("workspace no longer registered")
            .and_then(|ws| ws.find_task(&slug))
            .and_then(|task| crate::cli::secrets::cancel_in(&task, None));
        self.status_msg = Some(match result {
            Ok(()) => format!("withdrew pending secrets requests for '{slug}'"),
            Err(e) => e.to_string(),
        });
        self.rebuild_rows();
    }

    // ── Rename ────────────────────────────────────────────────────────────────

    fn start_rename(&mut self) {
        if let Some(r) = self.selected_row() {
            self.mode = Mode::Rename(RenameForm {
                slug: r.slug.clone(),
                path: r.path.clone(),
                buffer: r.title.clone(),
            });
        }
    }

    fn handle_rename_key(&mut self, key: KeyEvent) -> Result<bool> {
        let mut form = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::Rename(f) => f,
            other => {
                self.mode = other;
                return Ok(false);
            }
        };
        match key.code {
            KeyCode::Esc => return Ok(false),
            KeyCode::Enter => {
                let title = form.buffer.trim().to_string();
                if title.is_empty() {
                    self.status_msg = Some("title cannot be empty".into());
                    self.mode = Mode::Rename(form);
                    return Ok(false);
                }
                match crate::workspace::set_task_title(&form.path, &title) {
                    Ok(()) => {
                        // The zellij tab is named by the immutable slug, not the
                        // title, so a title change doesn't touch it — the header
                        // and lists read the title from TASK.md.
                        let keep = form.slug.clone();
                        self.rebuild_rows();
                        if let Some(pos) =
                            self.filtered.iter().position(|&i| self.rows[i].slug == keep)
                        {
                            self.selected = pos;
                        }
                        self.status_msg = Some("renamed".into());
                    }
                    Err(e) => self.status_msg = Some(e.to_string()),
                }
                return Ok(false);
            }
            KeyCode::Backspace => {
                form.buffer.pop();
            }
            KeyCode::Char(c) => {
                form.buffer.push(c);
            }
            _ => {}
        }
        self.mode = Mode::Rename(form);
        Ok(false)
    }
}

/// Case-insensitive subsequence match (fuzzy): are all chars of `needle` found
/// in `haystack` in order? Both are expected pre-lowercased.
fn subseq_match(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    for nc in needle.chars() {
        if nc == ' ' {
            continue;
        }
        loop {
            match hay.next() {
                Some(hc) if hc == nc => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

/// Suspend the TUI to run the real, interactive secrets fulfillment —
/// leaves raw mode and the alternate screen so `age`'s passphrase prompt (and
/// `set`'s own value prompt) reach this pane's *real* controlling terminal
/// (which is unaffected by raw-mode/alt-screen state either way, but the
/// TUI's own rendering would otherwise stomp all over the prompt while it's
/// waiting on input). This works whether the column is running in a plain
/// terminal or inside a zellij pane — either way it's a real interactive
/// terminal, which is all `age`/`set`'s own prompt need; nothing
/// zellij-specific about this path.
///
/// The plugin's only job here is spawning the real commands and getting out
/// of its way — same principle as the design's other unlock path (a spawned
/// pane in the `tenx-zellij` column, running the real CLI directly): this
/// function never touches the identity, the encrypted bundle, or a secret
/// value itself, it just hands the real terminal to the real `age`/`sops`
/// process. Delegates the actual sequencing (decrypt if release-pending,
/// then set once per pending value-name) to `cli::secrets::fulfill_in` —
/// shared with `tenx-zellij`'s spawned pane, which calls the same logic via
/// `tenx secrets fulfill` since it can only shell out, not link against
/// these functions directly. Keeping both callers on one implementation is
/// deliberate — see `fulfill_in`'s own doc comment.
pub(super) fn run_unlock(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    column: &mut Column,
    ws_idx: usize,
    slug: &str,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange)?;
    terminal.show_cursor()?;

    println!("secrets for '{slug}'...\n");
    let result = (|| -> Result<()> {
        let ws = column.workspaces.get(ws_idx).context("workspace no longer registered")?;
        let task = ws.find_task(slug)?;
        crate::cli::secrets::fulfill_in(ws, &task)
    })();
    match &result {
        Ok(()) => println!("\npress Enter to return"),
        Err(e) => println!("\ntenx: {e}\npress Enter to return"),
    }
    let mut discard = String::new();
    let _ = io::stdin().read_line(&mut discard);

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen, EnableMouseCapture, EnableFocusChange)?;
    terminal.clear()?;
    // Pending state changed (cleared on success) — rebuild so the row moves
    // out of SECRETS PENDING rather than showing a stale glyph until the next
    // reopen.
    column.rebuild_rows();
    Ok(())
}

/// Draw the column into `area` (the client's left-hand strip).
pub(super) fn render_in(f: &mut ratatui::Frame, column: &mut Column, area: Rect) {
    // Paint the ground first, so cells the widgets leave alone don't keep the
    // terminal's default background.
    f.render_widget(
        Block::default().style(Style::default().bg(palette::GROUND.color()).fg(palette::TEXT.color())),
        area,
    );
    // Dispatch on a discriminant (not `match &column.mode`) so the list path can
    // take `&mut column` without a live immutable borrow of `column.mode`.
    if matches!(column.mode, Mode::Create(_)) {
        render_create(f, column, area);
    } else if matches!(column.mode, Mode::AddRepo(_)) {
        render_addrepo(f, column, area);
    } else if matches!(column.mode, Mode::EditRepos(_)) {
        render_editrepos(f, column, area);
    } else {
        render_list(f, column, area);
    }
}

fn render_list(f: &mut ratatui::Frame, column: &mut Column, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Length(3), // search / rename box
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(area);

    let list_area = chunks[2];

    // Record clickable chrome/list areas for the mouse handler.
    column.tabs_area = chunks[0];
    column.search_area = chunks[1];
    column.list_area = list_area;

    // ── Tab bar (its own row, not on a border) ────────────────────────────────
    let tabs = Tabs::new(vec![" Tasks ", " Repos "])
        .select(if column.tab == Tab::Tasks { 0 } else { 1 })
        .style(Style::default().fg(palette::MUTED.color()))
        .highlight_style(Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD))
        .divider(Span::styled("│", Style::default().fg(palette::MUTED.color())));
    f.render_widget(tabs, chunks[0]);

    // ── Search box (or the rename input) ──────────────────────────────────────
    let title = if matches!(column.mode, Mode::Rename(_)) {
        " rename task "
    } else {
        ""
    };
    let (prefix, prefix_style, value) = match &column.mode {
        Mode::Rename(form) => ("✎ ", Style::default().fg(palette::ACCENT.color()), form.buffer.clone()),
        // `/` — the vim search prompt, a sibling of the `:` command line
        // below. A text glyph takes the accent colour and renders one column
        // wide everywhere; the emoji it replaced did neither.
        _ => ("/ ", Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD), column.filter.clone()),
    };
    // Real terminal cursor only when the cursor lives in the search field (or
    // renaming) — same condition the old `▏` fill-in bar used, but this is
    // the actual (blinking) terminal cursor now, so no fake glyph is drawn.
    // Column math uses `unicode-width` pinned to ratatui's own version (see
    // Cargo.toml) so it agrees with what `Paragraph` actually renders — the
    // rename prefix is a dingbat, so a plain `.chars().count()` could
    // misplace it by a column on some terminals.
    let show_cursor = matches!(column.mode, Mode::Rename(_)) || column.focus == Focus::Search;
    let top_spans = vec![Span::styled(prefix, prefix_style), Span::raw(value.clone())];
    let top = Paragraph::new(Line::from(top_spans))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())).title(title));
    f.render_widget(top, chunks[1]);
    if show_cursor {
        let col = chunks[1].x + 1 + (prefix.width() + value.width()) as u16;
        f.set_cursor_position((col, chunks[1].y + 1));
    }

    // ── Body list (tasks or repos) ────────────────────────────────────────────
    let list_width = list_area.width.saturating_sub(2) as usize;
    let (items, line_of_selected, line_to_pos) = match column.tab {
        Tab::Tasks => column_items(column, list_width),
        Tab::Repos => repo_items(column, list_width),
    };
    column.line_to_pos = line_to_pos;
    column.item_heights = items.iter().map(|i| i.height() as u16).collect();

    // Highlight a row only when the cursor is in the list (not the search field).
    column
        .list_state
        .select(if column.focus == Focus::List { line_of_selected } else { None });
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())))
        // A background bar: the row keeps its own status colours and chips
        // while selected, instead of collapsing into one accent colour.
        .highlight_style(Style::default().bg(palette::SEL_BG.color()))
        .highlight_spacing(HighlightSpacing::Never);
    f.render_stateful_widget(list, list_area, &mut column.list_state);

    // ── Footer / hints ────────────────────────────────────────────────────────
    let footer = match (&column.mode, &column.status_msg) {
        (Mode::Command(buf), _) => {
            let mut spans = vec![
                Span::styled(":", Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD)),
                Span::raw(buf.clone()),
                Span::styled("▏", Style::default().fg(palette::MUTED.color())),
            ];
            if buf.is_empty() {
                spans.push(Span::styled(
                    "  new · open · delete · rename · close · hide · quit",
                    Style::default().fg(palette::MUTED.color()),
                ));
            }
            Line::from(spans)
        }
        (Mode::Confirm(c), _) => Line::from(Span::styled(
            format!(" delete '{}' + worktrees?   y = delete   n/esc = cancel", c.title),
            Style::default().fg(palette::DANGER.color()).add_modifier(Modifier::BOLD),
        )),
        (Mode::Rename(_), _) => Line::from(Span::styled(
            " ⏎ save   esc cancel",
            Style::default().fg(palette::MUTED.color()),
        )),
        (_, Some(msg)) => Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(palette::SUCCESS.color()),
        )),
        _ => {
            // A column of ~36 cells: the mode tag and the two or three keys
            // that matter here; the full hint set lives on the wide surfaces.
            let (tag, tag_style) = mode_tag(column.input_mode);
            let hint = match (column.input_mode, column.tab) {
                (InputMode::Insert, _) => " filter · ↓↑ switch · ⏎ open",
                (InputMode::Normal, Tab::Tasks) if column.selected_answerable() => " y/N answer · ⏎ open",
                (InputMode::Normal, Tab::Tasks) if column.selected_row().is_some_and(|r| r.window_id.is_none()) => {
                    " closed · ⏎ open · ↓↑ move"
                }
                (InputMode::Normal, Tab::Tasks) => " ↓↑ switch · ⏎ open · n new · x close",
                (InputMode::Normal, Tab::Repos) => " a add-repo · gt tab",
            };
            Line::from(vec![Span::styled(tag, tag_style), Span::styled(hint, Style::default().fg(palette::MUTED.color()))])
        }
    };
    f.render_widget(Paragraph::new(footer), chunks[3]);
}

/// The footer's mode tag: INSERT on green, NORMAL on blue.
fn mode_tag(mode: InputMode) -> (&'static str, Style) {
    match mode {
        InputMode::Insert => (
            " INSERT ",
            Style::default().fg(palette::GROUND.color()).bg(palette::SUCCESS.color()).add_modifier(Modifier::BOLD),
        ),
        InputMode::Normal => (
            " NORMAL ",
            Style::default().fg(palette::GROUND.color()).bg(palette::INFO.color()).add_modifier(Modifier::BOLD),
        ),
    }
}

/// Pad `s` with spaces to exactly `w` chars, truncating with `…` if longer.
fn pad_cell(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n > w {
        let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

/// Header colour per section: amber for the pile that wants you, blue for
/// what's running, grey for what isn't.
fn group_color(group: workspace::TaskGroup) -> ratatui::style::Color {
    match group {
        // Same reasoning as the status bar's glyph priority: a pending
        // secrets request needs a specific action from you, distinct from
        // ordinary waiting — worth its own colour, not folded into WARN.
        workspace::TaskGroup::SecretsPending => palette::ACCENT.color(),
        workspace::TaskGroup::Waiting => palette::WARN.color(),
        workspace::TaskGroup::Working => palette::INFO.color(),
        workspace::TaskGroup::Inactive => palette::MUTED.color(),
    }
}

/// status colour, plus a gap. Shared by both list shapes.
fn row_glyph(row: &Row) -> (String, Style) {
    if !row.secrets_pending.is_empty() || !row.secrets_pending_set.is_empty() {
        ("🔒 ".to_string(), Style::default().fg(palette::ACCENT.color()))
    } else {
        (format!("{}  ", row.status.glyph()), Style::default().fg(palette::status_color(row.status).color()))
    }
}

/// What a row wants from you, as chip text with its colours: the secrets it
/// wants unlocked, else Claude Code's own waiting reason. `None` when it
/// wants nothing.
fn row_reason(row: &Row) -> Option<(String, &'static palette::Rgb, &'static palette::Rgb)> {
    if !row.secrets_pending.is_empty() || !row.secrets_pending_set.is_empty() {
        let wants: Vec<String> = row
            .secrets_pending
            .iter()
            .cloned()
            .chain(row.secrets_pending_set.iter().map(|n| format!("{n} (needs value)")))
            .collect();
        Some((format!("wants {}", wants.join(", ")), &palette::ACCENT, &palette::CHIP_SECRETS_BG))
    } else {
        row.waiting_for.as_deref().map(|r| (r.to_string(), &palette::WARN, &palette::CHIP_INPUT_BG))
    }
}

/// The column's list: the groups and rows of the task list, shaped for
/// a column of 30–48 cells. Each task is two lines — the glyph and the
/// bold title on the first, taking the whole width; on the second, muted
/// and indented under the title, the workspace, the age of a resting task,
/// then the PR, port and reason chips, each kept only if it fits whole.
/// The current task's title takes the "current" chip's colour instead of a
/// chip. No spacer between tasks: the headers already separate the groups,
/// and a column has less height to spare than width.
fn column_items(
    column: &Column,
    list_width: usize,
) -> (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>) {
    const INDENT: usize = 2 + 3; // indent + glyph column
    let mut items = Vec::new();
    let mut line_to_pos: Vec<Option<usize>> = Vec::new();
    let mut selected_line = None;

    let mut group_counts: [usize; 4] = [0; 4];
    for &i in &column.filtered {
        group_counts[column.rows[i].section.rank() as usize] += 1;
    }
    let mut last_group: Option<workspace::TaskGroup> = None;
    let dim = Style::default().fg(palette::MUTED.color());

    for (pos, &row_idx) in column.filtered.iter().enumerate() {
        let row = &column.rows[row_idx];
        let group = row.section;
        if last_group != Some(group) {
            if last_group.is_some() {
                items.push(ListItem::new(Line::from("")));
                line_to_pos.push(None);
            }
            let count = group_counts[group.rank() as usize];
            items.push(ListItem::new(Line::from(vec![
                Span::styled(group.label().to_string(), Style::default().fg(group_color(group)).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {count}"), dim),
            ])));
            line_to_pos.push(None);
            last_group = Some(group);
        }
        if pos == column.selected {
            selected_line = Some(items.len());
        }

        let selected = pos == column.selected && column.focus == Focus::List;
        let is_current = column.current.as_deref() == Some(row.slug.as_str());
        // Closed tasks (no window) read dimmer; ⏎ opens them.
        let title_fg = if selected {
            palette::SEL_TEXT.color()
        } else if is_current {
            palette::CURRENT.color()
        } else if row.window_id.is_none() {
            palette::MUTED.color()
        } else {
            palette::TEXT.color()
        };
        let (glyph, glyph_style) = row_glyph(row);
        // Sized per row, not per list: a column has no other columns to line
        // up with, so every title gets the whole width.
        let title_w = list_width.saturating_sub(INDENT).max(1);
        let first = Line::from(vec![
            Span::raw("  "),
            Span::styled(glyph, glyph_style),
            Span::styled(pad_cell(&row.title, title_w), Style::default().fg(title_fg).add_modifier(Modifier::BOLD)),
        ]);

        // Second line: pieces in priority order, each dropped whole when it
        // no longer fits, separated by a muted dot. What the task wants from
        // you comes first — it is the reason to look at the row at all — then
        // the workspace, the age, and the live chips.
        let mut pieces: Vec<Span<'static>> = Vec::new();
        if let Some((label, fg, bg)) = row_reason(row) {
            pieces.push(Span::styled(
                format!(" {label} "),
                Style::default().fg(fg.color()).bg(bg.color()).add_modifier(Modifier::BOLD),
            ));
        }
        pieces.push(Span::styled(row.ws_name.clone(), dim));
        // Name the agent when it isn't the default — a Codex or pi task reads as
        // such; Claude rows stay unadorned.
        if row.agent != crate::agent::AgentKind::Claude {
            pieces.push(Span::styled(format!(" {}", row.agent.as_str()), Style::default().fg(palette::INFO.color())));
        }
        if matches!(row.status, TaskStatus::Blocked | TaskStatus::Signaled | TaskStatus::Done)
            && let Some(changed) = row.changed
        {
            pieces.push(Span::styled(workspace::format_age(changed), dim));
        }
        for pr in &row.live.prs {
            let color = match pr.checks.as_str() {
                "failure" => palette::DANGER.color(),
                "success" => palette::SUCCESS.color(),
                _ => palette::INFO.color(),
            };
            pieces.push(Span::styled(pr.chip(), Style::default().fg(color)));
        }
        if !row.live.ports.is_empty() {
            let ports: Vec<String> = row.live.ports.iter().map(|p| format!(":{p}")).collect();
            pieces.push(Span::styled(ports.join(" "), dim));
        }
        let mut second = vec![Span::raw(" ".repeat(INDENT))];
        let mut used = INDENT;
        for (i, piece) in pieces.into_iter().enumerate() {
            let sep = if i == 0 { 0 } else { 3 };
            let w = piece.width();
            if used + sep + w > list_width {
                // Truncate the very first piece rather than show nothing;
                // skip any later piece that doesn't fit whole, and keep
                // going — a short age can still follow a long workspace.
                if i == 0 && list_width > INDENT + 1 {
                    let room = list_width - INDENT;
                    second.push(Span::styled(truncate(&piece.content, room), piece.style));
                    used = list_width;
                }
                continue;
            }
            if sep > 0 {
                second.push(Span::styled(" · ", dim));
            }
            used += sep + w;
            second.push(piece);
        }
        items.push(ListItem::new(vec![first, Line::from(second)]));
        line_to_pos.push(Some(pos));
    }

    if items.is_empty() {
        for line in empty_state_lines() {
            items.push(ListItem::new(line));
            line_to_pos.push(None);
        }
    }
    (items, selected_line, line_to_pos)
}

/// The first-run screen: the mark (`docs/logo/tenx-mark.svg`) drawn in text
/// with the same colours — two bright bars, the amber dot, two muted bars —
/// next to the wordmark and the one thing there is to do.
fn empty_state_lines() -> Vec<Line<'static>> {
    let bright = Style::default().fg(palette::TEXT.color());
    let muted = Style::default().fg(palette::MUTED.color());
    let dot = Style::default().fg(palette::WARN.color());
    let word = Style::default().fg(palette::BRIGHT.color()).add_modifier(Modifier::BOLD);
    let x = Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD);
    vec![
        Line::from(""),
        Line::from(vec![Span::raw("   "), Span::styled("━━━━━━━", bright)]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("━━━━ ", bright),
            Span::styled("●", dot),
            Span::raw("       "),
            Span::styled("ten", word),
            Span::styled("x", x),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("━━━━━━", muted),
            Span::raw("       "),
            Span::styled("no tasks yet — :n to create one", muted),
        ]),
        Line::from(vec![Span::raw("   "), Span::styled("━━━", muted)]),
    ]
}

/// Build the grouped repo list (lean: clone dot + last commit).
fn repo_items(
    column: &Column,
    list_width: usize,
) -> (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>) {
    let mut items = Vec::new();
    let mut line_to_pos: Vec<Option<usize>> = Vec::new();
    let mut selected_line = None;
    let mut last_ws: Option<usize> = None;

    for (pos, &idx) in column.repo_filtered.iter().enumerate() {
        let r = &column.repo_rows[idx];
        if last_ws != Some(r.ws_idx) {
            if last_ws.is_some() {
                items.push(ListItem::new(Line::from("")));
                line_to_pos.push(None);
            }
            items.push(ListItem::new(Line::from(Span::styled(
                r.ws_name.clone(),
                Style::default().fg(palette::WARN.color()).add_modifier(Modifier::BOLD),
            ))));
            line_to_pos.push(None);
            last_ws = Some(r.ws_idx);
        }
        if pos == column.repo_selected {
            selected_line = Some(items.len());
        }

        let (dot, dot_style, name_style, detail) = if r.cloned {
            (
                "● ",
                Style::default().fg(palette::SUCCESS.color()),
                Style::default().fg(palette::BRIGHT.color()),
                r.commit.clone().unwrap_or_else(|| "—".into()),
            )
        } else {
            (
                "○ ",
                Style::default().fg(palette::MUTED.color()),
                Style::default().fg(palette::MUTED.color()),
                "not cloned".into(),
            )
        };
        let left_w = 2 + 2 + r.name.chars().count() + 3;
        let detail = truncate(&detail, list_width.saturating_sub(left_w));
        items.push(ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(dot, dot_style),
            Span::styled(r.name.clone(), name_style),
            Span::raw("   "),
            Span::styled(detail, Style::default().fg(palette::MUTED.color())),
        ])));
        line_to_pos.push(Some(pos));
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  no repos",
            Style::default().fg(palette::MUTED.color()),
        ))));
        line_to_pos.push(None);
    }
    (items, selected_line, line_to_pos)
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let cut: String = chars[..max.saturating_sub(1)].iter().collect();
    format!("{cut}…")
}

fn render_addrepo(f: &mut ratatui::Frame, column: &Column, area: Rect) {
    let Mode::AddRepo(form) = &column.mode else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let ws_name = column
        .workspaces
        .get(form.ws_idx)
        .map(|w| w.config.name.clone())
        .unwrap_or_default();

    let lines = vec![
        Line::from(vec![
            Span::styled("  workspace  ", Style::default().fg(palette::MUTED.color())),
            Span::styled(ws_name, Style::default().fg(palette::WARN.color()).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        field_line(form.focus == 0, "git URL", &format!("{}{}", form.url, cursor(form.focus == 0))),
        Line::from(""),
        field_line(
            form.focus == 1,
            "name",
            &format!("{}{}   (optional — inferred from URL)", form.name, cursor(form.focus == 1)),
        ),
    ];

    let body = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())).title(" add repo "));
    f.render_widget(body, chunks[0]);

    let footer = if let Some(msg) = &column.status_msg {
        Line::from(Span::styled(format!(" {msg}"), Style::default().fg(palette::DANGER.color())))
    } else {
        Line::from(Span::styled(
            " ⏎ clone & add   esc cancel   ⇥ next field",
            Style::default().fg(palette::MUTED.color()),
        ))
    };
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

fn cursor(on: bool) -> &'static str {
    if on {
        "▏"
    } else {
        ""
    }
}

/// Repo checklist for an existing task: ticked rows the task already has read
/// as "worktree", the pending diff is called out per row, and detaching is
/// spelled out in the footer before it's confirmed.
fn render_editrepos(f: &mut ratatui::Frame, column: &Column, area: Rect) {
    let Mode::EditRepos(form) = &column.mode else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  task  ", Style::default().fg(palette::MUTED.color())),
            Span::styled(
                form.title.clone(),
                Style::default().fg(palette::WARN.color()).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];
    for (i, p) in form.picks.iter().enumerate() {
        let focused = form.focus == i;
        let check = if p.checked { "[x]" } else { "[ ]" };
        let prefix = if focused { "▸ " } else { "  " };
        let style = if focused {
            Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD)
        } else if p.checked {
            Style::default().fg(palette::BRIGHT.color())
        } else {
            Style::default().fg(palette::MUTED.color())
        };
        let (note, note_style) = match (p.checked, p.present) {
            (true, true) => ("worktree", Style::default().fg(palette::MUTED.color())),
            (true, false) => ("+ add", Style::default().fg(palette::SUCCESS.color())),
            (false, true) => ("− detach", Style::default().fg(palette::DANGER.color())),
            (false, false) => ("", Style::default()),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{prefix}{check} {}", pad_cell(&p.name, 24)), style),
            Span::styled(note, note_style),
        ]));
    }

    let body =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())).title(" task repos "));
    f.render_widget(body, chunks[0]);

    let footer = if form.confirm {
        Line::from(Span::styled(
            format!(
                " detach {} — removes the worktree AND its '{}' branch.   y = apply   esc = back",
                form.removed().join(", "),
                form.slug
            ),
            Style::default().fg(palette::DANGER.color()).add_modifier(Modifier::BOLD),
        ))
    } else if let Some(msg) = &column.status_msg {
        Line::from(Span::styled(format!(" {msg}"), Style::default().fg(palette::DANGER.color())))
    } else {
        Line::from(Span::styled(
            " ⏎ apply   esc cancel   space toggle   a all / n none",
            Style::default().fg(palette::MUTED.color()),
        ))
    };
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

fn render_create(f: &mut ratatui::Frame, column: &Column, area: Rect) {
    let Mode::Create(form) = &column.mode else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let ws_name = column
        .workspaces
        .get(form.ws_idx)
        .map(|w| w.config.name.clone())
        .unwrap_or_default();

    let mut lines = vec![
        // Chosen workspace shown as context (picked in the previous step).
        Line::from(vec![
            Span::styled("  workspace  ", Style::default().fg(palette::MUTED.color())),
            Span::styled(ws_name, Style::default().fg(palette::WARN.color()).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        field_line(form.focus == 0, "name", &format!("{}▏", form.name)),
        Line::from(""),
        Line::from(Span::styled("  repos", Style::default().fg(palette::MUTED.color()))),
    ];
    for (i, (name, on)) in form.repos.iter().enumerate() {
        let focused = form.focus == 1 + i;
        let check = if *on { "[x]" } else { "[ ]" };
        let prefix = if focused { "▸ " } else { "  " };
        let style = if focused {
            Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD)
        } else if *on {
            Style::default().fg(palette::BRIGHT.color())
        } else {
            Style::default().fg(palette::MUTED.color())
        };
        lines.push(Line::from(Span::styled(format!("{prefix}{check} {name}"), style)));
    }

    let body = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())).title(" new task "));
    f.render_widget(body, chunks[0]);

    let footer = if let Some(msg) = &column.status_msg {
        Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(palette::DANGER.color()),
        ))
    } else {
        Line::from(Span::styled(
            " ⏎ create   esc cancel   ⇥ next   space toggle repo",
            Style::default().fg(palette::MUTED.color()),
        ))
    };
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

fn field_line<'a>(focused: bool, label: &str, value: &str) -> Line<'a> {
    let prefix = if focused { "▸ " } else { "  " };
    let label_style = if focused {
        Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette::MUTED.color())
    };
    let value_style = if focused {
        Style::default().fg(palette::BRIGHT.color()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette::TEXT.color())
    };
    Line::from(vec![
        Span::styled(format!("{prefix}{label}: "), label_style),
        Span::styled(value.to_string(), value_style),
    ])
}
