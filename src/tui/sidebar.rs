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

fn icon_rgb(kind: Option<LinkKind>, tone: Option<Tone>) -> &'static palette::Rgb {
    if let Some(t) = tone {
        return tone_rgb(t);
    }
    match kind {
        Some(LinkKind::Artifact | LinkKind::Design) => &palette::ACCENT,
        Some(LinkKind::Linear) => &palette::CURRENT,
        Some(LinkKind::Slack) => &palette::WARN,
        Some(LinkKind::Obsidian) => &palette::ACCENT,
        Some(LinkKind::Port) => &palette::INFO,
        Some(LinkKind::Pr) => &palette::SUCCESS,
        Some(LinkKind::Web) | None => &palette::MUTED,
    }
}

fn fg(rgb: &palette::Rgb) -> Style {
    Style::default().fg(rgb.color())
}

/// An entry's state under its label: the check squares, then each part in
/// its colour, wrapped at part boundaries to `width`.
fn state_lines(e: &tenx_core::sidebar::Entry, width: usize) -> Vec<Line<'static>> {
    let mut pieces: Vec<(Vec<Span<'static>>, usize)> = Vec::new();
    if !e.checks.is_empty() {
        let squares = e.checks.iter().map(|t| Span::styled("■", fg(tone_rgb(*t)))).collect();
        pieces.push((squares, e.checks.len()));
    }
    for p in &e.parts {
        let span = if p.pill {
            let bg = match p.tone {
                Tone::Warn => &palette::CHIP_INPUT_BG,
                Tone::Merged => &palette::CHIP_SECRETS_BG,
                _ => &palette::SEL_BG,
            };
            Span::styled(format!(" {} ", p.text), fg(tone_rgb(p.tone)).bg(bg.color()).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(p.text.clone(), fg(tone_rgb(p.tone)))
        };
        let w = span.content.chars().count();
        pieces.push((vec![span], w));
    }
    let mut lines = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (spans, w) in pieces {
        if !cur.is_empty() && used + 1 + w > width {
            lines.push(Line::from(std::mem::take(&mut cur)));
            used = 0;
        }
        if cur.is_empty() {
            cur.push(Span::raw("  "));
            used = 2;
        } else {
            cur.push(Span::raw(" "));
            used += 1;
        }
        cur.extend(spans);
        used += w;
    }
    if !cur.is_empty() {
        lines.push(Line::from(cur));
    }
    lines
}

/// The sidebar's lines for a body `width` wide, each with the link a click
/// on it opens.
fn lines(view: Option<&SidebarView>, width: usize) -> Vec<(Line<'static>, Option<Link>)> {
    let Some(v) = view else {
        return vec![(Line::from(Span::styled("no task open", fg(&palette::MUTED))), None)];
    };
    let mut out = Vec::new();
    if v.sidebar.sections.is_empty() {
        out.push((Line::from(Span::styled("nothing linked yet", fg(&palette::MUTED))), None));
        out.push((Line::from(Span::styled("links the agent names and TASK.md's", fg(&palette::IDLE))), None));
        out.push((Line::from(Span::styled("## Links show here", fg(&palette::IDLE))), None));
        return out;
    }
    for (i, section) in v.sidebar.sections.iter().enumerate() {
        if i > 0 {
            out.push((Line::from(""), None));
        }
        let mut head = vec![Span::styled(section.title.to_uppercase(), fg(&palette::MUTED).add_modifier(Modifier::BOLD))];
        if !section.count.is_empty() {
            head.push(Span::styled(format!(" {}", section.count), fg(&palette::TEXT)));
        }
        out.push((Line::from(head), None));
        if let Some(done) = section.progress {
            let filled = ((done.clamp(0.0, 1.0)) * width as f32).round() as usize;
            out.push((
                Line::from(vec![
                    Span::styled("━".repeat(filled), fg(&palette::SUCCESS)),
                    Span::styled("━".repeat(width.saturating_sub(filled)), fg(&palette::BORDER)),
                ]),
                None,
            ));
        }
        for e in &section.entries {
            let link = e.url.clone().map(|url| Link { url, label: e.label.clone(), kind: e.kind.unwrap_or(LinkKind::Web) });
            out.push((
                Line::from(vec![
                    Span::styled(format!("{} ", e.icon), fg(icon_rgb(e.kind, e.icon_tone))),
                    Span::styled(e.label.clone(), fg(&palette::TEXT)),
                ]),
                link.clone(),
            ));
            for l in state_lines(e, width) {
                out.push((l, link.clone()));
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
    let all = lines(view, body.width as usize);
    let shown: Vec<_> = all.into_iter().skip(scroll as usize).take(body.height as usize).collect();
    let hits = shown.iter().enumerate().filter_map(|(i, (_, l))| l.clone().map(|l| (body.y + i as u16, l))).collect();
    f.render_widget(Paragraph::new(shown.into_iter().map(|(l, _)| l).collect::<Vec<_>>()), body);
    hits
}

/// How far the sidebar `area` can scroll.
pub(super) fn max_scroll(view: Option<&SidebarView>, area: Rect) -> u16 {
    (lines(view, area.width.saturating_sub(3) as usize).len() as u16).saturating_sub(area.height)
}
