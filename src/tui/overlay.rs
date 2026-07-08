//! Global "switch" overlay: a floating pane that lists every task across every
//! registered workspace, grouped by workspace, with each task's Claude-activity
//! status. Fuzzy-filter and hit Enter to jump straight to that task.
//!
//! Phase 1 (current, multi-session): jumping to a task in the *current*
//! workspace focuses its tab instantly. Jumping to a task in a *different*
//! workspace switches to that workspace's session in place. Phase 2 (single
//! session) collapses everything so every jump is a plain tab focus.

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph},
    Terminal,
};
use std::io;
use std::time::{Duration, SystemTime};

use crate::workspace::{self, TaskStatus, Workspace};

/// One selectable task row, flattened across all workspaces.
struct Row {
    ws_idx: usize,
    ws_name: String,
    session: String,
    slug: String,
    title: String,
    status: TaskStatus,
    changed: Option<SystemTime>,
    tab_id: Option<u32>,
}

struct Overlay {
    workspaces: Vec<Workspace>,
    rows: Vec<Row>,
    filter: String,
    /// Indices into `rows` that pass the current filter, in display order.
    filtered: Vec<usize>,
    /// Position within `filtered`.
    selected: usize,
    status_msg: Option<String>,
}

impl Overlay {
    fn new() -> Self {
        let workspaces = workspace::registered_workspaces();
        let mut o = Overlay {
            workspaces,
            rows: vec![],
            filter: String::new(),
            filtered: vec![],
            selected: 0,
            status_msg: None,
        };
        o.rebuild_rows();
        o
    }

    /// Rescan all workspaces for tasks + status. Cheap (small file reads), so we
    /// call it on every tick to keep the 💬 indicators live.
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            let session = crate::zellij::session_name(&ws.config.name);
            let mut tasks = ws.tasks().unwrap_or_default();
            // Newest first within a workspace (tasks() already sorts this way).
            tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            for task in tasks {
                let (status, changed) = workspace::read_task_status(&task.path);
                let tab_id = std::fs::read_to_string(task.path.join(".tenx-tab-id"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                rows.push(Row {
                    ws_idx,
                    ws_name: ws.config.name.clone(),
                    session: session.clone(),
                    slug: task.name.clone(),
                    title: task.display_name.clone(),
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
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
        }
    }

    fn move_up(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = if self.selected == 0 {
                self.filtered.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        self.filtered.get(self.selected).and_then(|&i| self.rows.get(i))
    }

    /// Returns `Ok(true)` when the overlay should close (a jump happened).
    fn jump(&mut self) -> Result<bool> {
        let Some(row) = self.selected_row() else {
            return Ok(false);
        };
        let ws_idx = row.ws_idx;
        let slug = row.slug.clone();
        let target = row.session.clone();
        let tab_id = row.tab_id;
        let title = row.title.clone();

        match crate::zellij::current_session() {
            // Same workspace as the overlay → focus the tab directly. Instant.
            Some(cur) if cur == target => {
                let ws = &self.workspaces[ws_idx];
                if let Err(e) = crate::cli::task::open_in(ws, &slug) {
                    self.status_msg = Some(e.to_string());
                    return Ok(false);
                }
                Ok(true)
            }
            // Different workspace → focus the task's tab in that session, then
            // switch to it in place (lands on the exact task).
            Some(_) => match crate::zellij::switch_to_task(&target, tab_id, &title) {
                Ok(()) => Ok(true),
                Err(e) => {
                    self.status_msg = Some(e.to_string());
                    Ok(false)
                }
            },
            None => {
                self.status_msg =
                    Some("run the overlay from inside a zellij session".to_string());
                Ok(false)
            }
        }
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

pub fn run() -> Result<()> {
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

    let mut overlay = Overlay::new();
    let result = run_loop(&mut terminal, &mut overlay);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    result
}

const TICK: Duration = Duration::from_millis(500);

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    overlay: &mut Overlay,
) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, overlay))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => break,
                    KeyCode::Char('c') if ctrl => break,
                    KeyCode::Enter => {
                        if overlay.jump()? {
                            break;
                        }
                    }
                    KeyCode::Down => overlay.move_down(),
                    KeyCode::Up => overlay.move_up(),
                    KeyCode::Char('n') | KeyCode::Char('j') if ctrl => overlay.move_down(),
                    KeyCode::Char('p') | KeyCode::Char('k') if ctrl => overlay.move_up(),
                    KeyCode::Backspace => {
                        overlay.status_msg = None;
                        overlay.filter.pop();
                        overlay.apply_filter();
                    }
                    KeyCode::Char(c) if !ctrl => {
                        overlay.status_msg = None;
                        overlay.filter.push(c);
                        overlay.apply_filter();
                    }
                    _ => {}
                }
            }
        } else {
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

fn render(f: &mut ratatui::Frame, overlay: &Overlay) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    // ── Search box ────────────────────────────────────────────────────────────
    let search = Paragraph::new(Line::from(vec![
        Span::styled("🔎 ", Style::default().fg(Color::Cyan)),
        Span::raw(&overlay.filter),
        Span::styled("▏", Style::default().fg(Color::DarkGray)),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" switch task "));
    f.render_widget(search, chunks[0]);

    // ── Grouped task list ─────────────────────────────────────────────────────
    // Content width inside the list box (minus the left/right borders), used to
    // right-align the age/open metadata column.
    let list_width = chunks[1].width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = Vec::new();
    let mut line_of_selected: Option<usize> = None;
    let mut last_ws: Option<usize> = None;

    for (pos, &row_idx) in overlay.filtered.iter().enumerate() {
        let row = &overlay.rows[row_idx];

        // Group header when the workspace changes.
        if last_ws != Some(row.ws_idx) {
            if last_ws.is_some() {
                items.push(ListItem::new(Line::from("")));
            }
            items.push(ListItem::new(Line::from(Span::styled(
                row.ws_name.clone(),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ))));
            last_ws = Some(row.ws_idx);
        }

        if pos == overlay.selected {
            line_of_selected = Some(items.len());
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

        // Right-aligned metadata column: "open" tag, then age (attention only).
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
            // "💬 " and the idle glyph both occupy 3 cells; 2 = the leading indent.
            let left_w = 2 + 3 + row.title.chars().count();
            let pad = list_width.saturating_sub(left_w + right.chars().count()).max(1);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(right, Style::default().fg(Color::DarkGray)));
        }
        items.push(ListItem::new(Line::from(spans)));
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  no tasks — create one with `tenx task new`",
            Style::default().fg(Color::DarkGray),
        ))));
    }

    let mut state = ListState::default();
    state.select(line_of_selected);
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Never);
    f.render_stateful_widget(list, chunks[1], &mut state);

    // ── Footer / hints ────────────────────────────────────────────────────────
    let footer = if let Some(msg) = &overlay.status_msg {
        Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::from(Span::styled(
            " ↑↓ move   ⏎ jump   esc close   type to filter",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(Paragraph::new(footer), chunks[2]);
}
