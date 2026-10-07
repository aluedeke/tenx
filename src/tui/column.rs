//! The column: every task across every registered workspace in one list,
//! sectioned by attention (secrets pending → waiting for input → working →
//! inactive), fuzzy-filtered, with each task's Claude-activity status. It is
//! both a switcher and a task manager — type to filter + Enter to open, plus
//! Telescope-style create/delete/rename/close bindings on a selected row
//! (see `Focus`/`InputMode`: the search field is Insert, plain typing
//! filters; a list row is Normal, plain letters act — `n` next task that
//! needs you, `A`/`D` answer, `dd` delete; `Ctrl+n` new from either).
//!
//! It is drawn by the client (`tui::client`) beside the embedded tmux
//! session, in the same process: a jump is the window switch (the terminal
//! shows it), quit keys hand focus to the task through `ClientRequest`, and
//! the selection survives switching because nothing restarts.

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph},
};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use unicode_width::UnicodeWidthStr;

use super::mouse;

#[cfg(test)]
mod demo;
#[cfg(test)]
mod screenshot;
pub(crate) mod view;
use crate::palette;
use crate::snapshot::{self, Row};
use crate::workspace::sessions::{Subagent, SubagentStatus};
use crate::workspace::{self, TaskStatus, Workspace};

/// Create-task form. `focus`: 0 = workspace picker, 1 = name,
/// 2.. = repo checkboxes, last = agent picker. The workspace starts as the
/// selected item's (or the first registered one) and ←/→ cycle it; the
/// repo checklist follows the chosen workspace.
struct CreateForm {
    ws_idx: usize,
    name: String,
    repos: Vec<(String, bool)>,
    /// The task's agent, or `None` to inherit the workspace/global default.
    agent: Option<crate::agent::AgentKind>,
    /// What `None` resolves to for the chosen workspace, read when the
    /// workspace is picked — shown beside "default".
    inherited: crate::agent::AgentKind,
    focus: usize,
}

impl CreateForm {
    const WORKSPACE: usize = 0;
    const NAME: usize = 1;

    fn field_count(&self) -> usize {
        // workspace, name, one per repo, then agent.
        3 + self.repos.len()
    }
    /// Focus index of the agent field (the last one).
    fn agent_field(&self) -> usize {
        2 + self.repos.len()
    }
    /// The repo checkbox under the cursor, if it is on one.
    fn repo_field(&self) -> Option<usize> {
        self.focus.checked_sub(2).filter(|&i| i < self.repos.len())
    }
    /// Cycle the workspace choice through the `count` registered ones.
    /// Returns whether it changed (the caller reloads the repo list).
    fn cycle_workspace(&mut self, count: usize, back: bool) -> bool {
        if count < 2 {
            return false;
        }
        let cur = self.ws_idx.min(count - 1);
        self.ws_idx = if back { (cur + count - 1) % count } else { (cur + 1) % count };
        self.ws_idx != cur
    }
    /// The agent choices, in the order ←/→ cycle them; `None` inherits the
    /// workspace/global default.
    const AGENTS: [Option<crate::agent::AgentKind>; 4] = [
        None,
        Some(crate::agent::AgentKind::Claude),
        Some(crate::agent::AgentKind::Codex),
        Some(crate::agent::AgentKind::Pi),
    ];

    /// Where the agent choice is in `AGENTS`.
    fn agent_index(&self) -> usize {
        Self::AGENTS.iter().position(|a| *a == self.agent).unwrap_or(0)
    }

    /// Cycle the agent choice: default → claude → codex → pi → default.
    fn cycle_agent(&mut self, back: bool) {
        let cur = self.agent_index();
        let n = Self::AGENTS.len();
        self.agent = Self::AGENTS[if back { (cur + n - 1) % n } else { (cur + 1) % n }];
    }
    fn agent_label(&self) -> String {
        self.agent.map(|k| k.as_str().to_string()).unwrap_or_else(|| "default".to_string())
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

/// New-workspace form: what `tenx init` asks, minus the per-machine agent
/// setup, which `tenx` self-heals on launch. Submitting calls
/// `cli::init::init_in`, the same step the CLI runs.
struct NewWorkspaceForm {
    /// Directory to create (`~` expanded); need not exist yet.
    path: String,
    /// Empty = the path's last segment, as the CLI does.
    name: String,
    /// One repo is enough for the first task; `a` adds more later.
    repo_url: String,
    /// Install /tenx and /standup (Claude, Codex, pi) and AGENTS.md.
    skills: bool,
    focus: usize,
}

impl NewWorkspaceForm {
    const FIELDS: usize = 4;
    const SKILLS: usize = 3;

    /// The text field under the cursor, if it is on one.
    fn field_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            0 => Some(&mut self.path),
            1 => Some(&mut self.name),
            2 => Some(&mut self.repo_url),
            _ => None,
        }
    }
}

/// Pending delete confirmation.
struct Confirm {
    ws_idx: usize,
    slug: String,
    title: String,
    /// The task's directory — its window's identity (see `tmux::find_task_window`).
    path: PathBuf,
}

/// Note prompt for rejecting a task's pending secrets requests (`D` on a
/// SECRETS PENDING row, `:reject`). The names are fixed when it opens; one
/// answered meanwhile is skipped by `deny_quiet`.
struct RejectForm {
    ws_idx: usize,
    slug: String,
    names: Vec<String>,
    buffer: String,
}

/// Rename-title form.
struct RenameForm {
    slug: String,
    path: PathBuf,
    buffer: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tab {
    Tasks,
    Repos,
    /// The long operations running off the UI thread, and the last few that
    /// finished. Its label carries a `[n]` of what is still going, so the
    /// count is visible from the other tabs without costing the list any rows.
    Work,
}

impl Tab {
    /// Left-to-right order, for cycling and for laying the bar out.
    const ALL: [Tab; 3] = [Tab::Tasks, Tab::Repos, Tab::Work];

    fn label(self) -> &'static str {
        match self {
            Tab::Tasks => "Tasks",
            Tab::Repos => "Repos",
            Tab::Work => "Work",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
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
    /// New-task form (workspace preselected from the current selection,
    /// changeable in the form).
    Create(CreateForm),
    /// Add-repo form (Repos tab), workspace from the selected repo.
    AddRepo(AddRepoForm),
    /// New-workspace form (`W`, `:init [path]`), from either tab.
    NewWorkspace(NewWorkspaceForm),
    /// Repo checklist for the selected task (add/detach worktrees).
    EditRepos(EditReposForm),
    Confirm(Confirm),
    Rename(RenameForm),
    Reject(RejectForm),
    /// `?` / `:help` — every key, from `KEYS`; the u16 is the scroll offset.
    Help(u16),
}

/// What the column asks the client to do, since it cannot move focus or
/// hide itself: the terminal is in the same process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientRequest {
    /// Put the keyboard in the embedded terminal (a jump landed, or a quit
    /// key: the column stays).
    FocusTerminal,
    /// `:hide` — take the column away.
    Hide,
    /// `:q` — leave the client altogether.
    Quit,
}

/// What the client needs to put a view in front of you: landing on a subagent
/// line (its agent view) or on a task line (back to main), ⏎ on either, `t`
/// on a subagent (`Column::take_agent_view`).
#[derive(Debug, Clone)]
pub(crate) struct AgentView {
    /// Switch Claude Code's own view in the session's pane
    /// (`tenx_core::agent_panel`), rather than open tenx's transcript window.
    pub(super) in_claude: bool,
    /// The session's main view rather than a subagent's.
    pub(super) main: bool,
    /// ⏎: once it's showing, hand the keyboard to it. Landing on a line only
    /// switches the view; the cursor stays in the column.
    pub(super) focus: bool,
    /// What its row in Claude Code's agent panel shows at first: its
    /// description, else its type.
    pub(super) label: String,
    /// Its type, and its place among its session's running subagents of that
    /// type in launch order (`nth` of `peers`) — how its row is found once
    /// Claude shows a live summary there instead of the description
    /// (`tenx_core::agent_panel::AgentRef`).
    pub(super) agent_type: String,
    pub(super) nth: Option<usize>,
    pub(super) peers: usize,
    /// The viewer's header: the subagent's label and type.
    pub(super) title: String,
    /// Its transcript, when there is one to follow (`t`, and the fallback
    /// when the agent view can't be reached).
    pub(super) transcript: Option<PathBuf>,
    /// Its harness (`claude`, `codex`, `pi`), which decides how the transcript reads.
    pub(super) agent: String,
    /// The session that spawned it — the viewer notes when it has gone.
    pub(super) session_pid: u32,
    /// The task directory, the viewer's working directory.
    pub(super) task_path: PathBuf,
}

pub(crate) struct Column {
    client_request: Option<ClientRequest>,
    /// No tmux, no registry: a switch or an answer only updates this
    /// struct. The README demo's mode; never set by the client.
    pub(super) offline: bool,
    /// The tmux session whose current window this column follows and
    /// switches: `tenx` for the terminal client, a grouped session of its
    /// own for each browser tab of `tenx web` (`in_session`).
    session: String,
    workspaces: Vec<Workspace>,
    /// The registry's entry names as of the last `reload_workspaces` — the
    /// slow refresh compares them with `workspace::registry_keys()` and
    /// reloads on any difference, so a workspace registered (`tenx init`)
    /// or pruned while the client runs is listed without a restart.
    registry: Vec<String>,
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
    /// The subagent under the cursor, by id, when the cursor is on one of the
    /// selected task's child lines rather than on the task itself. By id, not
    /// position, so it survives a tick that reorders the task's subagents; an
    /// id the selected row no longer lists reads as "on the task"
    /// (`selected_sub`).
    sub: Option<String>,
    /// A view to put in front of you, taken by the client
    /// (`take_agent_view`); the latest request wins.
    pending_view: Option<AgentView>,
    /// The selection (task slug, subagent id) the last view switch was asked
    /// for, so moving within it — or a no-op arrow at the bottom — asks for
    /// nothing again.
    last_follow: Option<(String, Option<String>)>,
    repo_rows: Vec<RepoRow>,
    repo_filtered: Vec<usize>,
    repo_selected: usize,
    status_msg: Option<String>,
    /// Long operations running off the UI thread — creating a task, adding a
    /// repo, reconciling a checklist, deleting a task — plus the last few that
    /// finished, kept so an outcome (especially a failure) can be read after
    /// the footer message has gone. The Work tab renders this list.
    ///
    /// Several may run at once: `git::lock_repo` serialises whatever actually
    /// collides, so two jobs on different repos have no reason to queue behind
    /// each other. See `tui::job`. Shared with other columns under `tenx web`
    /// (`with_jobs`), so every browser tab lists every job.
    jobs: super::job::Jobs,
    /// This column's id: the `owner` of the jobs it starts, the only column
    /// that runs their follow-up.
    id: u64,
    /// Jobs whose landing this column has already acted on (its own) or
    /// rebuilt its rows for (another column's).
    seen_landed: std::collections::HashSet<u64>,
    /// Position within the Work tab's list.
    work_selected: usize,
    /// Advances on `progress::TICK`, for anything that animates without new
    /// data: the pending row's glyph, the job panel's spinner and marquee.
    /// Paced on its own clock rather than per draw — the client redraws at
    /// ~30fps for the embedded terminal's sake, and a braille spinner at that
    /// rate is a blur.
    frame: usize,
    last_frame: Option<Instant>,
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
    /// Each tab's x-range in the bar, recorded during render so a click maps
    /// to the tab actually drawn there. The bar is laid out by hand (rather
    /// than by ratatui's `Tabs`) precisely so these are exact — the old
    /// half-the-width guess only ever worked for two tabs.
    tab_spans: Vec<(u16, u16)>,
    line_to_pos: Vec<Option<usize>>,
    /// The subagent each list item is, parallel to `line_to_pos` (`None` for
    /// task lines, headers and every other tab's items).
    line_to_sub: Vec<Option<String>>,
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

    /// Window signals and open windows as of the last slow refresh (see
    /// `refresh_statuses`) — what `window_of` matches a task against.
    windows: snapshot::Windows,
    /// Directory of the task in the session's current window, if it's a
    /// task — drawn with the "current" marker. A directory, not a slug: two
    /// workspaces can each have a task with the same slug.
    current: Option<PathBuf>,
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
        Self::in_session(crate::tmux::SESSION)
    }

    /// A column that follows and switches `session`'s current window — a
    /// session grouped with `tenx` (`tmux::new_grouped_session`), so moving
    /// through it leaves every other client where it is.
    pub(crate) fn in_session(session: &str) -> Self {
        let mut o = Self::empty();
        o.session = session.to_string();
        o.reload_workspaces();
        o.rebuild_rows();
        o
    }

    /// List (and start) jobs in `jobs` instead of a list of its own — how
    /// every `tenx web` tab shows the same Work tab.
    pub(crate) fn with_jobs(mut self, jobs: super::job::Jobs) -> Self {
        self.jobs = jobs;
        self
    }

    /// Put a job on the list as this column's own, as `start_job` does.
    #[cfg(test)]
    fn push_job(&mut self, mut job: super::job::Job) {
        job.owner = self.id;
        self.jobs.lock().push(job);
    }

    /// Re-read the workspace registry and remember what it listed. Rows are
    /// not rebuilt here (their `ws_idx` may be stale until they are), so
    /// every caller follows up with `rebuild_rows`/`tidy`. The Repos tab is
    /// derived from the same list, so it is rebuilt at once when it has
    /// been opened.
    fn reload_workspaces(&mut self) {
        self.workspaces = workspace::registered_workspaces();
        self.registry = workspace::registry_keys();
        if !self.repo_rows.is_empty() {
            self.rebuild_repo_rows();
        }
    }

    /// A row whose status moved it to another section since the rows were
    /// last built — the list's grouping is stale.
    pub(crate) fn sections_stale(&self) -> bool {
        self.rows.iter().any(|r| r.live_section() != r.section)
    }

