//! Opening a task's links from the column: `L` lists every link of the
//! selected task in a box over the list (the picker). The list's rows carry
//! no link chips; the sidebar lists the shown task's links.
//!
//! What a link *is* — where it was found, its name, its order — is
//! `tenx_core::links`; this is the keys and the drawing. Opening or copying
//! one is the front end's job ([`LinkAction`]): the terminal client opens the
//! browser of the machine it runs on (copies over SSH), and tenx web opens it
//! in the page, which needs the tap or key that asked for it.

use super::{Column, Focus, Mode, truncate};
use crate::palette;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use tenx_core::links::{Link, LinkKind};
use unicode_width::UnicodeWidthStr;

/// What the column asks its front end to do with a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkAction {
    Open(Link),
    Copy(Link),
}

/// `L`: one task's links, and the one under the cursor.
pub(super) struct LinksPicker {
    /// The task's row id (`view::row_id`) and title.
    pub(super) id: String,
    pub(super) title: String,
    pub(super) links: Vec<Link>,
    pub(super) selected: usize,
}

/// A link kind's colour in the picker.
pub(super) fn kind_rgb(kind: LinkKind) -> &'static palette::Rgb {
    match kind {
        LinkKind::Pr => &palette::SUCCESS,
        LinkKind::Artifact | LinkKind::Design => &palette::ACCENT,
        LinkKind::Linear => &palette::CURRENT,
        LinkKind::Slack => &palette::WARN,
        LinkKind::Obsidian => &palette::ACCENT,
        LinkKind::Port => &palette::INFO,
        LinkKind::Web => &palette::MUTED,
    }
}

pub(super) const LINKS_HINT: &str = "⏎ 1-9 open · y copy · esc";

impl Column {
    /// `L`: the selected task's links.
    pub(super) fn start_links(&mut self) {
        let Some(row) = self.selected_row() else { return };
        let links = row.live.all_links();
        if links.is_empty() {
            self.status_msg = Some("no links for this task yet".into());
            return;
        }
        let (id, title) = (super::view::row_id(row), row.title.clone());
        self.status_msg = None;
        self.mode = Mode::Links(LinksPicker { id, title, links, selected: 0 });
    }

    pub(super) fn handle_links_key(&mut self, key: KeyEvent) {
        let Mode::Links(picker) = &mut self.mode else { return };
        let n = picker.links.len();
        let pick = |i: usize| picker.links.get(i).cloned();
        let action = match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                picker.selected = (picker.selected + 1).min(n.saturating_sub(1));
                return;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                return;
            }
            KeyCode::Enter | KeyCode::Char('o') | KeyCode::Char('l') => pick(picker.selected).map(LinkAction::Open),
            KeyCode::Char('y') => pick(picker.selected).map(LinkAction::Copy),
            KeyCode::Char(c @ '1'..='9') => pick(c as usize - '1' as usize).map(LinkAction::Open),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('L') => None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => None,
            _ => return,
        };
        self.finish_links(action);
    }

    /// Back to the list, with the action (if any) for the front end.
    fn finish_links(&mut self, action: Option<LinkAction>) {
        self.mode = Mode::List;
        self.focus = Focus::List;
        self.link_action = action;
    }

    /// The link the last key asked to open or copy.
    pub(crate) fn take_link_action(&mut self) -> Option<LinkAction> {
        self.link_action.take()
    }
}

/// The picker: a box over the bottom of the list, one line per link —
/// its number, icon, name and kind — then the keys.
pub(super) fn render_links_box(f: &mut ratatui::Frame, picker: &LinksPicker, list_area: Rect) {
    if list_area.height < 4 || list_area.width < 12 {
        return;
    }
    let height = (picker.links.len() as u16 + 3).min(list_area.height);
    let area = Rect { x: list_area.x, y: list_area.y + list_area.height - height, width: list_area.width, height };
    let inner_w = area.width.saturating_sub(2) as usize;
    let visible = (height - 3) as usize;
    let first = picker.selected.saturating_sub(visible.saturating_sub(1));
    let dim = Style::default().fg(palette::MUTED.color());
    let mut lines: Vec<Line> = Vec::new();
    for (i, link) in picker.links.iter().enumerate().skip(first).take(visible) {
        let on = i == picker.selected;
        let bg = if on { palette::SEL_BG.color() } else { palette::GROUND.color() };
        let key = if i < 9 { format!("{} ", i + 1) } else { "  ".to_string() };
        let kind = link.kind.name();
        // number + icon + space, the name, a space, the kind on the right
        let label_w = inner_w.saturating_sub(4 + 1 + kind.width());
        let label = truncate(&link.label, label_w);
        let pad = label_w.saturating_sub(label.width());
        let title = Style::default().fg(if on { palette::SEL_TEXT.color() } else { palette::TEXT.color() }).bg(bg);
        lines.push(Line::from(vec![
            Span::styled(key, dim.bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} ", link.kind.icon()), Style::default().fg(kind_rgb(link.kind).color()).bg(bg)),
            Span::styled(label, if on { title.add_modifier(Modifier::BOLD) } else { title }),
            Span::styled(format!("{} {kind}", " ".repeat(pad)), dim.bg(bg)),
        ]));
    }
    lines.push(Line::from(Span::styled(LINKS_HINT, dim)));
    let title = truncate(&format!(" links · {} ", picker.title), inner_w);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette::GROUND.color())).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette::ACCENT.color()))
                .title(Span::styled(title, Style::default().fg(palette::ACCENT.color()))),
        ),
        area,
    );
}
