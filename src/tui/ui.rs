use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::app::{App, ConfirmAction, View};
use crate::workspace::format_age;

pub fn render(f: &mut Frame, app: &mut App) {
    match app.view {
        View::List   => render_list(f, app),
        View::Create => {
            render_list(f, app);
            render_create(f, app);
        }
    }
}

fn render_list(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    if app.tasks.is_empty() {
        let guide = vec![
            Line::from(""),
            Line::from(Span::styled("  No tasks yet.", Style::default().fg(Color::DarkGray))),
            Line::from(""),
            Line::from(vec![
                Span::styled("  n      ", Style::default().fg(Color::Cyan)),
                Span::styled("new task", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(vec![
                Span::styled("  Enter  ", Style::default().fg(Color::Cyan)),
                Span::styled("open / switch to tab", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(vec![
                Span::styled("  x      ", Style::default().fg(Color::Cyan)),
                Span::styled("close tab (keep worktree)", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(vec![
                Span::styled("  d      ", Style::default().fg(Color::Cyan)),
                Span::styled("delete task", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(vec![
                Span::styled("  j/k    ", Style::default().fg(Color::Cyan)),
                Span::styled("navigate", Style::default().fg(Color::DarkGray)),
            ]),
        ];
        f.render_widget(Paragraph::new(guide), chunks[1]);
    } else {
        let header = Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<24}", "NAME"),   Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<24}", "BRANCH"), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<6}",  "AGE"),    Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("OPEN", Style::default().add_modifier(Modifier::BOLD)),
        ]);
        f.render_widget(Paragraph::new(header), chunks[0]);

        let items: Vec<ListItem> = app.tasks.iter().map(|task| {
            let age  = format_age(task.created_at);
            let open = if app.task_is_open(&task.display_name) {
                Span::styled("●", Style::default().fg(Color::Green))
            } else {
                Span::raw(" ")
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<24}", truncate(&task.display_name, 23))),
                Span::styled(format!("{:<24}", truncate(&task.branch, 23)), Style::default().fg(Color::Gray)),
                Span::styled(format!("{:<6}", age), Style::default().fg(Color::DarkGray)),
                open,
            ]))
        }).collect();

        app.list_area = chunks[1];
        app.list_state.select(Some(app.selected));

        let list = List::new(items)
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, chunks[1], &mut app.list_state);
    }

    let (help, style) = if let Some(ConfirmAction::Delete(ref slug)) = app.confirm {
        let display = app.tasks.iter()
            .find(|t| t.name == *slug)
            .map(|t| t.display_name.as_str())
            .unwrap_or(slug.as_str());
        (format!(" delete '{}'? [y/N] ", display), Style::default().fg(Color::Yellow))
    } else if let Some(ref msg) = app.status_msg {
        (format!(" {} ", msg), Style::default().fg(Color::Red))
    } else {
        (" j/k · Enter open · x close · d del · n new · q quit".into(),
         Style::default().fg(Color::DarkGray))
    };
    f.render_widget(Paragraph::new(help).style(style), chunks[2]);
}

fn render_create(f: &mut Frame, app: &mut App) {
    let area  = f.area();
    let popup = centered_rect(55, 60, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" New Task ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let n_repos = app.create_repos.len();
    let constraints: Vec<Constraint> = [
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ]
    .into_iter()
    .chain(vec![Constraint::Length(1); n_repos])
    .chain([Constraint::Min(0), Constraint::Length(1)])
    .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Record clickable field areas for the mouse handler.
    app.create_name_area = chunks[2];
    app.create_repo_areas = (0..n_repos).map(|i| chunks[5 + i]).collect();

    f.render_widget(Paragraph::new("  Name:"), chunks[1]);

    let name_style = if app.create_focus == 0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(format!("  [{}]", app.create_name)).style(name_style),
        chunks[2],
    );

    f.render_widget(Paragraph::new("  Repos:"), chunks[4]);

    for (i, (repo_name, checked)) in app.create_repos.iter().enumerate() {
        let check = if *checked { "✓" } else { " " };
        let style = if app.create_focus == i + 1 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        f.render_widget(
            Paragraph::new(format!("  [{}] {}", check, repo_name)).style(style),
            chunks[5 + i],
        );
    }

    let last = chunks.len() - 1;
    let help = if let Some(ref msg) = app.status_msg {
        Paragraph::new(format!("  {}", msg)).style(Style::default().fg(Color::Red))
    } else {
        Paragraph::new("  Tab move · Space toggle · Enter create · Esc cancel")
            .style(Style::default().fg(Color::DarkGray))
    };
    f.render_widget(help, chunks[last]);
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
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