    /// Rebuild (re-group and re-sort) while keeping the selection on the
    /// same task. The column calls this while the keyboard is elsewhere,
    /// so rows move only when nobody is moving through them.
    pub(crate) fn tidy(&mut self) {
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
    pub(crate) fn take_request(&mut self) -> Option<ClientRequest> {
        self.client_request.take()
    }

    /// Whether the list (not a form or the command line) is showing — when
    /// the idle tick may refresh rows.
    pub(crate) fn in_list_mode(&self) -> bool {
        matches!(self.mode, Mode::List)
    }

    pub(crate) fn take_agent_view(&mut self) -> Option<AgentView> {
        self.pending_view.take()
    }

    pub(crate) fn take_unlock(&mut self) -> Option<(usize, String)> {
        self.pending_unlock.take()
    }

    /// The task a `take_unlock` names, looked up now.
    pub(crate) fn unlock_task(&self, ws_idx: usize, slug: &str) -> Option<workspace::Task> {
        self.workspaces.get(ws_idx)?.find_task(slug).ok()
    }

    /// The session this column follows and switches (`in_session`).
    pub(crate) fn session(&self) -> &str {
        &self.session
    }

    /// Say something in the footer — how the client reports an outcome that
    /// arrived from off the key path (an unlock popup closing).
    pub(crate) fn set_status(&mut self, msg: String) {
        self.status_msg = Some(msg);
    }

    /// A column with no workspaces and no rows, touching nothing outside the
    /// process — `new` fills it from the registry; the screenshot test fills
    /// it with fixtures.
    fn empty() -> Self {
        Column {
            client_request: None,
            offline: false,
            session: crate::tmux::SESSION.to_string(),
            workspaces: vec![],
            registry: vec![],
            tab: Tab::Tasks,
            input_mode: InputMode::Insert,
            focus: Focus::Search,
            pending: None,
            rows: vec![],
            filter: String::new(),
            filtered: vec![],
            selected: 0,
            sub: None,
            pending_view: None,
            last_follow: None,
            repo_rows: vec![],
            repo_filtered: vec![],
            repo_selected: 0,
            status_msg: None,
            jobs: super::job::Jobs::default(),
            id: super::job::next_id(),
            seen_landed: std::collections::HashSet::new(),
            work_selected: 0,
            frame: 0,
            last_frame: None,
            mode: Mode::List,
            pending_unlock: None,
            list_state: ListState::default(),
            line_to_pos: Vec::new(),
            line_to_sub: Vec::new(),
            item_heights: Vec::new(),
            tabs_area: Rect::default(),
            tab_spans: Vec::new(),
            search_area: Rect::default(),
            list_area: Rect::default(),
            last_swept: None,
            windows: snapshot::Windows::default(),
            current: None,
            slow_refreshed: None,
        }
    }

    // ── Tabs ──────────────────────────────────────────────────────────────────

    /// `gt` / Tab forward, `gT` / BackTab back, through all three tabs.
    fn cycle_tab(&mut self, back: bool) {
        let n = Tab::ALL.len();
        let i = self.tab.index();
        let next = if back { (i + n - 1) % n } else { (i + 1) % n };
        self.select_tab(Tab::ALL[next]);
    }

    fn select_tab(&mut self, tab: Tab) {
        self.tab = tab;
        if self.tab == Tab::Repos && self.repo_rows.is_empty() {
            self.rebuild_repo_rows();
        }
        self.clamp_work_selection();
    }

    fn clamp_work_selection(&mut self) {
        let n = self.jobs.lock().len();
        if self.work_selected >= n {
            self.work_selected = n.saturating_sub(1);
        }
    }

    fn select_repos_tab(&mut self) {
        self.select_tab(Tab::Repos);
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
    pub(crate) fn rebuild_rows(&mut self) {
        // One flat list across all workspaces, grouped by agent status
        // (needs-input first, idle last) and, within a group, by last status
        // change newest first (`tenx_core::column::compare`).
        let sessions = workspace::sessions::sessions();
        self.refresh_windows();
        let rows = snapshot::rows(&self.workspaces, &sessions, &self.windows);
        // A task a job is still building has a directory only once the worker
        // gets that far, so carry its ghost row across the rebuild — but only
        // while the real row is absent, or the two would both be listed.
        let ghosts: Vec<Row> = std::mem::take(&mut self.rows)
            .into_iter()
            .filter(|g| g.pending && !rows.iter().any(|r| r.ws_idx == g.ws_idx && r.slug == g.slug))
            .collect();
        let mut rows = rows;
        rows.extend(ghosts);
        self.rows = rows;
        self.sort_rows();
        self.apply_filter();
        self.current = self.current_from(crate::tmux::current_window_id_in(&self.session));
    }

    fn sort_rows(&mut self) {
        snapshot::sort(&mut self.rows);
    }

    /// Idle-tick refresh: re-read each row's status/age/tab-id in place,
    /// WITHOUT re-sorting or re-discovering tasks. The list order is frozen
    /// while the column is showing (no rows shuffling under the cursor) and
    /// only recomputed when the list is (re)opened: floating column spawn,
    /// home-pane startup, regaining focus, returning after a jump, or a
    /// mutating action (create/delete/rename). The one exception is the
    /// slow refresh's two probes for things that happened outside this
    /// client — a workspace registered or pruned, a task directory created
    /// or removed — which rebuild the rows, keeping the selection on its
    /// task.
    pub(crate) fn refresh_statuses(&mut self) {
        let sessions = workspace::sessions::sessions();
        let slow = self.slow_refreshed.is_none_or(|t| t.elapsed() >= SLOW_REFRESH);
        if slow {
            self.refresh_windows();
            // A workspace registered from outside (`tenx init`, or `tenx`
            // run inside an unregistered workspace) has tasks this column
            // has never scanned: reload the list, then rebuild. One
            // `read_dir` of the registry.
            if workspace::registry_keys() != self.registry {
                self.reload_workspaces();
                self.tidy();
                return;
            }
            // A task created or removed from outside (the CLI, the skill,
            // another client) is not a row yet: rebuild, keeping the
            // selection on its task. One `read_dir` per workspace.
            // Ghost rows have no directory yet, so they are discounted here
            // — otherwise a running create job would trip this every tick.
            let on_disk: usize = self.workspaces.iter().map(|ws| ws.task_dir_count()).sum();
            let ghosts = self.rows.iter().filter(|r| r.pending).count();
            if on_disk != self.rows.len() - ghosts {
                self.tidy();
                return;
            }
            // A secrets request queued (an agent's `secrets need`) or answered
            // (another client, the CLI) since the rows were built: rebuild, so
            // the task moves into or out of SECRETS PENDING in every client —
            // the row's frozen queue fields never update on their own.
            if self.rows.iter().any(|r| r.secrets_changed_on_disk()) {
                self.tidy();
                return;
            }
        }
        let windows = slow.then_some(&self.windows);
        for r in self.rows.iter_mut() {
            r.refresh(&sessions, &self.windows.signals, windows);
        }
        // Fresher than the slow refresh's window list: the task beside the
        // column is what ↓/↑ start from.
        self.current = self.current_from(crate::tmux::current_window_id_in(&self.session));
    }

    /// One `list-windows` for both the bell signals and the open windows.
    fn refresh_windows(&mut self) {
        self.windows = snapshot::Windows::read_in(&self.session);
        self.slow_refreshed = Some(Instant::now());
    }

    /// The directory of the task whose window is `window_id`, if any row
    /// holds it.
    fn current_from(&self, window_id: Option<String>) -> Option<PathBuf> {
        let id = window_id?;
        self.rows.iter().find(|r| r.window_id.as_deref() == Some(id.as_str())).map(|r| r.path.clone())
    }

    /// Whether `row` is the task in the session's current window.
    fn is_current(&self, row: &Row) -> bool {
        self.current.as_deref() == Some(row.path.as_path())
    }

    fn apply_filter(&mut self) {
        let needle = self.filter.as_str();
        self.filtered = self.rows.iter().enumerate().filter(|(_, r)| r.matches(needle)).map(|(i, _)| i).collect();
        self.repo_filtered = self
            .repo_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                needle.is_empty() || tenx_core::column::filter_matches(needle, &format!("{} {}", r.ws_name, r.name))
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
            Tab::Work => self.jobs.lock().len(),
        }
    }

    fn cur_sel(&self) -> usize {
        match self.tab {
            Tab::Tasks => self.selected,
            Tab::Repos => self.repo_selected,
            Tab::Work => self.work_selected,
        }
    }

    fn set_cur_sel(&mut self, i: usize) {
        match self.tab {
            Tab::Tasks => {
                self.selected = i;
                self.sub = None;
            }
            Tab::Repos => self.repo_selected = i,
            Tab::Work => self.work_selected = i,
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
    pub(crate) fn select_current(&mut self) {
        if let Some(p) = self.own_row() {
            self.selected = p;
            self.focus_list();
        }
    }

    /// The keyboard left the column: drop the row highlight so the list
    /// shows no cursor while the task has it. Ctrl+w brings it back on the
    /// current task (`select_current`).
    pub(crate) fn blur(&mut self) {
        if matches!(self.mode, Mode::List) {
            self.focus_search();
            // A message is about what you just did here and has been read
            // by the time the keyboard leaves; Ctrl+w brings back a fresh
            // column, its footer showing the mode tag, not a stale line.
            self.status_msg = None;
        }
    }

    /// The selected task's title when it has no open window (and the list
    /// has the cursor) — what the client shows an empty screen for.
    pub(super) fn selected_closed(&self) -> Option<String> {
        self.selected_closed_row().map(|r| r.title.clone())
    }

    fn selected_closed_row(&self) -> Option<&Row> {
        if self.tab != Tab::Tasks || self.focus != Focus::List {
            return None;
        }
        self.selected_row().filter(|r| r.window_id.is_none())
    }

    /// Whether `row` is what the task area beside the column shows — the
    /// row that gets the "current" marker. That is the selected task's empty
    /// screen while the cursor rests on a closed task (`selected_closed`),
    /// else the session's current window: the marker says "this is what's
    /// on the right", so it moves onto a closed task as soon as the right
    /// side does.
    fn is_shown(&self, row: &Row) -> bool {
        match self.selected_closed_row() {
            Some(closed) => closed.path == row.path,
            None => self.is_current(row),
        }
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
                // Through the selected task's subagents before the next task.
                if self.tab == Tab::Tasks
                    && let Some(row) = self.selected_row()
                {
                    let next = self.selected_sub().map_or(0, |k| k + 1);
                    if let Some(a) = row.subagents.get(next) {
                        self.sub = Some(a.id.clone());
                        return;
                    }
                }
                let len = self.cur_len();
                let next = (self.cur_sel() + 1).min(len.saturating_sub(1));
                if len > 0 && next != self.cur_sel() {
                    self.set_cur_sel(next);
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
                if let Some(k) = self.selected_sub() {
                    self.sub = k.checked_sub(1).and_then(|p| self.selected_row().map(|r| r.subagents[p].id.clone()));
                } else if self.cur_sel() == 0 {
                    self.focus_search();
                } else {
                    self.set_cur_sel(self.cur_sel() - 1);
                    // Up into a task from below lands on its last subagent.
                    if self.tab == Tab::Tasks {
                        self.sub = self.selected_row().and_then(|r| r.subagents.last()).map(|a| a.id.clone());
                    }
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

    /// `n`: put the cursor on the next task that needs you, cycling through
    /// the filtered list, and show it — the same follow as ↓/↑, so `A`/`D`
    /// or ⏎ can act on it at once. From the search field (`:next`) the
    /// search starts at the top, so the first press lands on the most
    /// urgent task. Says so when nothing needs you.
    fn jump_needs_you(&mut self) {
        if !self.require_tasks() {
            return;
        }
        let needs: Vec<bool> = self.filtered.iter().map(|&i| self.rows[i].needs_you()).collect();
        let from = match self.focus {
            Focus::List => Some(self.selected),
            Focus::Search => None,
        };
        match tenx_core::column::next_needing(from, &needs) {
            Some(i) => {
                self.status_msg = None;
                self.focus_list();
                self.set_cur_sel(i);
                self.follow_selection();
            }
            None => self.status_msg = Some("nothing needs you".into()),
        }
    }

    /// Position (in `filtered`) of the task the column sits beside; `None`
    /// on the other surfaces, or when the filter hides it.
    fn own_row(&self) -> Option<usize> {
        if self.tab != Tab::Tasks {
            return None;
        }
        self.filtered.iter().position(|&i| self.is_current(&self.rows[i]))
    }

    /// The column follows its selection: moving onto a task whose window is
    /// open switches to that window, cmux-style — the embedded terminal
    /// shows it, and the column is the same process, so the selection
    /// simply carries on. A task with no window is only selected (the
    /// client shows an empty screen for it; ⏎ opens it). Never fires from
    /// the search field or off the Tasks tab.
    ///
    /// Within a task it follows the agents too: landing on a Claude Code
    /// subagent's line puts that subagent's view in the task's pane, landing
    /// on the task's own line puts the session's main view back
    /// (`follow_agent`). The cursor stays here either way; ⏎ moves it over.
    fn follow_selection(&mut self) {
        if self.tab != Tab::Tasks || self.focus != Focus::List {
            return;
        }
        let Some(row) = self.selected_row() else { return };
        if row.window_id.is_none() {
            return;
        }
        if !self.is_current(row) {
            let slug = row.slug.clone();
            let path = row.path.clone();
            if !self.offline {
                let Some(w) = crate::tmux::find_task_window(&slug, &path).ok().flatten() else { return };
                if crate::tmux::select_window_in(&self.session, &w.id).is_err() {
                    return;
                }
            }
            self.current = Some(path);
        }
        self.follow_agent();
    }

    /// Ask the client to switch the task's Claude pane to what the cursor is
    /// on: a Claude Code subagent's view, or — on the task's own line — the
    /// session's main view (which presses nothing when main is already up).
    /// Codex and pi subagents have no view to switch to; ⏎ or `t` open their
    /// transcript.
    fn follow_agent(&mut self) {
        let Some(row) = self.selected_row() else { return };
        let key = (row.slug.clone(), self.selected_subagent().map(|a| a.id.clone()));
        if self.last_follow.as_ref() == Some(&key) {
            return;
        }
        let view = if self.offline {
            None
        } else {
            match self.selected_subagent() {
            Some(a) if a.agent == "claude" => Some(self.agent_view(a, true, false)),
            Some(_) => None,
            None => row.subagents.iter().find(|a| a.agent == "claude").map(|a| AgentView {
                in_claude: true,
                main: true,
                focus: false,
                label: "main".into(),
                agent_type: String::new(),
                nth: None,
                peers: 0,
                title: String::new(),
                transcript: None,
                agent: a.agent.clone(),
                session_pid: a.session_pid,
                task_path: row.path.clone(),
            }),
            }
        };
        self.last_follow = Some(key);
        if view.is_some() {
            self.pending_view = view;
        }
    }

    /// The view request for subagent `a` of the selected task.
    fn agent_view(&self, a: &Subagent, in_claude: bool, focus: bool) -> AgentView {
        let title =
            if a.description.is_some() { format!(" {} · {} ", a.label(), a.agent_type) } else { format!(" {} ", a.label()) };
        // Its running peers of the same type in the same session, by launch.
        let mut peers: Vec<&Subagent> = self
            .selected_row()
            .map(|r| r.subagents.iter().collect())
            .unwrap_or_default();
        peers.retain(|p| {
            p.session_pid == a.session_pid && p.agent_type == a.agent_type && p.status != SubagentStatus::Finished
        });
        peers.sort_by_key(|p| p.started_at);
        AgentView {
            in_claude,
            main: false,
            focus,
            label: a.label().to_string(),
            agent_type: a.agent_type.clone(),
            nth: peers.iter().position(|p| p.id == a.id),
            peers: peers.len(),
            title,
            transcript: a.transcript_path.clone().filter(|p| self.offline || p.is_file()),
            agent: a.agent.clone(),
            session_pid: a.session_pid,
            task_path: self.selected_row().map(|r| r.path.clone()).unwrap_or_default(),
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        self.filtered.get(self.selected).and_then(|&i| self.rows.get(i))
    }

    /// Position of the subagent under the cursor in the selected task's list,
    /// or `None` when the cursor is on the task itself.
    fn selected_sub(&self) -> Option<usize> {
        if self.tab != Tab::Tasks || self.focus != Focus::List {
            return None;
        }
        let id = self.sub.as_deref()?;
        self.selected_row()?.subagents.iter().position(|a| a.id == id)
    }

    fn selected_subagent(&self) -> Option<&Subagent> {
        let k = self.selected_sub()?;
        self.selected_row()?.subagents.get(k)
    }

    /// ⏎ on a subagent line (`in_claude`): put it on screen in Claude Code's
    /// own agent view and hand the keyboard to that pane — a Claude Code
    /// subagent in an open window; anything else, or one Claude no longer
    /// lists, opens as its transcript. `t`: its transcript in a tmux window of
    /// its own. Says why when there is nothing to show.
    fn view_subagent(&mut self, in_claude: bool) {
        let Some(row) = self.selected_row() else { return };
        let Some(a) = self.selected_subagent() else { return };
        let in_claude = in_claude && a.agent == "claude" && row.window_id.is_some();
        let view = self.agent_view(a, in_claude, true);
        if !in_claude && view.transcript.is_none() {
            self.status_msg = Some(match a.transcript_path {
                // A pi subagent run with `--no-session` writes none.
                None => format!("'{}' keeps no transcript", a.label()),
                Some(_) => format!("no transcript for '{}' yet", a.label()),
            });
            return;
        }
        if self.offline {
            self.status_msg = Some(format!("following '{}'", a.label()));
            return;
        }
        self.pending_view = Some(view);
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
                    let rel = m.column.saturating_sub(self.tabs_area.x);
                    if let Some(i) = self.tab_spans.iter().position(|(a, b)| rel >= *a && rel < *b)
                        && let Some(tab) = Tab::ALL.get(i).copied()
                    {
                        self.select_tab(tab);
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
                    if self.tab == Tab::Tasks {
                        self.sub = self.line_to_sub.get(line).cloned().flatten();
                    }
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
            NewWorkspace,
            EditRepos,
            Confirm,
            Rename,
            Reject,
            Help,
        }
        let kind = match self.mode {
            Mode::List => Kind::List,
            Mode::Command(_) => Kind::Command,
            Mode::Create(_) => Kind::Create,
            Mode::AddRepo(_) => Kind::AddRepo,
            Mode::NewWorkspace(_) => Kind::NewWorkspace,
            Mode::EditRepos(_) => Kind::EditRepos,
            Mode::Confirm(_) => Kind::Confirm,
            Mode::Rename(_) => Kind::Rename,
            Mode::Reject(_) => Kind::Reject,
            Mode::Help(_) => Kind::Help,
        };
        let close = match kind {
            Kind::List => self.handle_list_key(key),
            Kind::Command => self.handle_command_key(key),
            Kind::Create => self.handle_create_key(key),
            Kind::AddRepo => self.handle_addrepo_key(key),
            Kind::NewWorkspace => self.handle_newws_key(key),
            Kind::EditRepos => self.handle_editrepos_key(key),
            Kind::Confirm => {
                self.handle_confirm_key(key);
                Ok(false)
            }
            Kind::Rename => self.handle_rename_key(key),
            Kind::Reject => self.handle_reject_key(key),
            Kind::Help => {
                self.handle_help_key(key);
                Ok(false)
            }
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
            KeyCode::Tab => self.cycle_tab(false),
            KeyCode::BackTab => self.cycle_tab(true),
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
            KeyCode::Char('n') if ctrl => self.start_create(),
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
                ('g', KeyCode::Char('t')) => self.cycle_tab(false),
            ('g', KeyCode::Char('T')) => self.cycle_tab(true),
                ('d', KeyCode::Char('d')) if self.tab == Tab::Work => self.dismiss_job(),
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
            KeyCode::Tab => self.cycle_tab(false),
            KeyCode::BackTab => self.cycle_tab(true),
            // `n` for a new task (matches the `:n`/`:new` command below),
            // `a` to add a repo — distinct verbs, distinct letters.
            KeyCode::Char('n') if ctrl => self.start_create(),
            KeyCode::Char('n') if self.tab == Tab::Tasks => self.jump_needs_you(),
            KeyCode::Char('a') if self.tab == Tab::Repos => self.start_add_repo(),
            // `W` for a whole new workspace (`:init [path]`), from either tab.
            KeyCode::Char('W') => self.start_new_workspace(""),
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
            KeyCode::Char('A') if self.require_tasks() => self.answer(tenx_core::dialog::Answer::Yes),
            KeyCode::Char('D') if self.require_tasks() => self.deny_selected(),
            KeyCode::Char('t') if self.selected_sub().is_some() => self.view_subagent(false),
            KeyCode::Char('u') => {
                if self.require_tasks() {
                    self.start_unlock();
                }
            }
            KeyCode::Enter | KeyCode::Char('o') | KeyCode::Char('l') => {
                if self.tab == Tab::Tasks {
                    if self.selected_sub().is_some() {
                        self.view_subagent(true);
                        return Ok(false);
                    }
                    return self.jump();
                }
            }
            KeyCode::Char(':') => {
                self.status_msg = None;
                self.mode = Mode::Command(String::new());
            }
            KeyCode::Char('?') => self.mode = Mode::Help(0),
            _ => {}
        }
        Ok(false)
    }

    /// The help overlay scrolls like a pager; any other key closes it, so it
    /// never swallows the key you went to it to look up for more than once.
    fn handle_help_key(&mut self, key: KeyEvent) {
        let Mode::Help(scroll) = &mut self.mode else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => *scroll = scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => *scroll = scroll.saturating_add(10),
            KeyCode::Char('u') if ctrl => *scroll = scroll.saturating_sub(10),
            KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(10),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
            KeyCode::Char('G') | KeyCode::End => *scroll = u16::MAX,
            _ => self.mode = Mode::List,
        }
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
        // `:ask <question>` — an adhoc session started on the question,
        // from either tab.
        if cmd == "ask" || cmd.starts_with("ask ") {
            self.ask(cmd["ask".len()..].trim());
            return Ok(false);
        }
        // `:init [path]` — the new-workspace form, from either tab.
        if cmd == "init" || cmd.starts_with("init ") {
            self.start_new_workspace(cmd["init".len()..].trim());
            return Ok(false);
        }
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
            "work" | "jobs" => {
                self.select_tab(Tab::Work);
                return Ok(false);
            }
            // Quitting kills the client, and a job runs in one of its
            // threads — so leaving would take a clone with it. Refuse once
            // and name what is running; `:q!` goes anyway, as in vim. The
            // work is recoverable either way (an interrupted bare clone is
            // detected and re-cloned), but losing ten minutes of download
            // without being told is not something to do silently.
            "q" | "quit" => {
                let first = self.jobs.lock().iter().find(|j| !j.landed()).map(|j| j.plan.title.clone());
                match first {
                    Some(title) => {
                        let n = self.active_jobs();
                        let what = if n > 1 { format!("{n} jobs still running") } else { title };
                        self.status_msg = Some(format!("{what} — :q! quits anyway"));
                    }
                    None => self.client_request = Some(ClientRequest::Quit),
                }
                return Ok(false);
            }
            "q!" | "quit!" => {
                self.client_request = Some(ClientRequest::Quit);
                return Ok(false);
            }
            // `:n` works from either tab — it uses the selected item's workspace.
            "n" | "new" => {
                self.start_create();
                return Ok(false);
            }
            "h" | "help" | "?" | "keys" => {
                self.mode = Mode::Help(0);
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
        // `:agent [<kind>|default]` — show or set the selected task's agent.
        if cmd == "agent" || cmd.starts_with("agent ") {
            self.set_selected_agent(cmd["agent".len()..].trim());
            return Ok(false);
        }
        match cmd {
            "d" | "del" | "delete" | "rm" => self.start_delete(),
            "r" | "rename" => self.start_rename(),
            // NB: not `:repos` — that's taken above by the Repos tab switch.
            "e" | "edit" | "edit-repos" => self.start_edit_repos(),
            "x" | "close" => self.close_selected_tab(),
            "u" | "unlock" => self.start_unlock(),
            "a" | "approve" | "allow" => self.answer(tenx_core::dialog::Answer::Yes),
            "deny" => self.deny_selected(),
            "reject" => self.start_reject(),
            "cancel" => self.cancel_secrets(),
            "hide" => self.client_request = Some(ClientRequest::Hide),
            "o" | "open" => return self.jump(),
            "next" => self.jump_needs_you(),
            other => self.status_msg = Some(format!("unknown command: :{other}")),
        }
        Ok(false)
    }

    /// `:agent` shows the selected task's agent; `:agent <claude|codex|pi>` sets
    /// it (writes `.tenx-agent`), `:agent default` clears the override. Takes
    /// effect the next time the task's window opens.
    fn set_selected_agent(&mut self, token: &str) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let (title, path, current) = (row.title.clone(), row.path.clone(), row.agent);
        if token.is_empty() {
            self.status_msg =
                Some(format!("'{title}' uses {} — :agent <claude|codex|pi|default> to change", current.as_str()));
            return;
        }
        let kind = (token != "default").then(|| crate::agent::AgentKind::from_token(token));
        if self.offline {
            // The demo never touches disk; reflect the choice in the row only.
            let i = self.filtered[self.selected];
            self.rows[i].agent = kind.unwrap_or(crate::agent::AgentKind::Claude);
            self.status_msg = Some(format!("'{title}' → {}", self.rows[i].agent.as_str()));
            return;
        }
        match crate::agent::set_task_agent(&path, kind) {
            Ok(()) => {
                self.rebuild_rows();
                let now = self.rows.iter().find(|r| r.path == path).map(|r| r.agent.as_str()).unwrap_or("claude");
                self.status_msg = Some(format!("'{title}' → {now} — reopen the task to apply"));
            }
            Err(e) => self.status_msg = Some(format!("couldn't set agent for '{title}': {e}")),
        }
    }

    // ── Answering a permission prompt ─────────────────────────────────────────

    /// `A` / `D` on a blocked row: answer Claude Code's permission dialog in
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
        let state = workspace::resolve_task_state(&path, &sessions, &self.windows.signals);
        if state.status != TaskStatus::Blocked {
            self.status_msg = Some(format!("'{title}' is not waiting on a prompt"));
            return;
        }
        if !state.waiting_for.as_deref().is_some_and(tenx_core::dialog::is_permission_reason) {
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

    /// `D` / `:deny`: a permission prompt is what blocks the agent now, so it
    /// takes the key; failing that, a row with pending secrets rejects them;
    /// anything else goes to `answer`, which says why there is nothing to deny.
    fn deny_selected(&mut self) {
        if !self.selected_answerable() && self.selected_has_secrets() {
            self.start_reject();
        } else {
            self.answer(tenx_core::dialog::Answer::No);
        }
    }

    /// The selected row has a permission dialog the column can answer.
    fn selected_answerable(&self) -> bool {
        self.selected_row().is_some_and(|r| {
            r.status == TaskStatus::Blocked && r.waiting_for.as_deref().is_some_and(tenx_core::dialog::is_permission_reason)
        })
    }

    /// Some task other than the selected one needs you — what the footer's
    /// `n` hint is for.
    fn another_needs_you(&self) -> bool {
        self.filtered
            .iter()
            .enumerate()
            .any(|(pos, &i)| pos != self.selected && self.rows[i].needs_you())
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
        // A task still being built has no directory and no window; opening it
        // would fail with git's error rather than the real reason.
        if row.pending {
            self.status_msg = Some(format!("'{}' is still being set up", row.title));
            return Ok(false);
        }
        let ws_idx = row.ws_idx;
        let slug = row.slug.clone();
        let path = row.path.clone();
        if !self.offline {
            let ws = &self.workspaces[ws_idx];
            if let Err(e) = crate::cli::task::open_in_session(ws, &slug, &self.session) {
                self.status_msg = Some(e.to_string());
                return Ok(false);
            }
        }
        self.current = Some(path.clone());
        self.client_request = Some(ClientRequest::FocusTerminal);
        self.filter.clear();
        self.apply_filter();
        if let Some(pos) = self.filtered.iter().position(|&i| self.rows[i].path == path) {
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
            Tab::Work => None,
            Tab::Repos => self
                .repo_filtered
                .get(self.repo_selected)
                .and_then(|&i| self.repo_rows.get(i))
                .map(|r| r.ws_idx),
        }
    }

    /// `:n` — open the new-task form, preselecting the selected item's
    /// workspace (else the first registered one); the form's workspace
    /// field changes it.
    fn start_create(&mut self) {
        let ws_idx = match self.selected_ws_idx() {
            Some(i) => i,
            None if !self.workspaces.is_empty() => 0,
            None => {
                self.status_msg = Some("no workspace yet: W creates one".into());
                return;
            }
        };
        let repos = self.ws_repos(ws_idx);
        let inherited = self.ws_agent(ws_idx);
        self.status_msg = None;
        self.mode = Mode::Create(CreateForm {
            ws_idx,
            name: String::new(),
            repos,
            agent: None,
            inherited,
            focus: CreateForm::NAME,
        });
    }

    /// The agent a new task in this workspace inherits.
    fn ws_agent(&self, ws_idx: usize) -> crate::agent::AgentKind {
        self.workspaces.get(ws_idx).map_or(crate::agent::AgentKind::Claude, crate::agent::workspace_agent)
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
            // On the workspace and agent fields, arrows (and space) cycle
            // the choice; a new workspace brings its own repo list.
            KeyCode::Right | KeyCode::Left | KeyCode::Char(' ') if form.focus == CreateForm::WORKSPACE => {
                let back = key.code == KeyCode::Left;
                if form.cycle_workspace(self.workspaces.len(), back) {
                    form.repos = self.ws_repos(form.ws_idx);
                    form.inherited = self.ws_agent(form.ws_idx);
                }
            }
            KeyCode::Right if form.focus == form.agent_field() => form.cycle_agent(false),
            KeyCode::Left if form.focus == form.agent_field() => form.cycle_agent(true),
            KeyCode::Char(' ') => {
                if form.focus == form.agent_field() {
                    form.cycle_agent(false);
                } else if let Some(i) = form.repo_field() {
                    form.repos[i].1 = !form.repos[i].1;
                } else if form.focus == CreateForm::NAME {
                    form.name.push(' ');
                }
            }
            KeyCode::Backspace => {
                if form.focus == CreateForm::NAME {
                    form.name.pop();
                }
            }
            KeyCode::Char(c) if !ctrl && form.focus == CreateForm::NAME => form.name.push(c),
            _ => {}
        }
        self.mode = Mode::Create(form);
        Ok(false)
    }

    // ── Background jobs ───────────────────────────────────────────────────────

    /// How many jobs are still going. This is the `[n]` on the Work tab.
    fn active_jobs(&self) -> usize {
        self.jobs.lock().iter().filter(|j| !j.landed()).count()
    }

    /// True while any long operation is running.
    fn job_running(&self) -> bool {
        self.active_jobs() > 0
    }

    /// Keep this many settled jobs. They are the only record of an outcome
    /// once the footer message has been replaced — a failed clone you were
    /// not looking at when it failed would otherwise leave no trace.
    const JOB_HISTORY: usize = 10;

    /// Start `work` on a worker thread, with `plan` describing its steps.
    ///
    /// Several jobs may run at once: anything that would actually collide is
    /// serialised by `git::lock_repo`, so two clones of different repos have
    /// no reason to wait for each other. The panel-free cost of this is that
    /// the Work tab, not a modal, is where they are watched.
    fn start_job<F>(&mut self, plan: tenx_core::progress::Plan, then: super::job::Then, work: F)
    where
        F: FnOnce(&dyn crate::progress::Reporter) -> Result<String, String> + Send + 'static,
    {
        self.status_msg = None;
        // Drop the oldest settled jobs so the list stays bounded, keeping
        // every running one regardless.
        {
            let mut jobs = self.jobs.lock();
            let settled = jobs.iter().filter(|j| j.landed()).count();
            if settled >= Self::JOB_HISTORY {
                let mut drop = settled - Self::JOB_HISTORY + 1;
                jobs.retain(|j| {
                    if j.landed() && drop > 0 {
                        drop -= 1;
                        return false;
                    }
                    true
                });
            }
            jobs.push(super::job::Job::spawn(plan, then, self.id, work));
        }
        self.clamp_work_selection();
    }

    /// Fold whatever the running jobs have reported into their plans, and
    /// finish up any that landed. Called once per client tick; returns true if
    /// the screen should be redrawn.
    ///
    /// This is where a job's effects reach the column's state — on the UI
    /// thread, from disk, after the work is done. No worker touches `self`, so
    /// there is nothing to lock.
    pub(crate) fn drain_job(&mut self) -> bool {
        if self.last_frame.is_none_or(|t| t.elapsed() >= crate::progress::TICK) {
            self.last_frame = Some(Instant::now());
            self.frame = self.frame.wrapping_add(1);
        }
        // Collect what landed on this tick before touching the column: the
        // follow-ups rebuild rows, which must not happen mid-iteration.
        let mut settled: Vec<(super::job::Then, Result<String, String>)> = Vec::new();
        let mut running = false;
        // Another column's job landed: nothing of ours to finish, but its
        // task or repo is on disk now and our rows should show it.
        let mut elsewhere = false;
        {
            let mut jobs = self.jobs.lock();
            if jobs.is_empty() {
                return false;
            }
            for job in jobs.iter_mut() {
                // Whichever column drains first folds the events in; a shared
                // job's plan is the same for all of them.
                if !job.landed() {
                    job.drain();
                }
                if !job.landed() {
                    running = true;
                    continue;
                }
                if !self.seen_landed.insert(job.id) {
                    continue;
                }
                if job.owner == self.id {
                    if let Some(outcome) = job.take_landing() {
                        settled.push((job.then.clone(), outcome));
                    }
                } else {
                    elsewhere = true;
                }
            }
        }
        if settled.is_empty() && !elsewhere {
            // Redraw while anything runs even when nothing arrived: the
            // spinners and the marquee animate off `frame`.
            return running;
        }
        // One rebuild covers every job that landed together.
        self.drop_pending();
        self.rebuild_rows();
        self.rebuild_repo_rows();
        for (then, outcome) in settled {
            match outcome {
                Ok(msg) => {
                    self.status_msg = Some(msg);
                    self.finish(then);
                }
                Err(e) => self.status_msg = Some(e),
            }
        }
        self.clamp_work_selection();
        true
    }

    /// The landed job's follow-up, run against freshly rebuilt rows.
    fn finish(&mut self, then: super::job::Then) {
        use super::job::Then;
        match then {
            Then::Nothing => {}
            Then::SelectTask(ws_idx, slug) => {
                self.select_task(ws_idx, &slug);
            }
            // A task that has just been built: give it its window, then land
            // the selection on it.
            Then::OpenTask(ws_idx, slug) => {
                if !self.offline
                    && let Some(ws) = self.workspaces.get(ws_idx)
                    && let Err(e) = crate::cli::task::ensure_window_in(ws, &slug)
                {
                    // The task exists and its worktrees are there; only the
                    // window is missing, and ⏎ still makes one. Say so rather
                    // than losing the "created" message to an error.
                    self.status_msg = Some(format!("created '{slug}', but its window didn't open: {e}"));
                }
                // The row was built before the window existed, so its
                // `window_id` is stale — re-read so it reads as open, not
                // closed, the moment it appears.
                self.rebuild_rows();
                self.select_task(ws_idx, &slug);
            }
            Then::Workspace(dir) => self.finish_new_workspace(&dir),
        }
    }

    /// Put the selection on a task by slug. By slug and not by position
    /// because every rebuild re-sorts the list.
    fn select_task(&mut self, ws_idx: usize, slug: &str) {
        if let Some(pos) = self
            .filtered
            .iter()
            .position(|&i| self.rows[i].ws_idx == ws_idx && self.rows[i].slug == slug)
        {
            self.tab = Tab::Tasks;
            self.selected = pos;
        }
    }

    /// Remove every ghost row whose job is no longer running. The real ones
    /// come back from `rebuild_rows`.
    fn drop_pending(&mut self) {
        self.rows.retain(|r| !r.pending);
        self.apply_filter();
    }

    /// Forget a settled job from the Work tab. Running ones stay: there is
    /// nothing to dismiss until they finish.
    fn dismiss_job(&mut self) {
        let landed = self.jobs.lock().get(self.work_selected).map(|j| j.landed());
        match landed {
            Some(true) => {
                self.jobs.lock().remove(self.work_selected);
                self.clamp_work_selection();
            }
            Some(false) => self.status_msg = Some("still running — it clears when it finishes".into()),
            None => {}
        }
    }

    /// Create the task and select its row. `Ok(true)` when the new row is now
    /// the selection (so a `jump` lands on it), `Ok(false)` if it couldn't be
    /// found in the rebuilt list.
    fn submit_create(&mut self, form: &CreateForm) -> Result<bool, String> {
        let name = form.name.trim().to_string();
        if name.is_empty() {
            return Err("task name cannot be empty".into());
        }
        // None ticked is a task without worktrees: its agent runs in the
        // task directory and can read the whole workspace.
        let repos: Vec<String> = form
            .repos
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect();
        let ws_idx = form.ws_idx;
        // The same slug the job's `new_in` will pick — counted up in the
        // adhoc workspace — so the ghost row and `OpenTask` name the task
        // the job creates. Its refusals (a taken name) surface here, before
        // anything starts.
        let slug = if self.offline {
            crate::workspace::slugify(&name)
        } else {
            crate::cli::task::plan_slug(&self.workspaces[ws_idx], &name).map_err(|e| e.to_string())?
        };
        if self.offline {
            // The demo: the task appears as a row, its agent already at work.
            let ws = &self.workspaces[ws_idx];
            let now = SystemTime::now();
            self.rows.push(Row {
                pending: false,
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
                agent: form.agent.unwrap_or_else(|| crate::agent::agent_for(ws, &ws.dir.join("tasks").join(&slug))),
                secrets_pending: vec![],
                secrets_pending_set: vec![],
                secrets_why: vec![],
                section: TaskStatus::Working.group(),
                subagents: vec![],
            });
            self.filter.clear();
            self.sort_rows();
            self.apply_filter();
        } else {
            if self.job_running() {
                return Err("one repo operation at a time — this one is still running".into());
            }
            let ws = &self.workspaces[ws_idx];
            let ws_dir = ws.dir.clone();
            let task_dir = ws.dir.join("tasks").join(&slug);
            let plan = tenx_core::progress::Plan::new(
                format!("creating '{name}'"),
                crate::cli::task::new_steps(ws, Some(&repos)),
            );

            // The row appears now, ghosted, and the clone fills in behind it:
            // the task is visibly *there* from the keystroke that made it,
            // instead of after a minute of frozen screen.
            let ghost = self.ghost_row(ws_idx, &slug, &name, &repos, form.agent);
            self.rows.push(ghost);
            self.filter.clear();
            self.sort_rows();
            self.apply_filter();
            let (job_name, agent) = (name.clone(), form.agent);
            self.start_job(plan, super::job::Then::OpenTask(ws_idx, slug.clone()), move |rep| {
                // Reloaded on the worker rather than captured: `Workspace` is
                // read from disk, and this is the convention everywhere else
                // — derive state from the live source, don't carry a snapshot
                // across a thread boundary.
                let ws = crate::workspace::load(&ws_dir).map_err(|e| e.to_string())?;
                // no_open=true: the window is opened by the `jump` that
                // follows the job landing, not from the worker — tmux calls
                // belong on the thread that owns the terminal.
                crate::cli::task::new_in(&ws, &job_name, Some(&repos), crate::cli::task::OpenMode::Closed, rep).map_err(|e| e.to_string())?;
                // Pin the chosen agent before anything opens the window;
                // `None` inherits the workspace/global default.
                if let Some(kind) = agent {
                    let _ = crate::agent::set_task_agent(&task_dir, Some(kind));
                }
                Ok(format!("created '{job_name}'"))
            });
            // The ghost row is the selection, so ⏎ and the footer both have
            // something to point at; `jump` is deliberately not run here —
            // there is no window to jump to until the job lands, and when it
            // does the window opens *detached* (`Then::OpenTask`): a new task
            // should be running, not waiting on a ⏎, but it must not drag the
            // terminal off whatever you are looking at.
            let selected = self
                .filtered
                .iter()
                .position(|&i| self.rows[i].ws_idx == ws_idx && self.rows[i].slug == slug);
            if let Some(pos) = selected {
                self.tab = Tab::Tasks;
                self.selected = pos;
            }
            return Ok(false);
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

    /// A placeholder row for a task a job is still building. Filed under
    /// `Working` — something *is* working on it — with nothing derived from
    /// disk, because none of it is on disk yet.
    fn ghost_row(
        &self,
        ws_idx: usize,
        slug: &str,
        title: &str,
        repos: &[String],
        agent: Option<crate::agent::AgentKind>,
    ) -> Row {
        let ws = &self.workspaces[ws_idx];
        let path = ws.dir.join("tasks").join(slug);
        let now = SystemTime::now();
        Row {
            pending: true,
            ws_idx,
            ws_name: ws.config.name.clone(),
            path: path.clone(),
            slug: slug.to_string(),
            title: title.to_string(),
            status: TaskStatus::Working,
            group: TaskStatus::Working,
            changed: Some(now),
            waiting_for: None,
            activity: now,
            window_id: None,
            pane: None,
            live: crate::live::Live::default(),
            repos: repos.to_vec(),
            agent: agent.unwrap_or_else(|| crate::agent::agent_for(ws, &path)),
            secrets_pending: vec![],
            secrets_pending_set: vec![],
            secrets_why: vec![],
            section: TaskStatus::Working.group(),
            subagents: vec![],
        }
    }

    /// `:ask <question>`: a session in the adhoc workspace, titled after
    /// the question, that starts working on it the moment its window opens.
    /// Nothing is cloned, but it runs as a job like any creation so the row,
    /// the window and the selection land the same way.
    fn ask(&mut self, prompt: &str) {
        if prompt.is_empty() {
            self.status_msg = Some(":ask <question>".into());
            return;
        }
        let Some(ws_idx) = self.workspaces.iter().position(|w| w.is_adhoc()) else {
            self.status_msg = Some("no adhoc workspace — restart tenx to create it".into());
            return;
        };
        let mut title = tenx_core::orchestrate::ask_title(prompt);
        if crate::workspace::slugify(&title).is_empty() {
            title = "question".into();
        }
        let slug = match crate::cli::task::plan_slug(&self.workspaces[ws_idx], &title) {
            Ok(s) => s,
            Err(e) => {
                self.status_msg = Some(e.to_string());
                return;
            }
        };
        let mut ghost = self.ghost_row(ws_idx, &slug, &title, &[], None);
        // The demo never touches disk: the session is just there, at work.
        ghost.pending = !self.offline;
        self.rows.push(ghost);
        self.filter.clear();
        self.sort_rows();
        self.apply_filter();
        if self.offline {
            self.select_task(ws_idx, &slug);
            self.status_msg = Some(format!("asked '{title}'"));
            return;
        }
        let ws_dir = self.workspaces[ws_idx].dir.clone();
        let prompt = prompt.to_string();
        let plan = tenx_core::progress::Plan::new(format!("asking '{title}'"), Vec::new());
        self.start_job(plan, super::job::Then::OpenTask(ws_idx, slug.clone()), move |rep| {
            let ws = crate::workspace::load(&ws_dir).map_err(|e| e.to_string())?;
            let md = crate::cli::task::TaskMd { prompt: &prompt, ..Default::default() };
            let none: [String; 0] = [];
            crate::cli::task::new_with(&ws, &title, Some(&none), crate::cli::task::OpenMode::Closed, &md, None, rep)
                .map_err(|e| e.to_string())?;
            Ok(format!("asked '{title}'"))
        });
        self.select_task(ws_idx, &slug);
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
        let name_opt = if name.is_empty() { None } else { Some(name.to_string()) };
        if self.job_running() {
            return Err("one repo operation at a time — this one is still running".into());
        }
        let repo_name = name_opt.clone().unwrap_or_else(|| crate::cli::repo::infer_name(&url));
        let ws_dir = self.workspaces[form.ws_idx].dir.clone();
        let plan = tenx_core::progress::Plan::new(format!("cloning {repo_name}"), [repo_name.clone()]);
        self.start_job(plan, super::job::Then::Nothing, move |rep| {
            let mut ws = crate::workspace::load(&ws_dir).map_err(|e| e.to_string())?;
            crate::cli::repo::add_in(&mut ws, &url, name_opt.as_deref(), rep).map_err(|e| e.to_string())?;
            Ok(format!("added repo '{repo_name}'"))
        });
        Ok(())
    }

    // ── New workspace (either tab) ────────────────────────────────────────────

    /// `W` / `:init [path]` — open the new-workspace form. The path defaults
    /// to a sibling of the selected item's workspace (people keep their
    /// workspaces together), ready for the name to be typed at the end.
    fn start_new_workspace(&mut self, path: &str) {
        let path = if path.is_empty() {
            self.selected_ws_idx()
                .and_then(|i| self.workspaces.get(i))
                .and_then(|ws| ws.dir.parent())
                .map(|p| format!("{}/", p.display()))
                .unwrap_or_else(|| "~/".to_string())
        } else {
            path.to_string()
        };
        self.status_msg = None;
        self.mode = Mode::NewWorkspace(NewWorkspaceForm {
            path,
            name: String::new(),
            repo_url: String::new(),
            skills: true,
            focus: 0,
        });
    }

    fn handle_newws_key(&mut self, key: KeyEvent) -> Result<bool> {
        let mut form = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::NewWorkspace(f) => f,
            other => {
                self.mode = other;
                return Ok(false);
            }
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let fields = NewWorkspaceForm::FIELDS;
        match key.code {
            KeyCode::Esc => return Ok(false), // cancel; mode already List
            KeyCode::Enter => match self.submit_new_workspace(&form) {
                // `submit` chose where to land (the list, or the add-repo form).
                Ok(()) => return Ok(false),
                Err(e) => self.status_msg = Some(e),
            },
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % fields,
            KeyCode::BackTab | KeyCode::Up => form.focus = (form.focus + fields - 1) % fields,
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right if form.focus == NewWorkspaceForm::SKILLS => {
                form.skills = !form.skills
            }
            KeyCode::Backspace => {
                if let Some(s) = form.field_mut() {
                    s.pop();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(s) = form.field_mut() {
                    s.push(c);
                }
            }
            _ => {}
        }
        self.mode = Mode::NewWorkspace(form);
        Ok(false)
    }

    /// Create the workspace, then land where it can be seen: it has no
    /// tasks yet, so the Tasks tab would show nothing of it. Given a repo,
    /// that is the Repos tab with the repo selected (Ctrl+n there creates
    /// the first task); without one, the add-repo form for the new
    /// workspace, since a task needs a repo.
    fn submit_new_workspace(&mut self, form: &NewWorkspaceForm) -> Result<(), String> {
        let path = form.path.trim();
        if path.is_empty() {
            return Err("path cannot be empty".into());
        }
        let dir = PathBuf::from(workspace::expand_home(path));
        let name = match form.name.trim() {
            "" => dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .ok_or_else(|| "give a name — the path has no last segment to use".to_string())?,
            n => n.to_string(),
        };
        let url = form.repo_url.trim();
        let repos = if url.is_empty() {
            vec![]
        } else {
            vec![workspace::RepoConfig { name: crate::cli::repo::infer_name(url), url: url.to_string() }]
        };
        if self.offline {
            self.status_msg = Some(format!("would create workspace '{name}' at {}", dir.display()));
            return Ok(());
        }
        if self.job_running() {
            return Err("one repo operation at a time — this one is still running".into());
        }
        // The workspace directory and its config are written in an instant;
        // it is the repo clone inside `init_in` that takes the time, so the
        // whole call goes to the worker and the landing does the rest.
        let steps: Vec<String> = repos.iter().map(|r| r.name.clone()).collect();
        let plan = tenx_core::progress::Plan::new(format!("creating workspace '{name}'"), steps);
        self.filter.clear();
        let (dir2, name2, skills) = (dir.clone(), name.clone(), form.skills);
        self.start_job(plan, super::job::Then::Workspace(dir.clone()), move |rep| {
            crate::cli::init::init_in(&dir2, &name2, repos, String::new(), skills, rep)
                .map(|_| format!("workspace '{name2}' created"))
                .map_err(|e| e.to_string())
        });
        Ok(())
    }

    /// Land on a workspace the job just created: the Repos tab with it
    /// selected, or the add-repo form when it was created without a repo,
    /// since a task needs one.
    fn finish_new_workspace(&mut self, dir: &Path) {
        self.reload_workspaces();
        self.tidy();
        // The registry holds canonical paths; the form's may not be.
        let created = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|w| w.dir.canonicalize().unwrap_or_else(|_| w.dir.clone()) == created)
        else {
            self.status_msg = Some("workspace created, but it is not in the registry".into());
            return;
        };
        if self.workspaces[ws_idx].config.repos.is_empty() {
            let name = self.workspaces[ws_idx].config.name.clone();
            self.status_msg = Some(format!("workspace '{name}' created — add its first repo"));
            self.mode = Mode::AddRepo(AddRepoForm { ws_idx, url: String::new(), name: String::new(), focus: 0 });
            return;
        }
        self.select_repos_tab();
        if let Some(pos) = self.repo_filtered.iter().position(|&i| self.repo_rows[i].ws_idx == ws_idx) {
            self.repo_selected = pos;
        }
        self.focus_list();
    }

    // ── Edit repos (Tasks tab) ────────────────────────────────────────────────

    /// `e` / `:e` — open the repo checklist for the selected task, prefilled
    /// with the worktrees it already has.
    fn start_edit_repos(&mut self) {
        let Some(row) = self.selected_row() else {
            self.status_msg = Some("select a task first".into());
            return;
        };
        // Its worktrees are being created right now; editing the set would
        // race the job that is building it.
        if row.pending {
            self.status_msg = Some("still being set up — wait for it to finish".into());
            return;
        }
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
            self.status_msg = Some(if self.workspaces.get(ws_idx).is_some_and(|w| w.is_adhoc()) {
                "an adhoc session has no repos — create a task in a workspace for code".into()
            } else {
                "no repos in workspace — add one on the Repos tab".into()
            });
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
        if self.job_running() {
            self.status_msg = Some("one repo operation at a time — this one is still running".into());
            return;
        }
        let (added, removed) = (form.added().len(), form.removed().len());
        let desired = form.desired();
        let (ws_idx, slug, title) = (form.ws_idx, form.slug.clone(), form.title.clone());
        let ws_dir = self.workspaces[ws_idx].dir.clone();
        // `set_repos_steps` walks the same diff, in the same order, that
        // `set_repos_in` will report against — additions, then removals.
        let plan = tenx_core::progress::Plan::new(
            format!("updating '{title}'"),
            crate::cli::task::set_repos_steps(&self.workspaces[ws_idx], &slug, &desired),
        );
        let slug2 = slug.clone();
        // `SelectTask`, not `OpenTask`: editing an existing task's repos is no
        // reason to open a window it didn't have.
        self.start_job(plan, super::job::Then::SelectTask(ws_idx, slug.clone()), move |rep| {
            let ws = crate::workspace::load(&ws_dir).map_err(|e| e.to_string())?;
            crate::cli::task::set_repos_in(&ws, &slug2, &desired, false, rep).map_err(|e| e.to_string())?;
            Ok(match (added, removed) {
                (a, 0) => format!("added {a} repo(s) to '{title}'"),
                (0, r) => format!("detached {r} repo(s) from '{title}'"),
                (a, r) => format!("added {a}, detached {r} in '{title}'"),
            })
        });
    }

    // ── Delete ────────────────────────────────────────────────────────────────

    fn start_delete(&mut self) {
        if self.selected_row().is_some_and(|r| r.pending) {
            self.status_msg = Some("still being set up — wait for it to finish".into());
            return;
        }
        if let Some(r) = self.selected_row() {
            self.mode = Mode::Confirm(Confirm {
                ws_idx: r.ws_idx,
                slug: r.slug.clone(),
                title: r.title.clone(),
                path: r.path.clone(),
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
        // dir goes. Looked up live, and by task — never the cached id (tmux
        // reuses `@N` after a server restart) and never by name alone (a slug
        // is unique only within its workspace).
        if let Some(w) = crate::tmux::find_task_window(&confirm.slug, &confirm.path).ok().flatten() {
            let _ = crate::tmux::kill_window(&w.id);
        }
        if self.job_running() {
            self.status_msg = Some("one repo operation at a time — this one is still running".into());
            return;
        }
        // Removing N worktrees is local, but on a large repo it is still
        // seconds of filesystem work — long enough to be worth not freezing
        // the client for. It reports no phases, so its panel is the
        // indeterminate one.
        let ws_dir = self.workspaces[confirm.ws_idx].dir.clone();
        let (slug, title) = (confirm.slug.clone(), confirm.title.clone());
        let plan = tenx_core::progress::Plan::new(format!("deleting '{title}'"), [title.clone()]);
        self.start_job(plan, super::job::Then::Nothing, move |rep| {
            rep.emit(crate::progress::Event::Start { step: 0, label: title.clone(), verb: "deleting" });
            let ws = crate::workspace::load(&ws_dir).map_err(|e| e.to_string())?;
            match crate::cli::task::rm_in(&ws, &slug, true) {
                Ok(()) => {
                    rep.emit(crate::progress::Event::Done { step: 0, note: "deleted".into() });
                    Ok(format!("deleted '{title}'"))
                }
                Err(e) => {
                    rep.emit(crate::progress::Event::Failed { step: 0, err: e.to_string() });
                    Err(e.to_string())
                }
            }
        });
    }

    // ── Close tab ─────────────────────────────────────────────────────────────

    fn close_selected_tab(&mut self) {
        let Some(r) = self.selected_row() else {
            return;
        };
        let path = r.path.clone();
        let slug = r.slug.clone();
        // The live window *of this task* is the only truth for a kill — a
        // cached id can belong to another task after a server restart, and a
        // name can belong to a namesake in another workspace.
        let id = crate::tmux::find_task_window(&slug, &path).ok().flatten().map(|w| w.id);
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
    pub(crate) fn maybe_sweep(&mut self) {
        if self.last_swept.is_some_and(|t| t.elapsed() < SWEEP_INTERVAL) {
            return;
        }
        self.last_swept = Some(Instant::now());
        let n = crate::cli::task::sweep_quiet(crate::cli::task::DEFAULT_SWEEP_AFTER, crate::cli::task::DEFAULT_IDLE_GRACE);
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

    pub(super) fn selected_has_secrets(&self) -> bool {
        self.selected_row().is_some_and(|r| !r.secrets_pending.is_empty() || !r.secrets_pending_set.is_empty())
    }

    // ── Rejecting secrets requests ────────────────────────────────────────────

    /// Open the note prompt for rejecting every pending secrets request of
    /// the selected row. Unlike `:cancel`, the waiting agent is told it was
    /// *denied* (exit 4, with the note), not that the request went away.
    pub(super) fn start_reject(&mut self) {
        let Some(r) = self.selected_row() else {
            return;
        };
        if !self.selected_has_secrets() {
            self.status_msg = Some("no pending secrets for this task".into());
            return;
        }
        let names = r.secrets_pending.iter().chain(&r.secrets_pending_set).cloned().collect();
        self.mode = Mode::Reject(RejectForm { ws_idx: r.ws_idx, slug: r.slug.clone(), names, buffer: String::new() });
    }

    fn handle_reject_key(&mut self, key: KeyEvent) -> Result<bool> {
        let mut form = match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::Reject(f) => f,
            other => {
                self.mode = other;
                return Ok(false);
            }
        };
        match key.code {
            KeyCode::Esc => return Ok(false),
            KeyCode::Enter => {
                self.reject_secrets(form);
                return Ok(false);
            }
            KeyCode::Backspace => {
                form.buffer.pop();
            }
            KeyCode::Char(c) => form.buffer.push(c),
            _ => {}
        }
        self.mode = Mode::Reject(form);
        Ok(false)
    }

    /// Write the denials (`cli::secrets::deny_quiet`: inline, like
    /// `:cancel` — no passphrase, no terminal handoff) and drop the row out
    /// of SECRETS PENDING.
    fn reject_secrets(&mut self, form: RejectForm) {
        let note = form.buffer.trim();
        let note = (!note.is_empty()).then_some(note);
        let slug = form.slug;
        if self.offline {
            if let Some(row) = self.rows.iter_mut().find(|r| r.ws_idx == form.ws_idx && r.slug == slug) {
                row.secrets_pending.clear();
                row.secrets_pending_set.clear();
                row.secrets_why.clear();
            }
            self.status_msg = Some(format!("rejected {} for '{slug}'", form.names.join(", ")));
            return;
        }
        let result = self
            .workspaces
            .get(form.ws_idx)
            .context("workspace no longer registered")
            .and_then(|ws| ws.find_task(&slug))
            .and_then(|task| crate::cli::secrets::deny_quiet(&task, &form.names, note));
        self.status_msg = Some(match result {
            Ok(denied) if denied.is_empty() => format!("'{slug}' had nothing pending any more"),
            Ok(denied) => format!("rejected {} for '{slug}'", denied.join(", ")),
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

/// The fallback unlock, for when the client can't aim a tmux popup at its
/// own attach (`Client::start_unlock` is the normal path): suspend the TUI
/// to run the real, interactive secrets fulfillment —
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
/// process. The sitting itself — review, grant or deny, values, one
/// passphrase — is `cli::secrets::fulfill_in`, the same as
/// `tenx secrets fulfill` from a shell.
pub(super) fn run_unlock(
    terminal: &mut super::client::ClientTerminal,
    column: &mut Column,
    ws_idx: usize,
    slug: &str,
) -> Result<()> {
    disable_raw_mode()?;
    // Bracketed paste off too: the client turned it on for the embedded
    // terminal, and left on it wraps a pasted secret in `ESC[200~`…`ESC[201~`
    // at the value prompt.
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange, DisableBracketedPaste)?;
    terminal.show_cursor()?;

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
    execute!(terminal.backend_mut(), EnterAlternateScreen, EnableMouseCapture, EnableFocusChange, EnableBracketedPaste)?;
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
    } else if matches!(column.mode, Mode::NewWorkspace(_)) {
        render_newws(f, column, area);
    } else if matches!(column.mode, Mode::EditRepos(_)) {
        render_editrepos(f, column, area);
    } else if matches!(column.mode, Mode::Help(_)) {
        render_help(f, column, area);
    } else {
        render_list(f, column, area);
    }
}


// ── The Work tab ─────────────────────────────────────────────────────────────

/// At most this many step lines per job. Beyond it the entry scrolls a window
/// around the active step rather than growing: a workspace with a dozen repos
/// would otherwise push every other job off the screen.
const MAX_STEP_LINES: usize = 3;

/// One job as list lines: its title and step counter, the steps around the one
/// running, an overall bar, and what git says about the transfer.
///
/// A settled job collapses to its title and outcome — there is nothing left to
/// animate, and the point of keeping it is that you can read what happened.
fn job_lines(job: &super::job::Job, frame: usize, width: usize, selected: bool) -> Vec<Line<'static>> {
    use tenx_core::progress::StepState;
    let dim = Style::default().fg(palette::MUTED.color());
    let inner = width.saturating_sub(2);
    let mut lines: Vec<Line<'static>> = Vec::new();

    let title_fg = if selected { palette::SEL_TEXT.color() } else { palette::TEXT.color() };
    let (glyph, glyph_style) = if job.failed() {
        ("✗", Style::default().fg(palette::DANGER.color()))
    } else if job.landed() {
        ("✓", Style::default().fg(palette::SUCCESS.color()))
    } else {
        (
            crate::progress::FRAMES[frame % crate::progress::FRAMES.len()],
            Style::default().fg(palette::ACCENT.color()),
        )
    };
    let counter = if job.landed() { String::new() } else { format!("  {}", job.plan.counter()) };
    let title_w = inner.saturating_sub(2 + counter.width()).max(1);
    lines.push(Line::from(vec![
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(
            pad_cell(&job.plan.title, title_w),
            Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(counter, dim),
    ]));

    if job.landed() {
        // The outcome line: the message on success, the error on failure.
        if let Some(note) = job.outcome_note() {
            let style = if job.failed() {
                Style::default().fg(palette::DANGER.color())
            } else {
                dim
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(truncate(note, inner.saturating_sub(2)), style),
            ]));
        }
        return lines;
    }

    // A window around the active step, so a long repo list still shows what is
    // happening now rather than the first three repos forever.
    let active = job.plan.active().unwrap_or(0);
    let total = job.plan.steps.len();
    let start = active.saturating_sub(MAX_STEP_LINES - 1).min(total.saturating_sub(MAX_STEP_LINES));
    for (i, step) in job.plan.steps.iter().enumerate().skip(start).take(MAX_STEP_LINES) {
        let (g, style) = match &step.state {
            StepState::Pending => ("·", Style::default().fg(palette::IDLE.color())),
            StepState::Running(_) => (
                crate::progress::FRAMES[(frame + i) % crate::progress::FRAMES.len()],
                Style::default().fg(palette::ACCENT.color()),
            ),
            StepState::Done(_) => ("✓", Style::default().fg(palette::SUCCESS.color())),
            StepState::Failed(_) => ("✗", Style::default().fg(palette::DANGER.color())),
        };
        let note = step.note().to_string();
        // The label takes what the note leaves — a repo name is worth more
        // than the phase word, so the note is what gets dropped first.
        let label_w = inner.saturating_sub(4 + note.width() + 1).max(1);
        let label_style = match step.state {
            StepState::Pending => dim,
            _ => Style::default().fg(palette::TEXT.color()),
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{g} "), style),
            Span::styled(pad_cell(&step.label, label_w), label_style),
            Span::styled(note, dim),
        ]));
    }

    // The overall bar: every step, not just the one running, so it matches the
    // step counter beside the title and doesn't restart per repo.
    let snap = job.active_snapshot();
    let determinate = snap.is_some_and(|s| s.percent.is_some());
    let bar_w = inner.saturating_sub(8).max(1);
    if determinate {
        let frac = job.plan.fraction();
        let filled = tenx_core::progress::bar(bar_w, frac);
        let cut = filled.chars().take_while(|c| *c == '█').count();
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(filled.chars().take(cut).collect::<String>(), Style::default().fg(palette::ACCENT.color())),
            Span::styled(filled.chars().skip(cut).collect::<String>(), Style::default().fg(palette::BORDER.color())),
            Span::styled(format!(" {:>3}%", (frac * 100.0).round() as u16), dim),
        ]));
    } else {
        // Nothing has reported a percent yet (or ever will — a worktree
        // removal reports nothing). A still bar would read as a hang.
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                tenx_core::progress::marquee(bar_w, frame),
                Style::default().fg(palette::ACCENT.color()),
            ),
        ]));
    }

    let transfer = snap.as_ref().map(tenx_core::progress::transfer_line).unwrap_or_default();
    if !transfer.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(truncate(&transfer, inner.saturating_sub(2)), dim),
        ]));
    }
    lines
}

