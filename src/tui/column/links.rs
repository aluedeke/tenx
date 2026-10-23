//! Opening a task's links from the column: `L` lists every link of the
//! selected task in a box over the list (the picker), `f` puts a letter on
//! every link chip in the list (hints), like Vimium in a browser.
//!
//! What a link *is* — where it was found, its name, its order — is
//! `tenx_core::links`; this is the keys and the drawing. Opening or copying
//! one is the front end's job ([`LinkAction`]): the terminal client opens the
//! browser of the machine it runs on (copies over SSH), and tenx web opens it
//! in the page, which needs the tap or key that asked for it.

use super::{Column, Focus, Mode, Tab, pr_rgb, truncate};
use crate::palette;
use crate::snapshot::Row;
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

/// One chip on a row's second line that is about a link: a PR, the `◇` of
/// what the agent made with Claude, a port. `link` is what it opens; a wait
/// on a PR the lookup hasn't found has none.
pub(super) struct LinkChip {
    pub(super) text: String,
    pub(super) rgb: &'static palette::Rgb,
    pub(super) link: Option<Link>,
}

/// The link chips of `row`, in the order they are drawn.
pub(super) fn link_chips(row: &Row) -> Vec<LinkChip> {
    let mut out = Vec::new();
    for pr in &row.live.prs {
        out.push(LinkChip {
            text: row.live.chip(pr),
            rgb: pr_rgb(&pr.checks),
            link: (!pr.url.is_empty()).then(|| Link { url: pr.url.clone(), label: pr.label(), kind: LinkKind::Pr }),
        });
    }
    for text in row.live.watch_only_chips() {
        out.push(LinkChip { text, rgb: pr_rgb(""), link: None });
    }
    // One chip for everything made with Claude, opening the first of them;
    // `L` lists the rest.
    let all = row.live.all_links();
    let mut claude = all.iter().filter(|l| l.kind.is_claude());
    if let Some(first) = claude.next() {
        let more = claude.count();
        let icon = first.kind.icon();
        out.push(LinkChip {
            text: if more > 0 { format!("{icon}{}", more + 1) } else { icon.to_string() },
            rgb: &palette::ACCENT,
            link: Some(first.clone()),
        });
    }
    for p in &row.live.ports {
        out.push(LinkChip {
            text: format!(":{p}"),
            rgb: &palette::MUTED,
            link: Some(Link { url: format!("http://localhost:{p}"), label: format!("localhost:{p}"), kind: LinkKind::Port }),
        });
    }
    out
}

/// A link kind's colour in the picker.
pub(super) fn kind_rgb(kind: LinkKind) -> &'static palette::Rgb {
    match kind {
        LinkKind::Pr => &palette::SUCCESS,
        LinkKind::Artifact | LinkKind::Design => &palette::ACCENT,
        LinkKind::Linear => &palette::CURRENT,
        LinkKind::Port => &palette::INFO,
        LinkKind::Web => &palette::MUTED,
    }
}

pub(super) const LINKS_HINT: &str = "⏎ 1-9 open · y copy · esc";
pub(super) const HINTS_HINT: &str = "a-z open · A-Z copy · esc";

impl Column {
    /// The chips `f` puts a letter on, in list order: the row's index in
    /// `rows`, the chip's place among its `link_chips`, and the link. At most
    /// 26 — one letter each.
    pub(super) fn hint_targets(&self) -> Vec<(usize, usize, Link)> {
        if self.tab != Tab::Tasks {
            return Vec::new();
        }
        let mut out = Vec::new();
        for &i in &self.filtered {
            for (k, chip) in link_chips(&self.rows[i]).into_iter().enumerate() {
                if let Some(link) = chip.link
                    && out.len() < 26
                {
                    out.push((i, k, link));
                }
            }
        }
        out
    }

    /// In `f` mode, each lettered chip as (row index, chip index, letter);
    /// otherwise none.
    pub(super) fn hint_letters(&self) -> Vec<(usize, usize, char)> {
        if !matches!(self.mode, Mode::Hints) {
            return Vec::new();
        }
        self.hint_targets()
            .into_iter()
            .enumerate()
            .filter_map(|(i, (r, c, _))| Some((r, c, tenx_core::links::hint_key(i)?)))
            .collect()
    }

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

    /// `f`: a letter on every link chip.
    pub(super) fn start_hints(&mut self) {
        if self.hint_targets().is_empty() {
            self.status_msg = Some("no links in the list".into());
            return;
        }
        self.status_msg = None;
        self.mode = Mode::Hints;
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

    pub(super) fn handle_hints_key(&mut self, key: KeyEvent) {
        let action = match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let lower = c.to_ascii_lowercase();
                tenx_core::links::hint_index(lower)
                    .and_then(|i| self.hint_targets().into_iter().nth(i))
                    .map(|(_, _, link)| if c.is_ascii_uppercase() { LinkAction::Copy(link) } else { LinkAction::Open(link) })
            }
            _ => None,
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
