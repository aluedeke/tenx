//! Global task overlay: lists every task across every registered workspace,
//! grouped by workspace, with each task's Claude-activity status. It's both a
//! switcher and a task manager — fuzzy-filter + Enter to jump, plus
//! Ctrl-bindings to create, delete, close, and rename tasks from anywhere
//! (plain typing always filters, so actions live on Ctrl).
//!
//! Single-session model: all tasks live as (invisible) tabs in the one global
//! `tenx` zellij session. The overlay runs in two modes: as the session's
//! *home* base pane (`--home`, long-lived, jump switches tabs without exiting)
//! and as the Ctrl+w *floating* pane (exits after a jump; the tenx-zellij
//! plugin closes the pane).

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph, Tabs},
    Terminal,
};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::mouse::{self, ClickTracker};
use crate::workspace::{self, TaskStatus, Workspace};

/// One selectable task row, flattened across all workspaces.
struct Row {
    ws_idx: usize,
    ws_name: String,
    slug: String,
    title: String,
    path: PathBuf,
    status: TaskStatus,
    changed: Option<SystemTime>,
    tab_id: Option<u32>,
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
    tab_id: Option<u32>,
}

/// Rename-title form.
struct RenameForm {
    slug: String,
    tab_id: Option<u32>,
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
    Confirm(Confirm),
    Rename(RenameForm),
}

/// A task to land on after the overlay tears down its TUI. Recorded when the
/// user jumps while *outside* zellij (a plain terminal): we can't
/// `switch-session` in place, so we exit the overlay cleanly and then attach to
/// the tenx session.
struct AttachTarget {
    tab_id: Option<u32>,
    title: String,
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
    /// Set when a jump from outside zellij should attach after teardown.
    attach: Option<AttachTarget>,

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
    click: ClickTracker,
}