/// The Work tab's list: every job, newest last, running ones first-class and
/// settled ones collapsed to their outcome.
fn work_items(column: &Column, width: usize) -> (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>) {
    let mut items = Vec::new();
    let mut line_to_pos = Vec::new();
    let mut selected_line = None;
    for (i, job) in column.jobs.lock().iter().enumerate() {
        if i == column.work_selected && column.focus == Focus::List {
            selected_line = Some(items.len());
        }
        let selected = i == column.work_selected && column.focus == Focus::List;
        let mut lines = job_lines(job, column.frame, width, selected);
        lines.push(Line::from(""));
        items.push(ListItem::new(lines));
        line_to_pos.push(Some(i));
    }
    if items.is_empty() {
        for line in work_empty_lines() {
            items.push(ListItem::new(line));
            line_to_pos.push(None);
        }
    }
    (items, selected_line, line_to_pos)
}

/// What the Work tab says when nothing has run yet.
fn work_empty_lines() -> Vec<Line<'static>> {
    let dim = Style::default().fg(palette::MUTED.color());
    vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("nothing running", Style::default().fg(palette::TEXT.color())),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("clones and worktree changes", dim),
        ]),
        Line::from(vec![Span::raw("  "), Span::styled("show up here while they run.", dim)]),
    ]
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
    // `Work [2]` — the count of running jobs, so the one thing you might be
    // waiting on is legible from any tab without costing the list a row. The
    // brackets are dropped when nothing is running, rather than showing a
    // `[0]` that draws the eye for no reason.
    //
    // Laid out by hand rather than with ratatui's `Tabs` so each tab's x-range
    // is known exactly for the mouse, and so the `[n]` can keep its own colour
    // instead of inheriting the selected/unselected style.
    let active = column.active_jobs();
    let mut spans: Vec<Span> = Vec::new();
    let mut tab_spans: Vec<(u16, u16)> = Vec::new();
    let mut x: u16 = 0;
    for (i, t) in Tab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", Style::default().fg(palette::MUTED.color())));
            x += 1;
        }
        let start = x;
        let selected = *t == column.tab;
        let style = Style::default()
            .fg(if selected { palette::ACCENT.color() } else { palette::MUTED.color() })
            .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() });
        let label = format!(" {} ", t.label());
        x += label.width() as u16;
        spans.push(Span::styled(label, style));
        if *t == Tab::Work && active > 0 {
            // Always the attention colour, selected or not: the point of the
            // count is to be seen from the other tabs.
            let count = format!("[{active}] ");
            x += count.width() as u16;
            spans.push(Span::styled(
                count,
                Style::default().fg(palette::INFO.color()).add_modifier(Modifier::BOLD),
            ));
        }
        tab_spans.push((start, x));
    }
    column.tab_spans = tab_spans;
    f.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);

    // ── Search box (or the rename input) ──────────────────────────────────────
    let title = match column.mode {
        Mode::Rename(_) => " rename task ",
        Mode::Reject(_) => " reject · note for the agent ",
        _ => "",
    };
    let (prefix, prefix_style, value) = match &column.mode {
        Mode::Rename(form) => ("✎ ", Style::default().fg(palette::ACCENT.color()), form.buffer.clone()),
        Mode::Reject(form) => ("✗ ", Style::default().fg(palette::DANGER.color()), form.buffer.clone()),
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
    let show_cursor = matches!(column.mode, Mode::Rename(_) | Mode::Reject(_)) || column.focus == Focus::Search;
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
    let (items, line_of_selected, line_to_pos, line_to_sub) = match column.tab {
        Tab::Tasks => column_items(column, list_width),
        Tab::Repos => no_subs(repo_items(column, list_width)),
        Tab::Work => no_subs(work_items(column, list_width)),
    };
    column.line_to_pos = line_to_pos;
    column.line_to_sub = line_to_sub;
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
    let footer = footer_line(&view::footer(column));
    f.render_widget(Paragraph::new(footer), chunks[3]);
}

