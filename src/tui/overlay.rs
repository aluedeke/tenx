//! Global task overlay: lists every task across every registered workspace in
//! one flat list ordered by last agent activity (workspace shown per row),
//! with each task's Claude-activity status. It's both a
//! switcher and a task manager — fuzzy-filter + Enter to jump, plus
//! Telescope-style create/delete/rename/close bindings on a selected row
//! (see `Focus`/`InputMode`: the search field is Insert, plain typing
//! filters; a list row is Normal, plain letters act — `n` new, `d`d delete).
//!
//! Single-session model: all tasks live as (invisible) windows in the one
//! global `tenx` tmux session. The overlay runs in two modes: as the session's
//! *home* window (`--home`, long-lived, jump switches windows without exiting)
//! and as the Ctrl+w popup (`tmux display-popup -E`: exits after a jump, and
//! tmux closes the popup with it). Same binary, same code — there is exactly
//! one overlay implementation.

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
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
    /// — what the preview panel shows and `y`/`N` answer. Refreshed on the
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
#[derive(Clone, Copy, PartialEq)]
enum InputMode {
    Insert,
    Normal,
}

/// Where the single cursor lives: the search field, or a list row. Invariant:
/// `Search` implies Insert mode (you can only type while focused on search).
#[derive(Clone, Copy, PartialEq)]
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

/// The preview panel's contents: the visible screen of the selected task's
/// Claude pane, captured with `tmux capture-pane -e` and kept as styled
/// lines. Per-client and read-only — the way to see what a blocked task is
/// asking without switching the session's window under every other client
/// (and without clearing the window's bell flag by visiting it).
#[derive(Default)]
struct Preview {
    /// The pane the lines came from; `None` when the selected task has no
    /// live session (or nothing is selected).
    pane: Option<String>,
    lines: Vec<Line<'static>>,
    /// The capture failed (pane gone) — distinct from an empty pane.
    gone: bool,
}

/// Capture at most this many lines; the panel shows the tail that fits.
const PREVIEW_LINES: usize = 80;
/// Side-by-side list and preview from this width; below it, stacked under
/// the list and only for a blocked task; none when the pane is too small.
const PREVIEW_SIDE_MIN_COLS: u16 = 96;
/// Side by side, the list is as wide as its widest row but never more than
/// this share of the body, nor narrower than this many columns.
const PREVIEW_LIST_MAX_PERCENT: u16 = 60;
const PREVIEW_LIST_MIN_COLS: u16 = 40;
const PREVIEW_STACK_MIN_ROWS: u16 = 26;

/// A task to land on after the overlay tears down its TUI. Recorded when the
/// user jumps while *outside* tmux (a plain terminal): there's no client to
/// switch in place, so we exit the overlay cleanly and then attach to the tenx
/// session with that window selected.
struct AttachTarget {
    window_id: Option<String>,
}

struct Overlay {
    /// Running as the session's home base pane (long-lived, never quits) rather
    /// than the Ctrl+w floating pane (exits after a jump).
    home: bool,
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
    /// Set when a jump from outside tmux should attach after teardown.
    attach: Option<AttachTarget>,
    /// Set by `start_unlock` (the `u` key / `:unlock`) to (workspace index,
    /// slug). `run_loop` checks this after every event and, when set,
    /// suspends the TUI (leaves raw mode/alt screen) to run the real
    /// interactive `cli::secrets::decrypt_in` — the identity's passphrase
    /// prompt needs a real controlling terminal, which the alternate screen
    /// isn't. Not handled inside `Overlay` itself because only `run_loop` has
    /// the `Terminal` handle needed to leave and re-enter raw mode.
    pending_unlock: Option<(usize, String)>,

    // Mouse support (list view only; the modal forms stay keyboard-only).
    // `list_state` persists the scroll offset so a click's row maps to a list
    // line; `line_to_pos` maps each rendered line back to its filtered position
    // (None for workspace-group headers and blank separators). The three areas
    // are the tab bar, search box, and list, recorded during render.
    list_state: ListState,
    line_to_pos: Vec<Option<usize>>,
    tabs_area: Rect,
    search_area: Rect,
    list_area: Rect,

    /// Last time the background idle-tab sweep ran (`maybe_sweep`), so a
    /// bouncy window manager sending repeated `FocusGained` events can't fire
    /// it more than once per `SWEEP_INTERVAL`. `None` until the first one.
    last_swept: Option<Instant>,

    /// Window signals as of the last slow refresh (see `refresh_statuses`).
    signals: workspace::Signals,
    /// Slug of the session's current window, if it's a task — gets the
    /// "current" chip.
    current: Option<String>,
    /// When the slow inputs (tmux, per-task cache files) were last re-read.
    slow_refreshed: Option<Instant>,
    /// What the preview panel shows for the selected task (see `Preview`).
    preview: Preview,
}

