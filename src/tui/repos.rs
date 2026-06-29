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
use std::{
    collections::HashMap,
    io,
    sync::mpsc,
    time::Duration,
};

use crate::github::Pr;
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

// ── Types ──────────────────────────────────────────────────────────────────────

enum View {
    List,
    Prs,
    Add,
}

enum PrState {
    Loading,
    Loaded(Vec<Pr>),
    Error(String),
}

enum Msg {
    PrResult { repo_name: String, result: Result<Vec<Pr>, String> },
    GithubUser(String),
}

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    workspace: Workspace,
    selected: usize,
    view: View,
    status_msg: Option<String>,
    // add view
    add_url: String,
    add_name: String,
    add_focus: usize,
    // pr view
    pr_cache: HashMap<String, PrState>,
    pr_selected: usize,
    mine_only: bool,
    github_user: Option<String>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

impl App {
    fn new(workspace: Workspace) -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();

        // Fetch GitHub username in the background.
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            if let Ok(user) = crate::github::current_user() {
                let _ = tx2.send(Msg::GithubUser(user));
            }
        });

        App {
            workspace,
            selected: 0,
            view: View::List,
            status_msg: None,
            add_url: String::new(),
            add_name: String::new(),
            add_focus: 0,
            pr_cache: HashMap::new(),
            pr_selected: 0,
            mine_only: false,
            github_user: None,
            tx,
            rx,
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
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
        if url.is_empty() {
            return Err("URL cannot be empty".into());
        }
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

    fn enter_prs(&mut self) {
        if self.workspace.config.repos.is_empty() {
            return;
        }
        let repo = &self.workspace.config.repos[self.selected];
        let repo_name = repo.name.clone();
        let repo_url = repo.url.clone();

        self.pr_selected = 0;
        self.view = View::Prs;

        if !matches!(
            self.pr_cache.get(&repo_name),
            Some(PrState::Loaded(_) | PrState::Loading)
        ) {
            self.pr_cache.insert(repo_name.clone(), PrState::Loading);
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let result = crate::github::list_prs(&repo_url).map_err(|e| e.to_string());
                let _ = tx.send(Msg::PrResult { repo_name, result });
            });
        }
    }

    fn refresh_prs(&mut self) {
        if self.workspace.config.repos.is_empty() {
            return;
        }
        let repo = &self.workspace.config.repos[self.selected];
        let repo_name = repo.name.clone();
        let repo_url = repo.url.clone();
        self.pr_cache.insert(repo_name.clone(), PrState::Loading);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::github::list_prs(&repo_url).map_err(|e| e.to_string());
            let _ = tx.send(Msg::PrResult { repo_name, result });
        });
    }

    fn visible_prs(&self) -> Vec<&Pr> {
        let repo = match self.workspace.config.repos.get(self.selected) {
            Some(r) => r,
            None => return vec![],
        };
        match self.pr_cache.get(&repo.name) {
            Some(PrState::Loaded(prs)) => prs
                .iter()
                .filter(|p| {
                    !self.mine_only
                        || self.github_user.as_deref() == Some(p.author.as_str())
                })
                .collect(),
            _ => vec![],
        }
    }

    fn move_pr_up(&mut self) {
        if self.pr_selected > 0 {
            self.pr_selected -= 1;
        }
    }

    fn move_pr_down(&mut self) {
        let count = self.visible_prs().len();
        if self.pr_selected + 1 < count {
            self.pr_selected += 1;
        }
    }

    fn open_selected_pr(&self) {
        let prs = self.visible_prs();
        if let Some(pr) = prs.get(self.pr_selected) {
            let _ = std::process::Command::new("open").arg(&pr.url).spawn();
        }
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

const TICK: Duration = Duration::from_millis(250);

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, app))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if app.status_msg.is_some() {
                    app.status_msg = None;
                }

                match app.view {
                    View::List => {
                        if key.modifiers == KeyModifiers::NONE
                            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                        {
                            break;
                        }
                        handle_list_key(app, key);
                    }
                    View::Prs => handle_pr_key(app, key),
                    View::Add => handle_add_key(app, key),
                }
            }
        }

        // Drain background messages.
        while let Ok(msg) = app.rx.try_recv() {
            match msg {
                Msg::PrResult { repo_name, result } => {
                    let state = match result {
                        Ok(prs) => PrState::Loaded(prs),
                        Err(e) => PrState::Error(e),
                    };
                    app.pr_cache.insert(repo_name, state);
                }
                Msg::GithubUser(user) => {
                    app.github_user = Some(user);
                }
            }
        }
    }
    Ok(())
}

