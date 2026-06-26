use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{io, time::Duration};

use crate::workspace::Workspace;

pub fn run(workspace: Workspace) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(workspace);
    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    result
}

// ── App state ─────────────────────────────────────────────────────────────────

enum View { List, Add }

struct App {
    workspace: Workspace,
    selected: usize,
    view: View,
    status_msg: Option<String>,
    add_url: String,
    add_name: String,
    add_focus: usize,
}

impl App {
    fn new(workspace: Workspace) -> Self {
        App {
            workspace,
            selected: 0,
            view: View::List,
            status_msg: None,
            add_url: String::new(),
            add_name: String::new(),
            add_focus: 0,
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 { self.selected -= 1; }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.workspace.config.repos.len() {
            self.selected += 1;
        }
    }

    fn enter_add(&mut self) {
        self.add_url.clear();
        self.add_name.clear();
        self.add_focus = 0;
        self.status_msg = None;
        self.view = View::Add;
    }

    fn cancel_add(&mut self) {
        self.view = View::List;
        self.status_msg = None;
    }

    fn add_focus_next(&mut self) {
        self.add_focus = (self.add_focus + 1) % 2;
        self.autofill_name();
    }

    fn autofill_name(&mut self) {
        if self.add_focus == 1 && self.add_name.is_empty() {
            self.add_name = infer_name(&self.add_url);
        }
    }

    fn do_add(&mut self) -> Result<(), String> {
        let url = self.add_url.trim().to_string();
        if url.is_empty() { return Err("URL cannot be empty".into()); }
        let name = {
            let n = self.add_name.trim().to_string();
            if n.is_empty() { infer_name(&url) } else { n }
        };
        match crate::cli::repo::add(&url, Some(&name)) {
            Ok(_) => {
                self.view = View::List;
                self.reload();
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn reload(&mut self) {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(ws) = crate::workspace::find(&cwd) {
                if self.selected >= ws.config.repos.len() && !ws.config.repos.is_empty() {
                    self.selected = ws.config.repos.len() - 1;
                }
                self.workspace = ws;
            }
        }
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

const TICK: Duration = Duration::from_millis(500);

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, app))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if app.status_msg.is_some() { app.status_msg = None; }

                match app.view {
                    View::List => {
                        if key.modifiers == KeyModifiers::NONE
                            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                        {
                            break;
                        }
                        handle_list_key(app, key);
                    }
                    View::Add => handle_add_key(app, key),
                }
            }
        }
    }
    Ok(())
}

fn handle_list_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up   => app.move_up(),
        KeyCode::Char('a')                 => app.enter_add(),
        KeyCode::Char('r')                 => app.reload(),
        _ => {}
    }
}

fn handle_add_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc  => app.cancel_add(),
        KeyCode::Tab | KeyCode::Down => app.add_focus_next(),
        KeyCode::Up   => { app.add_focus = if app.add_focus == 0 { 1 } else { 0 }; }
        KeyCode::Enter => {
            if let Err(e) = app.do_add() { app.status_msg = Some(e); }
        }
        KeyCode::Backspace => {
            if app.add_focus == 0 { app.add_url.pop(); } else { app.add_name.pop(); }
        }
        KeyCode::Char(c) => {
            if app.add_focus == 0 { app.add_url.push(c); } else { app.add_name.push(c); }
        }
        _ => {}
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &App) {
    render_list(f, app);
    if matches!(app.view, View::Add) {
        render_add_popup(f, app);
    }
}

fn render_list(f: &mut Frame, app: &App) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let max_w = area.width.saturating_sub(2) as usize;
    let global = crate::workspace::load_global().unwrap_or_default();
    let bare_dir = app.workspace.bare_dir(&global);

    if app.workspace.config.repos.is_empty() {
        let hint = vec![
            Line::from(""),
            Line::from(Span::styled("  No repos yet.", Style::default().fg(Color::DarkGray))),
            Line::from(""),
            Line::from(vec![
                Span::styled("  a  ", Style::default().fg(Color::Cyan)),
                Span::styled("add repo", Style::default().fg(Color::DarkGray)),
            ]),
        ];
        f.render_widget(Paragraph::new(hint), chunks[0]);
    } else {
        let items: Vec<ListItem> = app.workspace.config.repos.iter().map(|repo| {
            let bare_path = crate::git::bare_repo_path(&bare_dir, &repo.name);
            let cloned = bare_path.exists();
            let status = if cloned {
                Span::styled("✓ ", Style::default().fg(Color::Green))
            } else {
                Span::styled("✗ ", Style::default().fg(Color::Red))
            };
            let name_line = Line::from(vec![
                status,
                Span::styled(
                    truncate(&repo.name, max_w.saturating_sub(2)).to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]);
            let url_line = Line::from(Span::styled(
                format!("  {}", truncate(&short_url(&repo.url), max_w.saturating_sub(2))),
                Style::default().fg(Color::Gray),
            ));
            let commit_line = if cloned {
                let c = crate::git::last_commit(&bare_path).unwrap_or_else(|| "—".into());
                Line::from(Span::styled(
                    format!("  {}", truncate(&c, max_w.saturating_sub(2))),
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                Line::from(Span::styled("  not cloned", Style::default().fg(Color::DarkGray)))
            };
            ListItem::new(vec![name_line, url_line, commit_line, Line::from("")])
        }).collect();

        let mut state = ListState::default();
        state.select(Some(app.selected));

        let list = List::new(items)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, chunks[0], &mut state);
    }

    let (help, style) = if let Some(ref msg) = app.status_msg {
        (format!(" {} ", msg), Style::default().fg(Color::Red))
    } else {
        (" a add · r refresh · j/k navigate · q quit".into(), Style::default().fg(Color::DarkGray))
    };
    f.render_widget(Paragraph::new(help).style(style), chunks[1]);
}

fn render_add_popup(f: &mut Frame, app: &App) {
    let area  = f.area();
    let popup = centered_rect(90, 50, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Add Repo ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    f.render_widget(Paragraph::new("  URL:"), chunks[1]);
    let url_style = if app.add_focus == 0 { Style::default().fg(Color::Yellow) } else { Style::default() };
    f.render_widget(
        Paragraph::new(format!("  [{}]", app.add_url)).style(url_style),
        chunks[2],
    );

    f.render_widget(Paragraph::new("  Name: (optional, inferred from URL)"), chunks[4]);
    let name_style = if app.add_focus == 1 { Style::default().fg(Color::Yellow) } else { Style::default() };
    f.render_widget(
        Paragraph::new(format!("  [{}]", app.add_name)).style(name_style),
        chunks[5],
    );

    let help = if let Some(ref msg) = app.status_msg {
        Paragraph::new(format!("  {}", msg)).style(Style::default().fg(Color::Red))
    } else {
        Paragraph::new("  Tab · Enter add · Esc cancel").style(Style::default().fg(Color::DarkGray))
    };
    f.render_widget(help, chunks[7]);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

fn short_url(url: &str) -> String {
    let s = url.trim_end_matches(".git");
    if let Some(r) = s.strip_prefix("git@")    { return r.to_string(); }
    if let Some(r) = s.strip_prefix("https://") { return r.to_string(); }
    if let Some(r) = s.strip_prefix("http://")  { return r.to_string(); }
    s.to_string()
}

fn infer_name(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).trim_end_matches(".git").to_string()
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vert[1])[1]
}