// ── Help (`?`, `:help`) ──────────────────────────────────────────────────────

/// Every key the column answers to, by section, for the `?` overlay. Keep it
/// in step with `handle_*_key`, `run_command` and the README's key table.
const KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "anywhere",
        &[
            ("^w", "into the column / hide it"),
            ("?", "this help (list mode)"),
            (":", "command line"),
        ],
    ),
    (
        "search field",
        &[
            ("type", "filter"),
            ("⏎", "open the top match"),
            ("↓ ↑ ^j ^k", "into the list at your task"),
            ("esc", "into the list"),
            ("⇥ ⇧⇥", "switch tab"),
            ("^n", "new task"),
        ],
    ),
    (
        "list",
        &[
            ("j k ↓ ↑", "move"),
            ("gg G", "top / bottom"),
            ("⏎ o l", "open task / agent"),
            ("t", "agent transcript"),
            ("n", "next task that needs you"),
            ("A D", "approve / deny permission"),
            ("D", "reject pending secrets"),
            ("u", "unlock pending secrets"),
            ("^n", "new task"),
            ("r", "rename"),
            ("e", "edit repos"),
            ("x", "close window"),
            ("dd", "delete (Work tab: dismiss)"),
            ("a", "add repo (Repos tab)"),
            ("W", "new workspace"),
            ("i /", "search field"),
            ("⇥ gt gT", "switch tab (⇧⇥ back)"),
            ("esc q ^c", "back to the task"),
        ],
    ),
    (
        "commands",
        &[
            (":n", "new task (:new)"),
            (":ask", "ask in an adhoc session"),
            (":o", "open task (:open)"),
            (":r", "rename"),
            (":e", "edit repos (:edit-repos)"),
            (":x", "close window (:close)"),
            (":u", "unlock secrets (:unlock)"),
            (":reject", "reject secrets request"),
            (":cancel", "withdraw secrets request"),
            (":a", "approve (:approve)"),
            (":deny", "deny permission"),
            (":d :rm", "delete task"),
            (":next", "next needing you"),
            (":agent", "show / set agent [kind]"),
            (":init", "new workspace [path]"),
            (":tasks", "Tasks tab (:repos :work)"),
            (":hide", "hide the column"),
            (":q :q!", "quit client"),
            (":help", "this help"),
        ],
    ),
    (
        "forms",
        &[
            ("⇥ ↓ ⇧⇥ ↑", "next / previous field"),
            ("← →", "cycle workspace / agent"),
            ("space", "toggle"),
            ("⏎", "submit"),
            ("esc", "cancel"),
        ],
    ),
];