fn handle_list_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Char('a') => app.enter_add(),
        KeyCode::Char('r') => app.reload(),
        KeyCode::Enter | KeyCode::Char('p') => app.enter_prs(),
        _ => {}
    }
}

fn handle_pr_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.view = View::List,
        KeyCode::Char('j') | KeyCode::Down => app.move_pr_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_pr_up(),
        KeyCode::Char('m') => {
            app.mine_only = !app.mine_only;
            app.pr_selected = 0;
        }
        KeyCode::Enter | KeyCode::Char('o') => app.open_selected_pr(),
        KeyCode::Char('r') => app.refresh_prs(),
        _ => {}
    }
}

fn handle_add_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_add(),
        KeyCode::Tab | KeyCode::Down => app.add_focus_next(),
        KeyCode::Up => {
            app.add_focus = if app.add_focus == 0 { 1 } else { 0 };
        }
        KeyCode::Enter => {
            if let Err(e) = app.do_add() {
                app.status_msg = Some(e);
            }
        }
        KeyCode::Backspace => {
            if app.add_focus == 0 {
                app.add_url.pop();
            } else {
                app.add_name.pop();
            }
        }
        KeyCode::Char(c) => {
            if app.add_focus == 0 {
                app.add_url.push(c);
            } else {
                app.add_name.push(c);
            }
        }
        _ => {}
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &App) {
    match app.view {
        View::List | View::Add => {
            render_list(f, app);
            if matches!(app.view, View::Add) {
                render_add_popup(f, app);
            }
        }
        View::Prs => render_pr_popup(f, app),
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
            Line::from(Span::styled(
                "  No repos yet.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("  a  ", Style::default().fg(Color::Cyan)),
                Span::styled("add repo", Style::default().fg(Color::DarkGray)),
            ]),
        ];
        f.render_widget(Paragraph::new(hint), chunks[0]);
    } else {
        let items: Vec<ListItem> = app.workspace.config.repos.iter().enumerate().map(|(i, repo)| {
            let bare_path = crate::git::bare_repo_path(&bare_dir, &repo.name);
            let cloned = bare_path.exists();
            let status = if cloned {
                Span::styled("✓ ", Style::default().fg(Color::Green))
            } else {
                Span::styled("✗ ", Style::default().fg(Color::Red))
            };
            let name_style = Style::default().add_modifier(Modifier::BOLD);
            let name_line = Line::from(vec![
                status,
                Span::styled(
                    truncate(&repo.name, max_w.saturating_sub(2)).to_string(),
                    name_style,
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
                Line::from(Span::styled(
                    "  not cloned",
                    Style::default().fg(Color::DarkGray),
                ))
            };

            // Show cached PR count if available.
            let pr_hint = match app.pr_cache.get(&repo.name) {
                Some(PrState::Loaded(prs)) if !prs.is_empty() => {
                    let n = prs.len();
                    Line::from(Span::styled(
                        format!("  {} open PR{}", n, if n == 1 { "" } else { "s" }),
                        Style::default().fg(if i == app.selected { Color::Yellow } else { Color::DarkGray }),
                    ))
                }
                Some(PrState::Loaded(_)) => Line::from(""),
                _ => Line::from(""),
            };

            ListItem::new(vec![name_line, url_line, commit_line, pr_hint, Line::from("")])
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
        (
            " a add · Enter PRs · r refresh · j/k · q quit".into(),
            Style::default().fg(Color::DarkGray),
        )
    };
    f.render_widget(Paragraph::new(help).style(style), chunks[1]);
}

fn render_pr_popup(f: &mut Frame, app: &App) {
    let area = f.area();

    let repo_name = app
        .workspace
        .config
        .repos
        .get(app.selected)
        .map(|r| r.name.as_str())
        .unwrap_or("?");

    let mine_label = if app.mine_only { " [mine]" } else { "" };
    let title = format!(" PRs — {repo_name}{mine_label} ");

    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(&block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);

    let repo_name_owned = repo_name.to_string();
    match app.pr_cache.get(&repo_name_owned) {
        None | Some(PrState::Loading) => {
            f.render_widget(
                Paragraph::new("  Loading…").style(Style::default().fg(Color::DarkGray)),
                chunks[0],
            );
        }
        Some(PrState::Error(e)) => {
            let msg = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  error: {e}"),
                    Style::default().fg(Color::Red),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Is gh installed and authenticated?",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            f.render_widget(Paragraph::new(msg), chunks[0]);
        }
        Some(PrState::Loaded(all_prs)) => {
            let prs = app.visible_prs();
            if prs.is_empty() {
                let msg = if app.mine_only {
                    "  No open PRs from you.  Press m to show all."
                } else {
                    "  No open PRs."
                };
                f.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
                    chunks[0],
                );
            } else {
                let width = chunks[0].width as usize;
                let items: Vec<ListItem> = prs.iter().map(|pr| {
                    let draft = if pr.is_draft {
                        Span::styled("[draft] ", Style::default().fg(Color::DarkGray))
                    } else {
                        Span::raw("")
                    };
                    let num = Span::styled(
                        format!("#{} ", pr.number),
                        Style::default().fg(Color::Cyan),
                    );
                    let is_mine = app.github_user.as_deref() == Some(pr.author.as_str());
                    let author_style = if is_mine {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let author = Span::styled(
                        format!(" {}", truncate(&pr.author, 12)),
                        author_style,
                    );
                    let date = Span::styled(
                        format!(" {}", crate::github::pr_age(&pr.created_at)),
                        Style::default().fg(Color::DarkGray),
                    );
                    // Title gets whatever space is left.
                    let suffix_len = author.content.len() + date.content.len() + 2;
                    let num_len = num.content.len();
                    let draft_len = draft.content.len();
                    let title_max = width
                        .saturating_sub(suffix_len + num_len + draft_len + 2);
                    let title = Span::raw(truncate(&pr.title, title_max).to_string());

                    ListItem::new(Line::from(vec![draft, num, title, author, date]))
                }).collect();

                let mut state = ListState::default();
                state.select(Some(app.pr_selected));

                let list = List::new(items)
                    .highlight_style(Style::default().add_modifier(Modifier::BOLD))
                    .highlight_symbol("▶ ");
                f.render_stateful_widget(list, chunks[0], &mut state);
            }

            // Show total count when mine filter is active.
            if app.mine_only && !all_prs.is_empty() {
                let total = all_prs.len();
                let visible = prs.len();
                let note = format!(" showing {visible}/{total} PRs");
                f.render_widget(
                    Paragraph::new(note).style(Style::default().fg(Color::DarkGray)),
                    chunks[1],
                );
                return;
            }
        }
    }

    f.render_widget(
        Paragraph::new(" j/k · Enter open · m mine · r refresh · Esc back")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

fn render_add_popup(f: &mut Frame, app: &App) {
    let area = f.area();
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
    let url_style = if app.add_focus == 0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(format!("  [{}]", app.add_url)).style(url_style),
        chunks[2],
    );

    f.render_widget(
        Paragraph::new("  Name: (optional, inferred from URL)"),
        chunks[4],
    );
    let name_style = if app.add_focus == 1 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(format!("  [{}]", app.add_name)).style(name_style),
        chunks[5],
    );

    let help = if let Some(ref msg) = app.status_msg {
        Paragraph::new(format!("  {}", msg)).style(Style::default().fg(Color::Red))
    } else {
        Paragraph::new("  Tab · Enter add · Esc cancel")
            .style(Style::default().fg(Color::DarkGray))
    };
    f.render_widget(help, chunks[7]);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

fn short_url(url: &str) -> String {
    let s = url.trim_end_matches(".git");
    if let Some(r) = s.strip_prefix("git@") {
        return r.to_string();
    }
    if let Some(r) = s.strip_prefix("https://") {
        return r.to_string();
    }
    if let Some(r) = s.strip_prefix("http://") {
        return r.to_string();
    }
    s.to_string()
}

fn infer_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string()
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