/// How often the idle tick re-reads the *slow* inputs — `tmux list-windows`
/// (a subprocess) and each row's cache files. The watcher only changes them
/// every 2 s, so asking more often than that is spawn churn for nothing; the
/// session registry (a directory read) is still checked every tick.
const SLOW_REFRESH: Duration = Duration::from_secs(2);

/// Minimum spacing between the home pane's automatic idle-tab sweeps — see
/// `Overlay::maybe_sweep`. `FocusGained` already only triggers a rescan, which
/// is cheap; this just keeps the sweep itself (a `zellij` subprocess call per
/// candidate) from re-running on every glance back at the terminal.
const SWEEP_INTERVAL: Duration = Duration::from_secs(300);

impl Overlay {
    fn new(home: bool) -> Self {
        let mut o = Self::empty(home);
        o.workspaces = workspace::registered_workspaces();
        o.rebuild_rows();
        o
    }

    /// An overlay with no workspaces and no rows, touching nothing outside the
    /// process — `new` fills it from the registry; the screenshot test fills
    /// it with fixtures.
    fn empty(home: bool) -> Self {
        Overlay {
            home,
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
            attach: None,
            pending_unlock: None,
            list_state: ListState::default(),
            line_to_pos: Vec::new(),
            tabs_area: Rect::default(),
            search_area: Rect::default(),
            list_area: Rect::default(),
            last_swept: None,
            signals: workspace::Signals::new(),
            current: None,
            slow_refreshed: None,
            preview: Preview::default(),
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
    /// (local git only); cached until the overlay is reopened.
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
    fn rebuild_rows(&mut self) {
        // One flat list across all workspaces, grouped by agent status
        // (`TaskStatus::rank` — needs-input first, idle last) and, within a
        // group, by last status change newest first. Tasks with no agent
        // activity yet fall back to creation time.
        let sessions = workspace::claude::sessions();
        self.refresh_windows();
        let signals = &self.signals;
        let mut rows: Vec<Row> = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for task in ws.tasks().unwrap_or_default() {
                let state = workspace::resolve_task_state(&task.path, &sessions, signals);
                let window_id = read_window_id(&task.path);
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
                    secrets_pending,
                    secrets_pending_set,
                    section,
                });
            }
        }
        rows.sort_by(|a, b| {
            a.section
                .rank()
                .cmp(&b.section.rank())
                .then(a.group.rank().cmp(&b.group.rank()))
                .then(b.activity.cmp(&a.activity))
        });
        self.rows = rows;
        self.apply_filter();
    }

    /// Idle-tick refresh: re-read each row's status/age/tab-id in place,
    /// WITHOUT re-sorting or re-discovering tasks. The list order is frozen
    /// while the overlay is showing (no rows shuffling under the cursor) and
    /// only recomputed when the list is (re)opened: floating overlay spawn,
    /// home-pane startup, regaining focus, returning after a jump, or a
    /// mutating action (create/delete/rename).
    fn refresh_statuses(&mut self) {
        let sessions = workspace::claude::sessions();
        let slow = self.slow_refreshed.is_none_or(|t| t.elapsed() >= SLOW_REFRESH);
        if slow {
            self.refresh_windows();
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
                r.window_id = read_window_id(&r.path);
                r.live = crate::live::read(&r.path);
            }
        }
        self.rows = rows;
    }

    /// One `list-windows` for both the bell signals and the current window.
    fn refresh_windows(&mut self) {
        let windows = crate::tmux::list_windows().unwrap_or_default();
        self.signals = crate::tmux::signals_from(&windows);
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
    fn nav_down(&mut self) {
        match self.focus {
            Focus::Search => {
                if self.cur_len() > 0 {
                    self.focus_list();
                    self.set_cur_sel(0);
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

    /// Up: within the list, move up; at the top, return to the search field
    /// (→ Insert). In Search, stay put.
    fn nav_up(&mut self) {
        if self.focus == Focus::List {
            if self.cur_sel() == 0 {
                self.focus_search();
            } else {
                self.set_cur_sel(self.cur_sel() - 1);
            }
        }
    }

    fn move_top(&mut self) {
        self.focus_list();
        self.set_cur_sel(0);
    }

    fn move_bottom(&mut self) {
        self.focus_list();
        self.set_cur_sel(self.cur_len().saturating_sub(1));
    }

    fn selected_row(&self) -> Option<&Row> {
        self.filtered.get(self.selected).and_then(|&i| self.rows.get(i))
    }

    // ── Mouse dispatch ────────────────────────────────────────────────────────

    /// Handle a mouse event in the list view (the modal forms stay
    /// keyboard-only). Wheel scrolls the selection; clicking a tab header, the
    /// search box, or a task/repo row focuses it. Deliberately NO click-to-jump:
    /// jumping runs `zellij action go-to-tab`, which zellij applies to the last
    /// client that pressed a *key* — mouse events don't update that, so a
    /// tap-triggered jump from a phone (with a desktop client also attached)
    /// would switch the desktop's tab instead of the phone's. Requiring ⏎ to
    /// jump guarantees the jumping client just sent a keystroke and is
    /// therefore the one zellij switches. Returns `Ok(true)` when the overlay
    /// should close (a jump completed).
    fn handle_mouse(&mut self, m: MouseEvent) -> Result<bool> {
        if !matches!(self.mode, Mode::List) {
            return Ok(false);
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.nav_down(),
            MouseEventKind::ScrollUp => self.nav_up(),
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
                } else if let Some(line) = mouse::item_at(
                    self.list_area,
                    1,
                    self.list_state.offset(),
                    1,
                    m.column,
                    m.row,
                ) && let Some(Some(pos)) = self.line_to_pos.get(line).copied()
                {
                    self.focus_list();
                    self.set_cur_sel(pos);
                }
            }
            _ => {}
        }
        Ok(false)
    }

    // ── Key dispatch ──────────────────────────────────────────────────────────

    /// Returns `Ok(true)` when the overlay should close.
    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
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
        // Home mode is the session's anchor pane — quitting would leave a dead
        // pane/tab behind. Jumps already stay open in home mode (see `jump`),
        // so any close reaching here is a quit key: swallow it with a hint.
        if close && self.home {
            self.status_msg =
                Some("home overlay — jump to a task instead (Ctrl+w toggles the floating overlay)".into());
            return Ok(false);
        }
        Ok(close)
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
    /// the overlay. Commands that open a sub-view set `self.mode` themselves.
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
            "q" | "quit" => return Ok(true),
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
            "o" | "open" => return self.jump(),
            other => self.status_msg = Some(format!("unknown command: :{other}")),
        }
        Ok(false)
    }

    // ── Preview ───────────────────────────────────────────────────────────────

    /// Re-capture the selected task's pane for the preview panel. Cheap to
    /// call after every event: without `force` it only spawns `tmux` when
    /// the selection moved to a different pane; the idle tick passes `force`
    /// so a blocked pane's dialog (and a working one's output) stay live.
    fn refresh_preview(&mut self, force: bool) {
        let pane = match (self.tab, &self.mode) {
            (Tab::Tasks, Mode::List | Mode::Command(_)) => self.selected_row().and_then(|r| r.pane.clone()),
            _ => None,
        };
        if pane == self.preview.pane && !force {
            return;
        }
        let Some(target) = pane.clone() else {
            self.preview = Preview::default();
            return;
        };
        match crate::tmux::capture_pane(&target) {
            Ok(text) => {
                let lines = tenx_core::dialog::preview_tail(&text, PREVIEW_LINES).into_iter().map(super::ansi::line).collect();
                self.preview = Preview { pane: Some(target), lines, gone: false };
            }
            Err(_) => self.preview = Preview { pane: Some(target), lines: vec![], gone: true },
        }
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
        let (title, path) = (row.title.clone(), row.path.clone());
        let sessions = workspace::claude::sessions();
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
        self.refresh_preview(true);
    }

    /// The selected row is waiting on some prompt (permission or otherwise).
    fn selected_blocked(&self) -> bool {
        self.tab == Tab::Tasks && self.selected_row().is_some_and(|r| r.status == TaskStatus::Blocked)
    }

    /// The selected row has a permission dialog the overlay can answer.
    fn selected_answerable(&self) -> bool {
        self.selected_row().is_some_and(|r| {
            r.status == TaskStatus::Blocked && r.waiting_for.as_deref() == Some(tenx_core::dialog::PERMISSION_PROMPT)
        })
    }

    // ── Jump ──────────────────────────────────────────────────────────────────

    fn jump(&mut self) -> Result<bool> {
        let Some(row) = self.selected_row() else {
            return Ok(false);
        };
        let ws_idx = row.ws_idx;
        let slug = row.slug.clone();

        if crate::tmux::inside_tenx_session() {
            // `open_in` selects the window for the session, which is what this
            // client (the home pane or the popup) is looking at.
            let ws = &self.workspaces[ws_idx];
            if let Err(e) = crate::cli::task::open_in(ws, &slug) {
                self.status_msg = Some(e.to_string());
                return Ok(false);
            }
            if self.home {
                // Stay alive as the session's home pane — tmux already
                // switched the current window to the task. Reset the filter
                // and re-sort by activity so the next visit starts fresh,
                // with the most recently active task selected on top.
                self.filter.clear();
                self.rebuild_rows();
                self.selected = 0;
                self.focus_search();
                self.status_msg = None;
                return Ok(false);
            }
            return Ok(true);
        }

        if crate::tmux::inside_any_tmux() {
            // A client of some other tmux server: nothing can switch it here.
            self.status_msg = Some(crate::tmux::foreign_client_hint());
            return Ok(false);
        }

        // Outside tmux entirely (a plain terminal) → can't switch in place.
        // Open/select the window now (the server may be up without a client
        // attached), record it, and close; `run()` attaches after the TUI
        // tears down so tmux gets a clean terminal.
        if crate::tmux::server_running() {
            let ws = &self.workspaces[ws_idx];
            if let Err(e) = crate::cli::task::open_in(ws, &slug) {
                self.status_msg = Some(e.to_string());
                return Ok(false);
            }
        }
        let window_id = crate::tmux::find_window(&slug).ok().flatten().map(|w| w.id);
        self.attach = Some(AttachTarget { window_id });
        Ok(true)
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
        // no_open=true: the window is opened by the `jump` the caller runs
        // right after this, which also knows which surface it's on (home
        // pane, popup, plain terminal) and switches/attaches accordingly.
        {
            let ws = &self.workspaces[ws_idx];
            crate::cli::task::new_in(ws, &name, Some(&repos), true).map_err(|e| e.to_string())?;
        }
        self.filter.clear();
        self.rebuild_rows();
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

    /// Background idle-tab sweep (`cli::task::sweep_quiet`), run on the home
    /// pane's regular focus-gained rescan — see `run_loop`. Only the home
    /// pane does this: the Ctrl+w floating overlay exits right after a jump,
    /// so it would just repeat the same sweep on every glance rather than
    /// amortize it. Silent on stdout by design (this runs inside the
    /// alternate screen); a nonzero result gets a status line instead, same
    /// as any other background-ish action here.
    fn maybe_sweep(&mut self) {
        if !self.home {
            return;
        }
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

// Note: the Ctrl+w popup's lifecycle is tmux's (`display-popup -E` closes it
// when this process exits), so there is no toggle state to keep here.

pub fn run(home: bool) -> Result<()> {
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange);
        orig(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableFocusChange)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut overlay = Overlay::new(home);
    let result = run_loop(&mut terminal, &mut overlay);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange)?;
    terminal.show_cursor()?;
    result?;

    // The TUI is fully torn down now. If a jump from outside tmux was queued,
    // attach to (or create) the tenx session; both exec() and replace this
    // process.
    if let Some(t) = overlay.attach.take() {
        if crate::tmux::server_running() {
            crate::tmux::attach_at(t.window_id.as_deref())?;
        } else {
            let bin = std::env::current_exe()?;
            crate::tmux::attach_or_create(&bin.to_string_lossy())?;
        }
    }
    Ok(())
}

/// The task's cached tmux window id, if any (`tmux::WINDOW_ID_FILE`).
fn read_window_id(task_dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(task_dir.join(crate::tmux::WINDOW_ID_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

const TICK: Duration = Duration::from_millis(500);

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    overlay: &mut Overlay,
) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, overlay))?;

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => {
                    if overlay.handle_key(key)? {
                        break;
                    }
                    overlay.refresh_preview(false);
                }
                Event::Mouse(m) => {
                    if overlay.handle_mouse(m)? {
                        break;
                    }
                    overlay.refresh_preview(false);
                }
                // Coming back to look at the overlay (home tab refocused, or
                // the terminal regained focus) counts as a reopen — recompute
                // the activity ordering once, here, not on every tick.
                Event::FocusGained if matches!(overlay.mode, Mode::List) => {
                    overlay.rebuild_rows();
                    overlay.maybe_sweep();
                }
                _ => {}
            }
        } else if matches!(overlay.mode, Mode::List) {
            // Idle tick — update status glyphs/ages in place; the row order
            // stays frozen (see `refresh_statuses`). The preview follows the
            // pane live at the same cadence.
            overlay.refresh_statuses();
            overlay.refresh_preview(true);
        }

        if let Some((ws_idx, slug)) = overlay.pending_unlock.take() {
            run_unlock(terminal, overlay, ws_idx, &slug)?;
        }
    }
    Ok(())
}

