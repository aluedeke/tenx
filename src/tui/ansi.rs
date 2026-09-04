//! SGR escape sequences → ratatui spans, for the pane preview.
//!
//! `tmux capture-pane -e` reproduces a pane's colours as the same `ESC[…m`
//! sequences the programs in it wrote. This is the small subset that matters
//! for a Claude Code pane: reset, bold/dim/italic/underline and their
//! offs, the 16 base colours, 256-colour and truecolour foregrounds and
//! backgrounds. Anything else (cursor movement, other CSI finals, OSC) is
//! dropped; unknown SGR parameters are ignored. Deliberately hand-rolled
//! rather than a dependency: forty lines, and the palette mapping (a pane's
//! default colours become the overlay's ground and text) is ours.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// One captured line → styled spans. Style carries across the whole line
/// only; each line starts from the default (tmux re-emits attributes at the
/// start of a line when capturing with `-e`).
pub fn line(raw: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut text = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !c.is_control() {
                text.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                let mut fin = None;
                for c in chars.by_ref() {
                    if c.is_ascii_digit() || c == ';' || c == ':' {
                        params.push(c);
                    } else {
                        fin = Some(c);
                        break;
                    }
                }
                if fin == Some('m') {
                    if !text.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut text), style));
                    }
                    style = apply_sgr(style, &params);
                }
            }
            Some(']') => {
                // OSC: skip to BEL or ST.
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\x07' || (prev == '\x1b' && c == '\\') {
                        break;
                    }
                    prev = c;
                }
            }
            _ => {}
        }
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

fn apply_sgr(mut style: Style, params: &str) -> Style {
    let nums: Vec<u16> = params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect();
    let nums = if nums.is_empty() { vec![0] } else { nums };
    let mut i = 0;
    while i < nums.len() {
        match nums[i] {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style = style.fg(base(nums[i] - 30)),
            90..=97 => style = style.fg(base(nums[i] - 90 + 8)),
            40..=47 => style = style.bg(base(nums[i] - 40)),
            100..=107 => style = style.bg(base(nums[i] - 100 + 8)),
            39 => style = style.fg(Color::Reset),
            49 => style = style.bg(Color::Reset),
            38 | 48 => {
                let (color, used) = extended(&nums[i + 1..]);
                if let Some(color) = color {
                    style = if nums[i] == 38 { style.fg(color) } else { style.bg(color) };
                }
                i += used;
            }
            _ => {}
        }
        i += 1;
    }
    style
}

/// `5;n` or `2;r;g;b` after a 38/48 — returns the colour and how many
/// parameters it consumed.
fn extended(rest: &[u16]) -> (Option<Color>, usize) {
    match rest {
        [5, n, ..] => (Some(Color::Indexed(*n as u8)), 2),
        [2, r, g, b, ..] => (Some(Color::Rgb(*r as u8, *g as u8, *b as u8)), 4),
        _ => (None, 0),
    }
}

fn base(n: u16) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn strips_and_styles() {
        let l = line("\x1b[1m\x1b[38;5;208m ❯ 1. Yes\x1b[0m\x1b[K rest");
        assert_eq!(plain(&l), " ❯ 1. Yes rest");
        assert_eq!(l.spans[0].style.fg, Some(Color::Indexed(208)));
        assert!(l.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(l.spans[1].style, Style::default());
    }

    #[test]
    fn truecolour_and_osc() {
        let l = line("\x1b]0;title\x07\x1b[38;2;10;20;30mx\x1b[39my");
        assert_eq!(plain(&l), "xy");
        assert_eq!(l.spans[0].style.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(l.spans[1].style.fg, Some(Color::Reset));
    }

    #[test]
    fn plain_text_passes_through() {
        let l = line("no escapes here");
        assert_eq!(l.spans.len(), 1);
        assert_eq!(plain(&l), "no escapes here");
        assert!(line("").spans.is_empty());
    }
}