/// The `KEYS` table as lines: a section heading, then `key  action` rows
/// with the keys padded to the widest in the table so the actions align.
fn help_lines() -> Vec<Line<'static>> {
    let key_w = KEYS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (i, (section, rows)) in KEYS.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {section}"),
            Style::default().fg(palette::WARN.color()).add_modifier(Modifier::BOLD),
        )));
        for (key, action) in rows.iter() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key:<key_w$}  "),
                    Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD),
                ),
                Span::raw(*action),
            ]));
        }
    }
    lines
}

fn render_help(f: &mut ratatui::Frame, column: &mut Column, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let lines = help_lines();
    // Clamp here, where the height is known, so `G` (u16::MAX) lands on the
    // last page and `k` from there moves at once.
    let visible = chunks[0].height.saturating_sub(2);
    let max = (lines.len() as u16).saturating_sub(visible);
    let scroll = match &mut column.mode {
        Mode::Help(s) => {
            *s = (*s).min(max);
            *s
        }
        _ => 0,
    };
    let body = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())).title(" keys "));
    f.render_widget(body, chunks[0]);
    let more = if scroll < max { " ↓ more" } else { "" };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}{more}", view::HELP_HINT),
            Style::default().fg(palette::MUTED.color()),
        ))),
        chunks[1],
    );
}