impl Overlay {
    fn new(home: bool) -> Self {
        let workspaces = workspace::registered_workspaces();
        let mut o = Overlay {
            home,
            workspaces,
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
            list_state: ListState::default(),
            line_to_pos: Vec::new(),
            tabs_area: Rect::default(),
            search_area: Rect::default(),
            list_area: Rect::default(),
            click: ClickTracker::new(),
        };
        o.rebuild_rows();
        o
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

    /// Rescan all workspaces for tasks + status. Cheap (small file reads).
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            let mut tasks = ws.tasks().unwrap_or_default();
            tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            for task in tasks {
                let (status, changed) = workspace::read_task_status(&task.path);
                let tab_id = std::fs::read_to_string(task.path.join(".tenx-tab-id"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                rows.push(Row {
                    ws_idx,
                    ws_name: ws.config.name.clone(),
                    slug: task.name.clone(),
                    title: task.display_name.clone(),
                    path: task.path.clone(),
                    status,
                    changed,
                    tab_id,
                });
            }
        }
        self.rows = rows;
        self.apply_filter();
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
    /// search box, or a task/repo row focuses it; a double-click on a row jumps.
    /// Returns `Ok(true)` when the overlay should close (a jump completed).
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
                    if self.click.click(pos) {
                        return self.jump();
                    }
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
            Confirm,
            Rename,
        }
        let kind = match self.mode {
            Mode::List => Kind::List,
            Mode::Command(_) => Kind::Command,
            Mode::Create(_) => Kind::Create,
            Mode::AddRepo(_) => Kind::AddRepo,
            Mode::Confirm(_) => Kind::Confirm,
            Mode::Rename(_) => Kind::Rename,
        };
        let close = match kind {
            Kind::List => self.handle_list_key(key),
            Kind::Command => self.handle_command_key(key),
            Kind::Create => self.handle_create_key(key),
            Kind::AddRepo => self.handle_addrepo_key(key),
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
                ('d', KeyCode::Char('d')) => {
                    if self.require_tasks() {
                        self.start_delete();
                    }
                }
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
            KeyCode::Char('a') => match self.tab {
                Tab::Tasks => self.start_create(),
                Tab::Repos => self.start_add_repo(),
            },
            KeyCode::Char('r') => {
                if self.require_tasks() {
                    self.start_rename();
                }
            }
            KeyCode::Char('x') => {
                if self.require_tasks() {
                    self.close_selected_tab();
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
            "x" | "close" => self.close_selected_tab(),
            "o" | "open" => return self.jump(),
            other => self.status_msg = Some(format!("unknown command: :{other}")),
        }
        Ok(false)
    }

    // ── Jump ──────────────────────────────────────────────────────────────────

    fn jump(&mut self) -> Result<bool> {
        let Some(row) = self.selected_row() else {
            return Ok(false);
        };
        let ws_idx = row.ws_idx;
        let slug = row.slug.clone();
        let tab_id = row.tab_id;
        let title = row.title.clone();

        match crate::zellij::current_session() {
            Some(cur) if cur == crate::zellij::SESSION => {
                let ws = &self.workspaces[ws_idx];
                if let Err(e) = crate::cli::task::open_in(ws, &slug) {
                    self.status_msg = Some(e.to_string());
                    return Ok(false);
                }
                if self.home {
                    // Stay alive as the session's home pane — zellij already
                    // switched the visible tab to the task. Reset the filter so
                    // the next visit starts fresh.
                    self.filter.clear();
                    self.apply_filter();
                    self.focus_search();
                    self.status_msg = None;
                    return Ok(false);
                }
                Ok(true)
            }
            // Inside a *foreign* zellij session: focus the task's tab in the
            // tenx session and switch the client there in place. If the tenx
            // session doesn't exist yet, create-and-switch to it (lands in the
            // home overlay; the exact task is one Enter away there).
            Some(_) => {
                let res = if crate::zellij::session_exists(crate::zellij::SESSION).unwrap_or(false) {
                    crate::zellij::switch_to_task(crate::zellij::SESSION, tab_id, &title)
                } else {
                    let bin = std::env::current_exe()?;
                    crate::zellij::switch_to_tenx_session(&bin.to_string_lossy())
                };
                match res {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        self.status_msg = Some(e.to_string());
                        Ok(false)
                    }
                }
            }
            // Outside zellij entirely (a plain terminal) → can't switch in
            // place. Record the target and close; `run()` attaches after the TUI
            // tears down, so zellij gets a clean terminal.
            None => {
                self.attach = Some(AttachTarget { tab_id, title });
                Ok(true)
            }
        }
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
                Ok(()) => return Ok(false), // created; stay in List
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
            KeyCode::Char(c) if !ctrl => {
                if form.focus == 0 {
                    form.name.push(c);
                }
            }
            _ => {}
        }
        self.mode = Mode::Create(form);
        Ok(false)
    }