/// Suspend the TUI to run the real, interactive secrets fulfillment —
/// leaves raw mode and the alternate screen so `age`'s passphrase prompt (and
/// `set`'s own value prompt) reach this pane's *real* controlling terminal
/// (which is unaffected by raw-mode/alt-screen state either way, but the
/// TUI's own rendering would otherwise stomp all over the prompt while it's
/// waiting on input). This works whether the overlay is running in a plain
/// terminal or inside a zellij pane — either way it's a real interactive
/// terminal, which is all `age`/`set`'s own prompt need; nothing
/// zellij-specific about this path.
///
/// The plugin's only job here is spawning the real commands and getting out
/// of its way — same principle as the design's other unlock path (a spawned
/// pane in the `tenx-zellij` overlay, running the real CLI directly): this
/// function never touches the identity, the encrypted bundle, or a secret
/// value itself, it just hands the real terminal to the real `age`/`sops`
/// process. Delegates the actual sequencing (decrypt if release-pending,
/// then set once per pending value-name) to `cli::secrets::fulfill_in` —
/// shared with `tenx-zellij`'s spawned pane, which calls the same logic via
/// `tenx secrets fulfill` since it can only shell out, not link against
/// these functions directly. Keeping both overlays on one implementation is
/// deliberate — see `fulfill_in`'s own doc comment.
fn run_unlock(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    overlay: &mut Overlay,
    ws_idx: usize,
    slug: &str,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange)?;
    terminal.show_cursor()?;

    println!("secrets for '{slug}'...\n");
    let result = (|| -> Result<()> {
        let ws = overlay.workspaces.get(ws_idx).context("workspace no longer registered")?;
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
    overlay.rebuild_rows();
    Ok(())
}

// Responsive sizing note: the overlay pane's geometry is decided *before* this
// process exists — the tenx-zellij plugin computes breakpoints from the screen
// size and opens this TUI in a floating pane already at the right size. The TUI
// therefore always renders full-pane (`f.area()`).

fn render(f: &mut ratatui::Frame, overlay: &mut Overlay) {
    // Paint the ground first: cells left on the terminal's default background
    // render black inside a tmux popup (the popup frame, styled by tmux, is
    // charcoal), so without this the overlay looks like a hole in the popup.
    f.render_widget(
        Block::default().style(Style::default().bg(palette::GROUND.color()).fg(palette::TEXT.color())),
        f.area(),
    );
    // Dispatch on a discriminant (not `match &overlay.mode`) so the list path can
    // take `&mut overlay` without a live immutable borrow of `overlay.mode`.
    if matches!(overlay.mode, Mode::Create(_)) {
        render_create(f, overlay);
    } else if matches!(overlay.mode, Mode::AddRepo(_)) {
        render_addrepo(f, overlay);
    } else if matches!(overlay.mode, Mode::EditRepos(_)) {
        render_editrepos(f, overlay);
    } else {
        render_list(f, overlay);
    }
}

fn render_list(f: &mut ratatui::Frame, overlay: &mut Overlay) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Length(3), // search / rename box
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    // The body is the list plus, when there's room, the preview panel:
    // beside it on a wide terminal, under it on a tall narrow one (a phone),
    // absent on a small one.
    let (list_area, preview_area) = if chunks[2].width >= PREVIEW_SIDE_MIN_COLS {
        // The list takes only what its widest row needs (laid out as if it
        // had the whole width, so no column is dropped), capped so the
        // preview keeps a readable share; the preview gets the rest.
        let full = chunks[2].width.saturating_sub(2) as usize;
        let natural = match overlay.tab {
            Tab::Tasks => task_items(overlay, full).0,
            Tab::Repos => repo_items(overlay, full).0,
        }
        .iter()
        .map(|i| i.width())
        .max()
        .unwrap_or(0) as u16;
        let cap = chunks[2].width * PREVIEW_LIST_MAX_PERCENT / 100;
        let list_w = (natural + 2).clamp(PREVIEW_LIST_MIN_COLS.min(cap), cap);
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(list_w), Constraint::Min(20)])
            .split(chunks[2]);
        (split[0], Some(split[1]))
    } else if chunks[2].height >= PREVIEW_STACK_MIN_ROWS && overlay.selected_blocked() {
        // Narrow (a phone): the list keeps the whole screen, and the
        // preview only appears under it when the selected task has a
        // prompt to read — the one time the space is worth giving up.
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Percentage(40)])
            .split(chunks[2]);
        (split[0], Some(split[1]))
    } else {
        (chunks[2], None)
    };

    // Record clickable chrome/list areas for the mouse handler.
    overlay.tabs_area = chunks[0];
    overlay.search_area = chunks[1];
    overlay.list_area = list_area;

    // ── Tab bar (its own row, not on a border) ────────────────────────────────
    let tabs = Tabs::new(vec![" Tasks ", " Repos "])
        .select(if overlay.tab == Tab::Tasks { 0 } else { 1 })
        .style(Style::default().fg(palette::MUTED.color()))
        .highlight_style(Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD))
        .divider(Span::styled("│", Style::default().fg(palette::MUTED.color())));
    f.render_widget(tabs, chunks[0]);

    // ── Search box (or the rename input) ──────────────────────────────────────
    let title = if matches!(overlay.mode, Mode::Rename(_)) {
        " rename task "
    } else {
        ""
    };
    let (prefix, prefix_style, value) = match &overlay.mode {
        Mode::Rename(form) => ("✎ ", Style::default().fg(palette::ACCENT.color()), form.buffer.clone()),
        // `/` — the vim search prompt, a sibling of the `:` command line
        // below. A text glyph takes the accent colour and renders one column
        // wide everywhere; the emoji it replaced did neither.
        _ => ("/ ", Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD), overlay.filter.clone()),
    };
    // Real terminal cursor only when the cursor lives in the search field (or
    // renaming) — same condition the old `▏` fill-in bar used, but this is
    // the actual (blinking) terminal cursor now, so no fake glyph is drawn.
    // Column math uses `unicode-width` pinned to ratatui's own version (see
    // Cargo.toml) so it agrees with what `Paragraph` actually renders — the
    // rename prefix is a dingbat, so a plain `.chars().count()` could
    // misplace it by a column on some terminals.
    let show_cursor = matches!(overlay.mode, Mode::Rename(_)) || overlay.focus == Focus::Search;
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
    let (items, line_of_selected, line_to_pos) = match overlay.tab {
        Tab::Tasks => task_items(overlay, list_width),
        Tab::Repos => repo_items(overlay, list_width),
    };
    overlay.line_to_pos = line_to_pos;

    // Highlight a row only when the cursor is in the list (not the search field).
    overlay
        .list_state
        .select(if overlay.focus == Focus::List { line_of_selected } else { None });
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())))
        // A background bar: the row keeps its own status colours and chips
        // while selected, instead of collapsing into one accent colour.
        .highlight_style(Style::default().bg(palette::SEL_BG.color()))
        .highlight_spacing(HighlightSpacing::Never);
    f.render_stateful_widget(list, list_area, &mut overlay.list_state);

    if let Some(area) = preview_area {
        render_preview(f, overlay, area);
    }

    // ── Footer / hints ────────────────────────────────────────────────────────
    let footer = match (&overlay.mode, &overlay.status_msg) {
        (Mode::Command(buf), _) => {
            let mut spans = vec![
                Span::styled(":", Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD)),
                Span::raw(buf.clone()),
                Span::styled("▏", Style::default().fg(palette::MUTED.color())),
            ];
            if buf.is_empty() {
                spans.push(Span::styled(
                    "  new · open · delete · rename · close · quit",
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
            let (tag, tag_style) = match overlay.input_mode {
                InputMode::Insert => (
                    " INSERT ",
                    Style::default().fg(palette::GROUND.color()).bg(palette::SUCCESS.color()).add_modifier(Modifier::BOLD),
                ),
                InputMode::Normal => (
                    " NORMAL ",
                    Style::default().fg(palette::GROUND.color()).bg(palette::INFO.color()).add_modifier(Modifier::BOLD),
                ),
            };
            let hint = match (overlay.input_mode, overlay.tab) {
                (InputMode::Insert, _) => "  type to filter · ↓ list · ⏎ open · ⇥ tab",
                (InputMode::Normal, Tab::Tasks) if overlay.selected_answerable() => {
                    "  y approve · N deny · ⏎ open · j/k move · n new · dd del · r rename · x close · q quit"
                }
                (InputMode::Normal, Tab::Tasks) => {
                    "  j/k move · n new · e repos · u unlock · dd del · r rename · x close · gt tab · q quit"
                }
                (InputMode::Normal, Tab::Repos) => {
                    "  j/k move · ↑ search · a add-repo · gt tab · q quit"
                }
            };
            Line::from(vec![
                Span::styled(tag, tag_style),
                Span::styled(hint, Style::default().fg(palette::MUTED.color())),
            ])
        }
    };
    f.render_widget(Paragraph::new(footer), chunks[3]);
}

/// The preview panel: the tail of the selected task's Claude pane, or why
/// there is none. Lines are the pane's own, so a narrow panel truncates them
/// on the right — the dialog's question and options are left-aligned, which
/// is what matters.
fn render_preview(f: &mut ratatui::Frame, overlay: &Overlay, area: Rect) {
    let row = match overlay.tab {
        Tab::Tasks => overlay.selected_row(),
        Tab::Repos => None,
    };
    let title = match row {
        Some(r) => format!(" {} ", truncate(&r.title, area.width.saturating_sub(4) as usize)),
        None => " preview ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::BORDER.color()))
        .title(Span::styled(title, Style::default().fg(palette::MUTED.color())));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let muted = Style::default().fg(palette::MUTED.color());
    let placeholder = |msg: &str| Paragraph::new(Line::from(Span::styled(msg.to_string(), muted)));
    match (&overlay.preview.pane, row) {
        (None, Some(_)) => f.render_widget(placeholder("no live session"), inner),
        (None, None) => f.render_widget(placeholder("select a task"), inner),
        (Some(_), _) if overlay.preview.gone => f.render_widget(placeholder("pane closed"), inner),
        (Some(_), _) => {
            let lines = &overlay.preview.lines;
            let skip = lines.len().saturating_sub(inner.height as usize);
            let tail: Vec<Line<'static>> = lines[skip..].to_vec();
            f.render_widget(Paragraph::new(tail), inner);
        }
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

/// Build the task list grouped by status (needs-input first, idle last; see
/// `TaskStatus::rank`), rendered as fixed-width columns — glyph · title ·
/// workspace · open · age — so every column starts at the same x. Group headers
/// are interleaved as non-selectable lines. Returns items, the selected line
/// index, and a per-line map back to the filtered position (None for headers,
/// spacers and the empty-state line).
fn task_items(
    overlay: &Overlay,
    list_width: usize,
) -> (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>) {
    let mut items = Vec::new();
    let mut line_to_pos: Vec<Option<usize>> = Vec::new();
    let mut selected_line = None;

    // Column widths from the visible rows, degrading for narrow panes (phone
    // terminals can be ~40 cols): the workspace column fits its widest name
    // but never more than a third of the pane; the age and `open` columns are
    // dropped (age first) when they'd squeeze the title below a readable
    // minimum. Fixed parts: 2 indent + 3 glyph + 2-wide gaps between columns.
    const TITLE_MIN: usize = 12;
    let ws_w = overlay
        .filtered
        .iter()
        .map(|&i| overlay.rows[i].ws_name.chars().count())
        .max()
        .unwrap_or(0)
        .min(list_width / 3);
    let title_max = overlay
        .filtered
        .iter()
        .map(|&i| overlay.rows[i].title.chars().count())
        .max()
        .unwrap_or(0);
    let base = 2 + 3 + 2 + ws_w; // indent + glyph + gap + workspace
    let show_open = list_width.saturating_sub(base + 2 + 4) >= TITLE_MIN;
    let show_age = list_width.saturating_sub(base + 2 + 4 + 2 + 4) >= TITLE_MIN;
    let extras = if show_open { 2 + 4 } else { 0 } + if show_age { 2 + 4 } else { 0 };
    let title_w = title_max.min(list_width.saturating_sub(base + extras).max(8));

    // Rows arrive sorted by status rank, so each status is one contiguous run —
    // a header goes in wherever the status changes. Counts come from the
    // filtered set, so they describe what's actually on screen.
    let mut group_counts: [usize; 4] = [0; 4];
    for &i in &overlay.filtered {
        group_counts[overlay.rows[i].section.rank() as usize] += 1;
    }
    let mut last_group: Option<workspace::TaskGroup> = None;

    for (pos, &row_idx) in overlay.filtered.iter().enumerate() {
        let row = &overlay.rows[row_idx];
        let group = row.section;
        if last_group != Some(group) {
            if last_group.is_some() {
                items.push(ListItem::new(Line::from("")));
                line_to_pos.push(None);
            }
            let count = group_counts[group.rank() as usize];
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    group.label().to_string(),
                    Style::default()
                        .fg(group_color(group))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {count}"),
                    Style::default().fg(palette::MUTED.color()),
                ),
            ])));
            line_to_pos.push(None);
            last_group = Some(group);
        }
        if pos == overlay.selected {
            selected_line = Some(items.len());
        }

        // A pending secrets request takes glyph priority over the ordinary
        // Claude-session status — same reasoning as `tenx-statusbar`: it needs
        // a different action from you (unlocking) than approving a prompt
        // does, regardless of whether the task also happens to be idle.
        let has_secrets = !row.secrets_pending.is_empty() || !row.secrets_pending_set.is_empty();
        // Three columns: a one-column glyph (`TaskStatus::glyph`, shared with
        // the status line) in its status colour, plus a gap. The secrets
        // lock is the one emoji: it needs to look different from any status.
        let (glyph, glyph_style) = if has_secrets {
            ("🔒 ".to_string(), Style::default().fg(palette::ACCENT.color()))
        } else {
            (format!("{}  ", row.status.glyph()), Style::default().fg(palette::status_color(row.status).color()))
        };
        // Names are always bold — that's what made the old overlay read as
        // crisp; the status colour lives in the glyph and chip, not the name.
        let selected = pos == overlay.selected && overlay.focus == Focus::List;
        let title_style = Style::default()
            .fg(if selected { palette::SEL_TEXT.color() } else { palette::TEXT.color() })
            .add_modifier(Modifier::BOLD);
        // Age is only meaningful for resting states (how long it's waited/sat).
        let show_age = matches!(
            row.status,
            TaskStatus::Blocked | TaskStatus::Signaled | TaskStatus::Done
        );
        let age = row
            .changed
            .filter(|_| show_age)
            .map(workspace::format_age)
            .unwrap_or_default();
        let open_cell = if row.window_id.is_some() { "open" } else { "    " };

        let dim = Style::default().fg(palette::MUTED.color());
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(glyph, glyph_style),
            Span::styled(pad_cell(&row.title, title_w), title_style),
            Span::raw("  "),
            Span::styled(pad_cell(&row.ws_name, ws_w), dim),
        ];
        if show_open {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(open_cell, dim));
        }
        if show_age {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(age, dim));
        }
        // Live chips — PR state and listening ports — only when there's room
        // for the other extras too; a phone-width pane keeps the title.
        if show_open {
            for pr in &row.live.prs {
                let color = match pr.checks.as_str() {
                    "failure" => palette::DANGER.color(),
                    "success" => palette::SUCCESS.color(),
                    _ => palette::INFO.color(),
                };
                spans.push(Span::styled(format!("  {}", pr.chip()), Style::default().fg(color)));
            }
            if !row.live.ports.is_empty() {
                let ports: Vec<String> = row.live.ports.iter().map(|p| format!(":{p}")).collect();
                spans.push(Span::styled(format!("  {}", ports.join(" ")), dim));
            }
        }
        // Which secret(s) it wants takes priority over Claude Code's own
        // waiting-for reason — same priority as the glyph above, and for the
        // same reason: it's a different, more specific thing to act on.
        // One chip per row, on a tinted pill: what it wants unlocked, else
        // Claude Code's own words for what it's waiting on ("input needed",
        // the open dialog's label — you can tell a permission prompt from a
        // question without switching), else "current" for the window you're
        // sitting in.
        let chip: Option<(String, &palette::Rgb, &palette::Rgb)> = if has_secrets {
            let wants: Vec<String> = row
                .secrets_pending
                .iter()
                .cloned()
                .chain(row.secrets_pending_set.iter().map(|n| format!("{n} (needs value)")))
                .collect();
            Some((format!("wants {}", wants.join(", ")), &palette::ACCENT, &palette::CHIP_SECRETS_BG))
        } else if let Some(reason) = row.waiting_for.as_deref() {
            Some((reason.to_string(), &palette::WARN, &palette::CHIP_INPUT_BG))
        } else if overlay.current.as_deref() == Some(row.slug.as_str()) {
            Some(("current".to_string(), &palette::CURRENT, &palette::CHIP_CURRENT_BG))
        } else {
            None
        };
        if let Some((label, fg, bg)) = chip.filter(|_| show_open) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!(" {label} "),
                Style::default().fg(fg.color()).bg(bg.color()).add_modifier(Modifier::BOLD),
            ));
        }
        items.push(ListItem::new(Line::from(spans)));
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
    overlay: &Overlay,
    list_width: usize,
) -> (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>) {
    let mut items = Vec::new();
    let mut line_to_pos: Vec<Option<usize>> = Vec::new();
    let mut selected_line = None;
    let mut last_ws: Option<usize> = None;

    for (pos, &idx) in overlay.repo_filtered.iter().enumerate() {
        let r = &overlay.repo_rows[idx];
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
        if pos == overlay.repo_selected {
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

fn render_addrepo(f: &mut ratatui::Frame, overlay: &Overlay) {
    let Mode::AddRepo(form) = &overlay.mode else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let ws_name = overlay
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

    let footer = if let Some(msg) = &overlay.status_msg {
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
fn render_editrepos(f: &mut ratatui::Frame, overlay: &Overlay) {
    let Mode::EditRepos(form) = &overlay.mode else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

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
    } else if let Some(msg) = &overlay.status_msg {
        Line::from(Span::styled(format!(" {msg}"), Style::default().fg(palette::DANGER.color())))
    } else {
        Line::from(Span::styled(
            " ⏎ apply   esc cancel   space toggle   a all / n none",
            Style::default().fg(palette::MUTED.color()),
        ))
    };
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

fn render_create(f: &mut ratatui::Frame, overlay: &Overlay) {
    let Mode::Create(form) = &overlay.mode else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let ws_name = overlay
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

    let footer = if let Some(msg) = &overlay.status_msg {
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