/// The footer's mode tag: INSERT on green, NORMAL on blue.
/// The footer as a terminal line: each [`view::FooterKind`] in its voice.
fn footer_line(footer: &view::Footer) -> Line<'static> {
    use view::FooterKind;
    let muted = Style::default().fg(palette::MUTED.color());
    match footer.kind {
        FooterKind::Command => {
            let mut spans = vec![
                Span::styled(":", Style::default().fg(palette::ACCENT.color()).add_modifier(Modifier::BOLD)),
                Span::raw(footer.text.clone()),
                Span::styled("▏", muted),
            ];
            if let Some(hint) = footer.hint {
                spans.push(Span::styled(format!("  {hint}"), muted));
            }
            if let Some(warn) = footer.warn {
                spans.push(Span::styled(format!("  {warn}"), Style::default().fg(palette::WARN.color())));
            }
            Line::from(spans)
        }
        FooterKind::Confirm => Line::from(Span::styled(
            format!(" {}", footer.text),
            Style::default().fg(palette::DANGER.color()).add_modifier(Modifier::BOLD),
        )),
        FooterKind::Error => {
            Line::from(Span::styled(format!(" {}", footer.text), Style::default().fg(palette::DANGER.color())))
        }
        FooterKind::Message => {
            Line::from(Span::styled(format!(" {}", footer.text), Style::default().fg(palette::SUCCESS.color())))
        }
        FooterKind::Hint => match footer.tag {
            Some(tag) => {
                let (tag, tag_style) = mode_tag(if tag == "INSERT" { InputMode::Insert } else { InputMode::Normal });
                Line::from(vec![Span::styled(tag, tag_style), Span::styled(format!(" {}", footer.text), muted)])
            }
            None => Line::from(Span::styled(format!(" {}", footer.text), muted)),
        },
    }
}

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
    group_rgb(group).color()
}