    fn submit_create(&mut self, form: &CreateForm) -> Result<(), String> {
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
        // no_open=true: the task's workspace may not be the current session, so
        // we don't create a tab here — the user jumps to it (Enter) afterwards.
        {
            let ws = &self.workspaces[ws_idx];
            crate::cli::task::new_in(ws, &name, Some(&repos), true).map_err(|e| e.to_string())?;
        }
        self.filter.clear();
        self.rebuild_rows();
        if let Some(pos) = self
            .filtered
            .iter()
            .position(|&i| self.rows[i].ws_idx == ws_idx && self.rows[i].slug == slug)
        {
            self.selected = pos;
        }
        self.status_msg = Some(format!("created '{name}'"));
        Ok(())
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

    // ── Delete ────────────────────────────────────────────────────────────────

    fn start_delete(&mut self) {
        if let Some(r) = self.selected_row() {
            self.mode = Mode::Confirm(Confirm {
                ws_idx: r.ws_idx,
                slug: r.slug.clone(),
                title: r.title.clone(),
                tab_id: r.tab_id,
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
        // Close the tab first (best-effort) so it doesn't linger after the dir goes.
        if let Some(id) = confirm.tab_id {
            let _ = crate::zellij::close_tab_in(crate::zellij::SESSION, id);
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
        match r.tab_id {
            Some(id) => match crate::zellij::close_tab_in(crate::zellij::SESSION, id) {
                Ok(()) => {
                    let _ = std::fs::remove_file(path.join(".tenx-tab-id"));
                    self.rebuild_rows();
                    self.status_msg = Some("closed tab".into());
                }
                Err(e) => self.status_msg = Some(e.to_string()),
            },
            None => self.status_msg = Some("no open tab".into()),
        }
    }

    // ── Rename ────────────────────────────────────────────────────────────────

    fn start_rename(&mut self) {
        if let Some(r) = self.selected_row() {
            self.mode = Mode::Rename(RenameForm {
                slug: r.slug.clone(),
                tab_id: r.tab_id,
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
                        if let Some(id) = form.tab_id {
                            let _ = crate::zellij::rename_tab_in(crate::zellij::SESSION, id, &title);
                        }
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

// Note: single-instance/toggle behaviour lives in the tenx-zellij plugin now
// (it tracks the overlay pane it opened and closes it on the next toggle). The
// old per-session lock-file toggle was removed: with the plugin owning the
// lifecycle it could only misfire — a second spawn would SIGTERM the first and
// exit, so the overlay flickered and vanished instead of opening.

pub fn run(home: bool) -> Result<()> {
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, DisableMouseCapture);
        orig(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut overlay = Overlay::new(home);
    let result = run_loop(&mut terminal, &mut overlay);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    result?;

    // The TUI is fully torn down now. If a jump from outside zellij was queued,
    // attach to (or create) the tenx session; attach/create exec() and replace
    // this process.
    if let Some(t) = overlay.attach.take() {
        if crate::zellij::session_exists(crate::zellij::SESSION)? {
            crate::zellij::pre_focus_tab(crate::zellij::SESSION, t.tab_id, &t.title);
            crate::zellij::attach_session(crate::zellij::SESSION)?;
        } else {
            let bin = std::env::current_exe()?;
            crate::zellij::create_and_attach_session(&bin.to_string_lossy())?;
        }
    }
    Ok(())
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
                }
                Event::Mouse(m) => {
                    if overlay.handle_mouse(m)? {
                        break;
                    }
                }
                _ => {}
            }
        } else if matches!(overlay.mode, Mode::List) {
            // Idle tick — refresh status while preserving the selected task.
            let keep = overlay.selected_row().map(|r| (r.ws_idx, r.slug.clone()));
            overlay.rebuild_rows();
            if let Some((ws_idx, slug)) = keep
                && let Some(pos) = overlay
                    .filtered
                    .iter()
                    .position(|&i| overlay.rows[i].ws_idx == ws_idx && overlay.rows[i].slug == slug)
            {
                overlay.selected = pos;
            }
        }
    }
    Ok(())
}

// Responsive sizing note: the overlay pane's geometry is decided *before* this
// process exists — the tenx-zellij plugin computes breakpoints from the screen
// size and opens this TUI in a floating pane already at the right size. The TUI
// therefore always renders full-pane (`f.area()`).

fn render(f: &mut ratatui::Frame, overlay: &mut Overlay) {
    // Dispatch on a discriminant (not `match &overlay.mode`) so the list path can
    // take `&mut overlay` without a live immutable borrow of `overlay.mode`.
    if matches!(overlay.mode, Mode::Create(_)) {
        render_create(f, overlay);
    } else if matches!(overlay.mode, Mode::AddRepo(_)) {
        render_addrepo(f, overlay);
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

    // Record clickable chrome/list areas for the mouse handler.
    overlay.tabs_area = chunks[0];
    overlay.search_area = chunks[1];
    overlay.list_area = chunks[2];

    // ── Tab bar (its own row, not on a border) ────────────────────────────────
    let tabs = Tabs::new(vec![" Tasks ", " Repos "])
        .select(if overlay.tab == Tab::Tasks { 0 } else { 1 })
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .divider(Span::styled("│", Style::default().fg(Color::DarkGray)));
    f.render_widget(tabs, chunks[0]);

    // ── Search box (or the rename input) ──────────────────────────────────────
    let title = if matches!(overlay.mode, Mode::Rename(_)) {
        " rename task "
    } else {
        ""
    };
    let (prefix, prefix_style, value) = match &overlay.mode {
        Mode::Rename(form) => ("✎ ", Style::default().fg(Color::Magenta), form.buffer.clone()),
        _ => ("🔎 ", Style::default().fg(Color::Cyan), overlay.filter.clone()),
    };
    // Cursor bar only when the cursor lives in the search field (or renaming).
    let show_cursor = matches!(overlay.mode, Mode::Rename(_)) || overlay.focus == Focus::Search;
    let mut top_spans = vec![Span::styled(prefix, prefix_style), Span::raw(value)];
    if show_cursor {
        top_spans.push(Span::styled("▏", Style::default().fg(Color::DarkGray)));
    }
    let top = Paragraph::new(Line::from(top_spans))
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(top, chunks[1]);

    // ── Body list (tasks or repos) ────────────────────────────────────────────
    let list_width = chunks[2].width.saturating_sub(2) as usize;
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
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Never);
    f.render_stateful_widget(list, chunks[2], &mut overlay.list_state);

    // ── Footer / hints ────────────────────────────────────────────────────────
    let footer = match (&overlay.mode, &overlay.status_msg) {
        (Mode::Command(buf), _) => {
            let mut spans = vec![
                Span::styled(":", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(buf.clone()),
                Span::styled("▏", Style::default().fg(Color::DarkGray)),
            ];
            if buf.is_empty() {
                spans.push(Span::styled(
                    "  new · open · delete · rename · close · quit",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        }
        (Mode::Confirm(c), _) => Line::from(Span::styled(
            format!(" delete '{}' + worktrees?   y = delete   n/esc = cancel", c.title),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        (Mode::Rename(_), _) => Line::from(Span::styled(
            " ⏎ save   esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
        (_, Some(msg)) => Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(Color::Green),
        )),
        _ => {
            let (tag, tag_style) = match overlay.input_mode {
                InputMode::Insert => (
                    " INSERT ",
                    Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                InputMode::Normal => (
                    " NORMAL ",
                    Style::default().fg(Color::Black).bg(Color::Blue).add_modifier(Modifier::BOLD),
                ),
            };
            let hint = match (overlay.input_mode, overlay.tab) {
                (InputMode::Insert, _) => "  type to filter · ↓ list · ⏎ open · ⇥ tab",
                (InputMode::Normal, Tab::Tasks) => {
                    "  j/k move · ↑ search · a new · dd del · r rename · x close · gt tab · q quit"
                }
                (InputMode::Normal, Tab::Repos) => {
                    "  j/k move · ↑ search · a add-repo · gt tab · q quit"
                }
            };
            Line::from(vec![
                Span::styled(tag, tag_style),
                Span::styled(hint, Style::default().fg(Color::DarkGray)),
            ])
        }
    };
    f.render_widget(Paragraph::new(footer), chunks[3]);
}

/// Build the grouped task list; returns items, the selected line index, and a
/// per-line map back to the filtered position (None for headers/separators).
fn task_items(
    overlay: &Overlay,
    list_width: usize,
) -> (Vec<ListItem<'static>>, Option<usize>, Vec<Option<usize>>) {
    let mut items = Vec::new();
    let mut line_to_pos: Vec<Option<usize>> = Vec::new();
    let mut selected_line = None;
    let mut last_ws: Option<usize> = None;

    for (pos, &row_idx) in overlay.filtered.iter().enumerate() {
        let row = &overlay.rows[row_idx];
        if last_ws != Some(row.ws_idx) {
            if last_ws.is_some() {
                items.push(ListItem::new(Line::from("")));
                line_to_pos.push(None);
            }
            items.push(ListItem::new(Line::from(Span::styled(
                row.ws_name.clone(),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ))));
            line_to_pos.push(None);
            last_ws = Some(row.ws_idx);
        }
        if pos == overlay.selected {
            selected_line = Some(items.len());
        }

        let glyph = match row.status {
            TaskStatus::Attention => "💬 ",
            TaskStatus::Idle => "   ",
        };
        let title_style = if row.status == TaskStatus::Attention {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let age = row
            .changed
            .filter(|_| row.status == TaskStatus::Attention)
            .map(workspace::format_age)
            .unwrap_or_default();
        let mut right = String::new();
        if row.tab_id.is_some() {
            right.push_str("open");
        }
        if !age.is_empty() {
            if !right.is_empty() {
                right.push_str("  ");
            }
            right.push_str(&age);
        }

        let mut spans = vec![
            Span::raw("  "),
            Span::raw(glyph),
            Span::styled(row.title.clone(), title_style),
        ];
        if !right.is_empty() {
            let left_w = 2 + 3 + row.title.chars().count();
            let pad = list_width.saturating_sub(left_w + right.chars().count()).max(1);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(right, Style::default().fg(Color::DarkGray)));
        }
        items.push(ListItem::new(Line::from(spans)));
        line_to_pos.push(Some(pos));
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  no tasks — :n to create one",
            Style::default().fg(Color::DarkGray),
        ))));
        line_to_pos.push(None);
    }
    (items, selected_line, line_to_pos)
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
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
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
                Style::default().fg(Color::Green),
                Style::default().fg(Color::White),
                r.commit.clone().unwrap_or_else(|| "—".into()),
            )
        } else {
            (
                "○ ",
                Style::default().fg(Color::DarkGray),
                Style::default().fg(Color::DarkGray),
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
            Span::styled(detail, Style::default().fg(Color::DarkGray)),
        ])));
        line_to_pos.push(Some(pos));
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  no repos",
            Style::default().fg(Color::DarkGray),
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
            Span::styled("  workspace  ", Style::default().fg(Color::DarkGray)),
            Span::styled(ws_name, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
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
        .block(Block::default().borders(Borders::ALL).title(" add repo "));
    f.render_widget(body, chunks[0]);

    let footer = if let Some(msg) = &overlay.status_msg {
        Line::from(Span::styled(format!(" {msg}"), Style::default().fg(Color::Red)))
    } else {
        Line::from(Span::styled(
            " ⏎ clone & add   esc cancel   ⇥ next field",
            Style::default().fg(Color::DarkGray),
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
            Span::styled("  workspace  ", Style::default().fg(Color::DarkGray)),
            Span::styled(ws_name, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        field_line(form.focus == 0, "name", &format!("{}▏", form.name)),
        Line::from(""),
        Line::from(Span::styled("  repos", Style::default().fg(Color::DarkGray))),
    ];
    for (i, (name, on)) in form.repos.iter().enumerate() {
        let focused = form.focus == 1 + i;
        let check = if *on { "[x]" } else { "[ ]" };
        let prefix = if focused { "▸ " } else { "  " };
        let style = if focused {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else if *on {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        lines.push(Line::from(Span::styled(format!("{prefix}{check} {name}"), style)));
    }

    let body = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" new task "));
    f.render_widget(body, chunks[0]);

    let footer = if let Some(msg) = &overlay.status_msg {
        Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::from(Span::styled(
            " ⏎ create   esc cancel   ⇥ next   space toggle repo",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

fn field_line<'a>(focused: bool, label: &str, value: &str) -> Line<'a> {
    let prefix = if focused { "▸ " } else { "  " };
    let label_style = if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let value_style = if focused {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    Line::from(vec![
        Span::styled(format!("{prefix}{label}: "), label_style),
        Span::styled(value.to_string(), value_style),
    ])
}
