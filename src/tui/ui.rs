use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use super::app::{App, ConfirmAction, View};
use crate::workspace::format_age;

pub fn render(f: &mut Frame, app: &App) {
    match app.view {
        View::List => render_list(f, app),
        View::Create => render_create(f, app),
    }
}

fn render_list(f: &mut Frame, app: &App) {
    let area = f.area();

    let title = format!(" {} ", app.workspace.config.name);
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Header row
    let header_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(1)])
        .split(inner);

    let header = Line::from(vec![
        Span::styled(format!("{:<20}", "NAME"), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<25}", "REPOS"), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<20}", "BRANCH"), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<6}", "AGE"), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("OPEN", Style::default().add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(header), header_chunks[0]);

    // Task list
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .map(|task| {
            let repos = task.repos.join(", ");
            let age = format_age(task.created_at);
            let open = if app.task_is_open(&task.name) {
                Span::styled("●", Style::default().fg(Color::Green))
            } else {
                Span::raw(" ")
            };
            let line = Line::from(vec![
                Span::raw(format!("{:<20}", task.name)),
                Span::styled(
                    format!("{:<25}", truncate(&repos, 24)),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(
                    format!("{:<20}", truncate(&task.branch, 19)),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(format!("{:<6}", age), Style::default().fg(Color::DarkGray)),
                open,
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut list_state = ListState::default();
    if !app.tasks.is_empty() {
        list_state.select(Some(app.selected));
    }

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, header_chunks[1], &mut list_state);

    // Help / status line
    let help = if let Some(ConfirmAction::Delete(ref name)) = app.confirm {
        format!(" delete '{}'? [y/N] ", name)
    } else if let Some(ref msg) = app.status_msg {
        format!(" {} ", msg)
    } else {
        " j/k move · Enter open · x close · d delete · n new · q quit ".to_string()
    };

    let style = if app.confirm.is_some() {
        Style::default().fg(Color::Yellow)
    } else if app.status_msg.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    f.render_widget(
        Paragraph::new(help).style(style),
        header_chunks[2],
    );
}

fn render_create(f: &mut Frame, app: &App) {
    let area = f.area();

    // Center a box
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" New Task ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let n_repos = app.create_repos.len();
    let constraints: Vec<Constraint> = std::iter::once(Constraint::Length(1))   // padding
        .chain(std::iter::once(Constraint::Length(1)))   // "Name:" label
        .chain(std::iter::once(Constraint::Length(1)))   // name input
        .chain(std::iter::once(Constraint::Length(1)))   // padding
        .chain(std::iter::once(Constraint::Length(1)))   // "Repos:" label
        .chain(vec![Constraint::Length(1); n_repos])
        .chain(std::iter::once(Constraint::Min(0)))      // spacer
        .chain(std::iter::once(Constraint::Length(1)))   // help line
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Name label
    f.render_widget(Paragraph::new("  Name:"), chunks[1]);

    // Name input
    let name_style = if app.create_focus == 0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let name_text = format!("  [{}]", app.create_name);
    f.render_widget(Paragraph::new(name_text).style(name_style), chunks[2]);

    // Repos label
    f.render_widget(Paragraph::new("  Repos:"), chunks[4]);

    // Repo checkboxes
    for (i, (repo_name, checked)) in app.create_repos.iter().enumerate() {
        let chunk_idx = 5 + i;
        let check = if *checked { "✓" } else { " " };
        let style = if app.create_focus == i + 1 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let line = format!("  [{}] {}", check, repo_name);
        f.render_widget(Paragraph::new(line).style(style), chunks[chunk_idx]);
    }

    // Status / help
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
        s
    } else {
        &s[..max]
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
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
        .split(popup_layout[1])[1]
}
