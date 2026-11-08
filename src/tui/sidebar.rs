//! The sidebar the client draws right of the task screen: the task's links
//! grouped with their state, its repos' git state and its todos
//! (`tenx_core::sidebar`, built from the task's cache by
//! `Column::sidebar`). `s` in the column or `:sidebar` shows and hides it; a
//! click on an entry opens its link.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tenx_core::links::{Link, LinkKind};
use tenx_core::sidebar::Tone;

use super::column::view::SidebarView;
use crate::palette;

fn tone_rgb(tone: Tone) -> &'static palette::Rgb {
    match tone {
        Tone::Plain => &palette::MUTED,
        Tone::Good => &palette::SUCCESS,
        Tone::Warn => &palette::WARN,
        Tone::Bad => &palette::DANGER,
        Tone::Merged => &palette::ACCENT,
        Tone::Muted => &palette::IDLE,
    }
}

fn icon_rgb(kind: Option<LinkKind>, tone: Tone) -> &'static palette::Rgb {
    match kind {
        Some(LinkKind::Artifact | LinkKind::Design) => &palette::ACCENT,
        Some(LinkKind::Linear) => &palette::CURRENT,
        Some(LinkKind::Slack) => &palette::WARN,
        Some(LinkKind::Port) => &palette::INFO,
        Some(LinkKind::Web) => &palette::MUTED,
        Some(LinkKind::Pr) | None => tone_rgb(tone),
    }
}

/// The sidebar's lines, each with the link a click on it opens.
fn lines(view: Option<&SidebarView>) -> Vec<(Line<'static>, Option<Link>)> {
    let fg = |rgb: &palette::Rgb| Style::default().fg(rgb.color());
    let Some(v) = view else {
        return vec![(Line::from(Span::styled("no task open", fg(&palette::MUTED))), None)];
    };
    let mut out = vec![
        (Line::from(Span::styled(v.title.clone(), fg(&palette::BRIGHT).add_modifier(Modifier::BOLD))), None),
        (Line::from(Span::styled(v.ws.clone(), fg(palette::workspace_color(&v.ws)))), None),
    ];
    if v.sidebar.sections.is_empty() {
        out.push((Line::from(""), None));
        out.push((Line::from(Span::styled("nothing linked yet", fg(&palette::MUTED))), None));
        out.push((Line::from(Span::styled("links the agent names and TASK.md's", fg(&palette::IDLE))), None));
        out.push((Line::from(Span::styled("## Links show here", fg(&palette::IDLE))), None));
        return out;
    }
    for section in &v.sidebar.sections {
        out.push((Line::from(""), None));
        let mut head = vec![Span::styled(section.title.to_uppercase(), fg(&palette::MUTED).add_modifier(Modifier::BOLD))];
        if !section.count.is_empty() {
            head.push(Span::styled(format!(" {}", section.count), fg(&palette::TEXT)));
        }
        out.push((Line::from(head), None));
        for e in &section.entries {
            let link = e.url.clone().map(|url| Link { url, label: e.label.clone(), kind: e.kind.unwrap_or(LinkKind::Web) });
            out.push((
                Line::from(vec![
                    Span::styled(format!("{} ", e.icon), fg(icon_rgb(e.kind, e.tone))),
                    Span::styled(e.label.clone(), fg(&palette::TEXT)),
                ]),
                link.clone(),
            ));
            if !e.detail.is_empty() {
                out.push((Line::from(Span::styled(format!("  {}", e.detail), fg(tone_rgb(e.tone)))), link));
            }
        }
    }
    out
}

/// Draw the sidebar in `area`, its first `scroll` lines scrolled away.
/// Returns the screen rows a click opens a link on.
pub(super) fn render(f: &mut ratatui::Frame, area: Rect, view: Option<&SidebarView>, scroll: u16) -> Vec<(u16, Link)> {
    f.render_widget(Block::default().style(Style::default().bg(palette::GROUND.color())), area);
    if area.width < 4 || area.height == 0 {
        return Vec::new();
    }
    // A line down its left edge, as between the column and the task.
    let edge = Rect { width: 1, ..area };
    let bar: Vec<Line> = (0..area.height).map(|_| Line::from(Span::styled("│", Style::default().fg(palette::BORDER.color())))).collect();
    f.render_widget(Paragraph::new(bar), edge);
    let body = Rect { x: area.x + 2, y: area.y, width: area.width - 3, height: area.height };
    let all = lines(view);
    let shown: Vec<_> = all.into_iter().skip(scroll as usize).take(body.height as usize).collect();
    let hits = shown.iter().enumerate().filter_map(|(i, (_, l))| l.clone().map(|l| (body.y + i as u16, l))).collect();
    f.render_widget(Paragraph::new(shown.into_iter().map(|(l, _)| l).collect::<Vec<_>>()), body);
    hits
}

/// How far the sidebar can scroll for `height` rows.
pub(super) fn max_scroll(view: Option<&SidebarView>, height: u16) -> u16 {
    (lines(view).len() as u16).saturating_sub(height)
}