fn group_rgb(group: workspace::TaskGroup) -> &'static palette::Rgb {
    match group {
        // Same reasoning as the status bar's glyph priority: a pending
        // secrets request needs a specific action from you, distinct from
        // ordinary waiting — worth its own colour, not folded into WARN.
        workspace::TaskGroup::SecretsPending => &palette::ACCENT,
        workspace::TaskGroup::Waiting => &palette::WARN,
        workspace::TaskGroup::Working => &palette::INFO,
        workspace::TaskGroup::Inactive => &palette::MUTED,
    }
}

/// status colour, plus a gap. Shared by both list shapes.
fn row_glyph(row: &Row, frame: usize) -> (String, Style) {
    let (glyph, rgb) = row_glyph_rgb(row, frame);
    let gap = if glyph == "🔒" { " " } else { "  " };
    (format!("{glyph}{gap}"), Style::default().fg(rgb.color()))
}

/// The row's glyph (no gap) and its colour.
fn row_glyph_rgb(row: &Row, frame: usize) -> (&'static str, &'static palette::Rgb) {
    if row.pending {
        // Still being built: the same spinner the panel shows, so the row and
        // the panel below read as one thing.
        return (crate::progress::FRAMES[frame % crate::progress::FRAMES.len()], &palette::ACCENT);
    }
    if !row.secrets_pending.is_empty() || !row.secrets_pending_set.is_empty() {
        ("🔒", &palette::ACCENT)
    } else {
        (row.status.glyph(), palette::status_color(row.status))
    }
}

/// A task title's colour: selected, the current task, closed (no window,
/// dimmer — ⏎ opens it), or plain.
fn title_rgb(row: &Row, selected: bool, is_current: bool) -> &'static palette::Rgb {
    if selected {
        &palette::SEL_TEXT
    } else if is_current {
        &palette::CURRENT
    } else if row.window_id.is_none() {
        &palette::MUTED
    } else {
        &palette::TEXT
    }
}

/// A PR chip's colour, by its checks.
fn pr_rgb(checks: &str) -> &'static palette::Rgb {
    match checks {
        "failure" => &palette::DANGER,
        "success" => &palette::SUCCESS,
        _ => &palette::INFO,
    }
}

/// What a row wants from you, as chip text with its colours: the secrets it
/// wants unlocked, else Claude Code's own waiting reason. `None` when it
/// wants nothing.
fn row_reason(row: &Row) -> Option<(String, &'static palette::Rgb, &'static palette::Rgb)> {
    if row.pending {
        return Some(("setting up".to_string(), &palette::ACCENT, &palette::CHIP_SECRETS_BG));
    }
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
/// The task shown beside the column (`Column::is_shown`: its window, or a
/// closed task's empty screen) has its title in the "current" colour, and a
/// `▌` in that colour marks both its lines in the indent. No spacer between tasks: the headers already separate the groups,
/// and a column has less height to spare than width.
fn column_items(column: &Column, list_width: usize) -> ListParts {
    const INDENT: usize = 2 + 3; // indent + glyph column
    let mut items = Vec::new();
    let mut line_to_pos: Vec<Option<usize>> = Vec::new();
    let mut line_to_sub: Vec<Option<String>> = Vec::new();
    let on_sub = column.selected_sub();
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
                line_to_sub.push(None);
            }
            let count = group_counts[group.rank() as usize];
            items.push(ListItem::new(Line::from(vec![
                Span::styled(group.label().to_string(), Style::default().fg(group_color(group)).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {count}"), dim),
            ])));
            line_to_pos.push(None);
            line_to_sub.push(None);
            last_group = Some(group);
        }
        if pos == column.selected && on_sub.is_none() {
            selected_line = Some(items.len());
        }

        let selected = pos == column.selected && column.focus == Focus::List && on_sub.is_none();
        let is_current = column.is_shown(row);
        // Closed tasks (no window) read dimmer; ⏎ opens them.
        let title_fg = title_rgb(row, selected, is_current).color();
        let (glyph, glyph_style) = row_glyph(row, column.frame);
        // Sized per row, not per list: a column has no other columns to line
        // up with, so every title gets the whole width.
        let title_w = list_width.saturating_sub(INDENT).max(1);
        // The current task carries a bar in the indent on both lines: a
        // colour change alone was too easy to miss, and the bar costs no
        // width and never reads as the selection's background bar.
        let gutter = || {
            if is_current {
                Span::styled("▌ ", Style::default().fg(palette::CURRENT.color()))
            } else {
                Span::raw("  ")
            }
        };
        let first = Line::from(vec![
            gutter(),
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
        pieces.push(Span::styled(row.ws_name.clone(), Style::default().fg(palette::workspace_color(&row.ws_name).color())));
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
            pieces.push(Span::styled(pr.chip(), Style::default().fg(pr_rgb(&pr.checks).color())));
        }
        if !row.live.ports.is_empty() {
            let ports: Vec<String> = row.live.ports.iter().map(|p| format!(":{p}")).collect();
            pieces.push(Span::styled(ports.join(" "), dim));
        }
        let mut second = vec![gutter(), Span::raw(" ".repeat(INDENT - 2))];
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
        line_to_sub.push(None);

        // The task's subagents, one line each, under its title.
        for (k, a) in row.subagents.iter().enumerate() {
            let on = pos == column.selected && on_sub == Some(k);
            if on {
                selected_line = Some(items.len());
            }
            items.push(ListItem::new(subagent_line(a, list_width, on && column.focus == Focus::List)));
            line_to_pos.push(Some(pos));
            line_to_sub.push(Some(a.id.clone()));
        }
    }

    if items.is_empty() {
        for line in empty_state_lines() {
            items.push(ListItem::new(line));
            line_to_pos.push(None);
            line_to_sub.push(None);
        }
    }
    (items, selected_line, line_to_pos, line_to_sub)
}

/// A rendered list: its items, the item to highlight, and per item the
/// filtered position it selects (`line_to_pos`) and the subagent it is
/// (`line_to_sub`).
type ListParts = (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>, Vec<Option<String>>);

/// A list's items with no subagent lines — the Repos and Work tabs.
fn no_subs((items, selected, line_to_pos): (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>)) -> ListParts {
    let subs = vec![None; line_to_pos.len()];
    (items, selected, line_to_pos, subs)
}

/// One subagent as a child line of its task: indented under the task's
/// title, its status glyph (the task glyph table, `SubagentStatus::as_task_status`),
/// its description, then whether it runs in the background and its type, as
/// far as they fit.
fn subagent_line(a: &Subagent, width: usize, selected: bool) -> Line<'static> {
    const SUB_INDENT: usize = 5; // under the task's title
    let dim = Style::default().fg(palette::MUTED.color());
    let status = a.status.as_task_status();
    let label_fg = if selected {
        palette::SEL_TEXT.color()
    } else if a.status == SubagentStatus::Finished {
        palette::MUTED.color()
    } else {
        palette::TEXT.color()
    };
    // After the label, in priority order, each kept only if it fits whole:
    // that it runs in the background, then its type. (A finished one is
    // listed for half a minute at most — no age worth showing.)
    let mut extras: Vec<String> = Vec::new();
    if a.background && a.status != SubagentStatus::Finished {
        extras.push("bg".to_string());
    }
    if a.description.is_some() {
        extras.push(a.agent_type.clone());
    }
    let room = width.saturating_sub(SUB_INDENT + 2).max(1);
    let label = truncate(a.label(), room);
    let mut used = label.width();
    let mut spans = vec![
        Span::raw(" ".repeat(SUB_INDENT)),
        Span::styled(format!("{} ", status.glyph()), Style::default().fg(palette::status_color(status).color())),
        Span::styled(label, Style::default().fg(label_fg)),
    ];
    for extra in extras {
        let piece = format!(" · {extra}");
        if used + piece.width() <= room {
            used += piece.width();
            spans.push(Span::styled(piece, dim));
        }
    }
    Line::from(spans)
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
                Style::default().fg(palette::workspace_color(&r.ws_name).color()).add_modifier(Modifier::BOLD),
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
            Span::styled(ws_name.clone(), Style::default().fg(palette::workspace_color(&ws_name).color()).add_modifier(Modifier::BOLD)),
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

    let footer = footer_line(&view::footer(column));
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

fn render_newws(f: &mut ratatui::Frame, column: &Column, area: Rect) {
    let Mode::NewWorkspace(form) = &column.mode else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let name_hint = if form.name.is_empty() { "   (optional — the path's last segment)" } else { "" };
    let url_hint = if form.repo_url.is_empty() { "   (optional — a first repo to clone)" } else { "" };
    let check = if form.skills { "[x]" } else { "[ ]" };
    let lines = vec![
        field_line(form.focus == 0, "path", &format!("{}{}", form.path, cursor(form.focus == 0))),
        Line::from(""),
        field_line(form.focus == 1, "name", &format!("{}{}{name_hint}", form.name, cursor(form.focus == 1))),
        Line::from(""),
        field_line(form.focus == 2, "git URL", &format!("{}{}{url_hint}", form.repo_url, cursor(form.focus == 2))),
        Line::from(""),
        field_line(form.focus == NewWorkspaceForm::SKILLS, "skills", &format!("{check} /tenx, /standup and AGENTS.md")),
    ];

    let body = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())).title(" new workspace "));
    f.render_widget(body, chunks[0]);

    let footer = footer_line(&view::footer(column));
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

    let footer = footer_line(&view::footer(column));
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

    // Workspace picker (the first field): ← / → cycle through the
    // registered workspaces when there is more than one.
    let ws_count = column.workspaces.len();
    let ws_focused = form.focus == CreateForm::WORKSPACE;
    // The name in its workspace colour; the arrows keep the field's value style.
    let mut ws_line = field_line(ws_focused, "workspace", "");
    let frame = ws_line.spans.pop().map(|s| s.style).unwrap_or_default();
    let mut ws_style = Style::default().fg(palette::workspace_color(&ws_name).color());
    if ws_focused {
        ws_style = ws_style.add_modifier(Modifier::BOLD);
    }
    if ws_count > 1 {
        ws_line.spans.push(Span::styled("‹ ", frame));
        ws_line.spans.push(Span::styled(ws_name, ws_style));
        ws_line.spans.push(Span::styled(format!(" ›  ({} of {ws_count})", form.ws_idx + 1), frame));
    } else {
        ws_line.spans.push(Span::styled(ws_name, ws_style));
    }
    let mut lines = vec![
        ws_line,
        Line::from(""),
        field_line(form.focus == CreateForm::NAME, "name", &format!("{}▏", form.name)),
        Line::from(""),
        Line::from(Span::styled("  repos", Style::default().fg(palette::MUTED.color()))),
    ];
    if form.repos.is_empty() {
        lines.push(Line::from(Span::styled(
            "  none — the agent runs on its own and can read the workspace",
            Style::default().fg(palette::MUTED.color()),
        )));
    }
    for (i, (name, on)) in form.repos.iter().enumerate() {
        let focused = form.repo_field() == Some(i);
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

    // Agent picker (the last field): ← / → or space cycle it.
    lines.push(Line::from(""));
    let focused = form.focus == form.agent_field();
    let value = if form.agent.is_none() { format!("{}  (inherits default)", form.agent_label()) } else { form.agent_label() };
    lines.push(field_line(focused, "agent", &format!("‹ {value} ›")));

    let body = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(palette::BORDER.color())).title(" new task "));
    f.render_widget(body, chunks[0]);

    let footer = footer_line(&view::footer(column));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn selected_slug(c: &Column) -> &str {
        c.selected_row().map(|r| r.slug.as_str()).unwrap_or("")
    }

    /// `?` from the list and `:help` both open the key overlay; scrolling
    /// stays inside it, any other key closes it without acting on it.
    #[test]
    fn question_mark_and_help_command_open_the_key_overlay() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).unwrap(); // → list
        let before = selected_slug(&c).to_string();
        c.handle_key(plain('?')).unwrap();
        assert!(matches!(c.mode, Mode::Help(0)));
        c.handle_key(plain('j')).unwrap();
        assert!(matches!(c.mode, Mode::Help(1)));
        c.handle_key(plain('x')).unwrap(); // closes; must not close a window
        assert!(matches!(c.mode, Mode::List));
        assert_eq!(selected_slug(&c), before);

        for ch in ":help".chars() {
            c.handle_key(plain(ch)).unwrap();
        }
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert!(matches!(c.mode, Mode::Help(_)));
    }

    /// The overlay lives in a ~36-cell column: keep the key column narrow
    /// enough that every action still has room to be read.
    #[test]
    fn help_keys_fit_the_column() {
        for (_, rows) in KEYS {
            for (key, action) in rows.iter() {
                assert!(key.chars().count() <= 10, "key label too wide: {key}");
                assert!(action.chars().count() <= 26, "action too long: {action}");
            }
        }
        assert!(!help_lines().is_empty());
    }

    /// `n` cycles through the tasks that need you — the blocked one, the
    /// bell, the secrets request — skipping done, working and idle rows,
    /// and the column shows the task it lands on. `:next` does the same
    /// from the search field, starting at the top.
    #[test]
    fn arrows_walk_through_a_tasks_subagents() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        let down = || KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let up = || KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let task = c.filtered.iter().position(|&i| c.rows[i].slug == "column-screenshot").unwrap();
        c.set_cur_sel(task);
        assert_eq!(c.selected_sub(), None);

        // Down enters the task's subagents in their listed order, then leaves.
        c.handle_key(down()).unwrap();
        assert_eq!((c.selected, c.sub.as_deref()), (task, Some("a1")));
        c.handle_key(down()).unwrap();
        assert_eq!((c.selected, c.sub.as_deref()), (task, Some("a2")));
        c.handle_key(down()).unwrap();
        assert_eq!((c.selected, c.selected_sub()), (task + 1, None));

        // Up from the next task lands on the last subagent, then climbs.
        c.handle_key(up()).unwrap();
        assert_eq!((c.selected, c.sub.as_deref()), (task, Some("a2")));
        c.handle_key(up()).unwrap();
        c.handle_key(up()).unwrap();
        assert_eq!((c.selected, c.selected_sub()), (task, None));

        // ⏎ on a subagent follows it, and doesn't open the task.
        c.handle_key(down()).unwrap();
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert_eq!(c.status_msg.as_deref(), Some("following 'Map the session registry'"));
        assert_eq!(c.take_request(), None);

        // The rendered list: the selected line is the subagent's own.
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 60)).unwrap();
        term.draw(|f| render_in(f, &mut c, f.area())).unwrap();
        let item = c.list_state.selected().unwrap();
        assert_eq!(c.line_to_sub[item].as_deref(), Some("a1"));
        assert_eq!(c.line_to_pos[item], Some(task));
    }

    #[test]
    fn a_namesake_in_another_workspace_is_neither_open_nor_current() {
        let mut c = screenshot::fixture_column();
        // Another row becomes the fixture task's namesake: same slug, another
        // workspace. Only the fixture task has a window, tagged with its own
        // directory.
        let mine = c.rows.iter().position(|r| r.slug == "column-screenshot").unwrap();
        let twin = (0..c.rows.len()).find(|&i| i != mine && !c.rows[i].pending).unwrap();
        c.rows[twin].slug = "column-screenshot".into();
        c.rows[twin].ws_name = "other".into();
        c.rows[twin].path = PathBuf::from("/home/you/other/tasks/column-screenshot");
        c.windows.windows = vec![crate::tmux::Window {
            id: "@7".into(),
            name: "column-screenshot".into(),
            active: true,
            bell: false,
            activity: false,
            last_activity: None,
            task_dir: Some(c.rows[mine].path.clone()),
        }];
        for i in [mine, twin] {
            c.rows[i].window_id = c.windows.window_of(&c.rows[i].slug, &c.rows[i].path);
        }
        assert_eq!(c.rows[mine].window_id.as_deref(), Some("@7"));
        assert_eq!(c.rows[twin].window_id, None, "the namesake has no window");

        c.current = c.current_from(Some("@7".into()));
        let current: Vec<usize> = (0..c.rows.len()).filter(|&i| c.is_current(&c.rows[i])).collect();
        assert_eq!(current, [mine], "only the task in the current window is current");
    }

    #[test]
    fn the_marker_follows_whats_shown_onto_a_closed_task() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        let shown = |c: &Column| -> Vec<String> {
            c.filtered.iter().map(|&i| &c.rows[i]).filter(|r| c.is_shown(r)).map(|r| r.slug.clone()).collect()
        };
        assert_eq!(shown(&c), ["column-screenshot"], "the current window's task, to start");

        // The cursor lands on a closed task: the right side shows its empty
        // screen, and the marker goes with it — to it alone.
        let closed = c.filtered.iter().position(|&i| c.rows[i].window_id.is_none() && !c.rows[i].pending).unwrap();
        c.selected = closed;
        let slug = c.rows[c.filtered[closed]].slug.clone();
        assert!(c.selected_closed().is_some());
        assert_eq!(shown(&c), [slug]);

        // The keyboard leaves the column: the empty screen goes, and the
        // marker is back on the window tmux shows.
        c.blur();
        assert_eq!(c.selected_closed(), None);
        assert_eq!(shown(&c), ["column-screenshot"]);
    }

    #[test]
    fn n_cycles_through_tasks_that_need_you() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        assert_eq!(selected_slug(&c), "add-release-workflow"); // the blocked one

        c.handle_key(plain('n')).unwrap();
        assert_eq!(selected_slug(&c), "rotate-signing-keys"); // signaled
        assert_eq!(c.current.as_deref(), c.selected_row().map(|r| r.path.as_path())); // rotate-signing-keys

        c.handle_key(plain('n')).unwrap();
        assert_eq!(selected_slug(&c), "stripe-webhook-signing"); // wraps to secrets pending

        c.handle_key(plain('n')).unwrap();
        assert_eq!(selected_slug(&c), "add-release-workflow");

        // From the search field, `:next` starts at the top.
        c.focus_search();
        c.mode = Mode::Command("next".into());
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert_eq!(c.focus, Focus::List);
        assert_eq!(selected_slug(&c), "stripe-webhook-signing");

        // A filter that hides every such task: the cursor stays, with a message.
        c.focus_search();
        for ch in "cdn".chars() {
            c.handle_key(plain(ch)).unwrap();
        }
        c.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        c.handle_key(plain('n')).unwrap();
        assert_eq!(c.status_msg.as_deref(), Some("nothing needs you"));
    }

    /// Ctrl+n opens the new-task form from either mode; `A`/`D` answer.
    #[test]
    fn ctrl_n_creates_and_a_d_answer() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)).unwrap();
        assert!(matches!(c.mode, Mode::Create(_)));
        c.mode = Mode::List;
        c.focus_search();
        c.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)).unwrap();
        assert!(matches!(c.mode, Mode::Create(_)));
        c.mode = Mode::List;
        // The blocked row is answerable; offline, `answer` reports rather than sends.
        c.focus_list();
        c.set_cur_sel(1);
        assert!(c.selected_answerable());
        c.handle_key(plain('A')).unwrap();
        assert!(c.status_msg.is_some(), "A answers (or explains why not)");
    }

    /// `D` on a SECRETS PENDING row asks for an optional note, then rejects
    /// every pending name; `Esc` leaves the request alone.
    #[test]
    fn d_rejects_pending_secrets() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.focus_list();
        let pos = c.filtered.iter().position(|&i| !c.rows[i].secrets_pending_set.is_empty() || !c.rows[i].secrets_pending.is_empty());
        c.set_cur_sel(pos.expect("fixture has a secrets row"));
        assert!(!c.selected_answerable());
        c.handle_key(plain('D')).unwrap();
        assert!(matches!(c.mode, Mode::Reject(_)));
        c.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        assert!(matches!(c.mode, Mode::List));
        assert!(c.selected_has_secrets(), "esc rejects nothing");
        c.handle_key(plain('D')).unwrap();
        for ch in "no".chars() {
            c.handle_key(plain(ch)).unwrap();
        }
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert!(matches!(c.mode, Mode::List));
        assert!(c.status_msg.as_deref().is_some_and(|m| m.starts_with("rejected STRIPE_WEBHOOK_SECRET")), "{:?}", c.status_msg);
        assert!(!c.selected_has_secrets());
    }

    fn ws(name: &str, repos: &[&str]) -> crate::workspace::Workspace {
        crate::workspace::Workspace {
            dir: std::path::PathBuf::from(format!("/home/you/{name}")),
            config: crate::workspace::WorkspaceConfig {
                schema_version: crate::workspace::CURRENT_SCHEMA,
                name: name.into(),
                kind: String::new(),
                layout: String::new(),
                repos: repos
                    .iter()
                    .map(|n| crate::workspace::RepoConfig { name: n.to_string(), url: format!("git@github.com:acme/{n}.git") })
                    .collect(),
                age_identity: None,
                agent: String::new(),
                agents: std::collections::HashMap::new(),
            },
        }
    }

    /// The new-task form starts in the selected task's workspace, and ←/→
    /// on its workspace field move to another one, bringing that
    /// workspace's repos along; the task is then created there.
    #[test]
    fn create_form_can_change_workspace() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.workspaces = vec![ws("ledger", &["api", "web"]), ws("infra", &["terraform"])];
        // Every fixture row points at workspace 0.
        c.focus_list();
        c.set_cur_sel(1);
        c.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)).unwrap();
        let Mode::Create(f) = &c.mode else { panic!("create form") };
        assert_eq!((f.ws_idx, f.focus), (0, CreateForm::NAME));
        assert_eq!(f.repos.len(), 2);

        // Up from the name lands on the workspace field; → picks the next one.
        c.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).unwrap();
        c.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)).unwrap();
        let Mode::Create(f) = &c.mode else { panic!("create form") };
        assert_eq!((f.ws_idx, f.focus), (1, CreateForm::WORKSPACE));
        assert_eq!(f.repos, vec![("terraform".to_string(), true)]);
        // Wraps around; ← goes back.
        c.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)).unwrap();
        c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)).unwrap();
        let Mode::Create(f) = &c.mode else { panic!("create form") };
        assert_eq!(f.ws_idx, 1);

        // Name it and create: the new row belongs to the chosen workspace.
        c.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).unwrap();
        for ch in "rotate certs".chars() {
            c.handle_key(plain(ch)).unwrap();
        }
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert!(matches!(c.mode, Mode::List));
        let row = c.rows.iter().find(|r| r.slug == "rotate-certs").expect("created");
        assert_eq!((row.ws_idx, row.ws_name.as_str()), (1, "infra"));
        assert_eq!(row.repos, vec!["terraform".to_string()]);
    }

    /// No repo ticked makes a task without worktrees, not an error.
    #[test]
    fn create_form_allows_a_task_without_repos() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.workspaces = vec![ws("ledger", &["api", "web"])];
        c.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)).unwrap();
        let Mode::Create(f) = &mut c.mode else { panic!("create form") };
        f.repos.iter_mut().for_each(|r| r.1 = false);
        for ch in "a question".chars() {
            c.handle_key(plain(ch)).unwrap();
        }
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert!(matches!(c.mode, Mode::List), "{:?}", c.status_msg);
        let row = c.rows.iter().find(|r| r.slug == "a-question").expect("created");
        assert!(row.repos.is_empty());
    }

    /// `:ask` makes a session in the adhoc workspace, titled after the
    /// question, and selects it; without that workspace it says so.
    #[test]
    fn ask_command_creates_an_adhoc_session() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.workspaces = vec![ws("ledger", &["api"])];
        c.focus_list();
        for ch in ":ask why?".chars() {
            c.handle_key(plain(ch)).unwrap();
        }
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert!(c.status_msg.as_deref().is_some_and(|m| m.contains("no adhoc workspace")), "{:?}", c.status_msg);

        let mut adhoc = ws("adhoc", &[]);
        adhoc.config.kind = crate::workspace::ADHOC_KIND.into();
        c.workspaces.push(adhoc);
        for ch in ":ask how does sweep work?".chars() {
            c.handle_key(plain(ch)).unwrap();
        }
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        let row = c.selected_row().expect("selected");
        assert_eq!((row.slug.as_str(), row.title.as_str(), row.ws_idx), ("how-does-sweep-work", "how does sweep work?", 1));
        assert!(row.repos.is_empty());
    }

    /// With no selection but a registered workspace, the form still opens
    /// (in the first workspace); with none at all it says so.
    #[test]
    fn create_form_without_selection() {
        let mut c = Column::empty();
        c.offline = true;
        c.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)).unwrap();
        assert!(matches!(c.mode, Mode::List));
        assert_eq!(c.status_msg.as_deref(), Some("no workspace yet: W creates one"));

        c.workspaces = vec![ws("ledger", &["api"])];
        c.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)).unwrap();
        let Mode::Create(f) = &c.mode else { panic!("create form") };
        assert_eq!(f.ws_idx, 0);
    }
}
