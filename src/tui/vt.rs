//! The VT emulator behind the embedded terminal: `alacritty_terminal`, and
//! the painting of its grid into a ratatui buffer.
//!
//! It replaced `vt100` for one thing that crate has no room for: OSC 8
//! hyperlinks. A link is part of a cell here, so it scrolls, gets erased and
//! is overwritten along with the text it sits on; `paint` hands it on to
//! [`super::hyperlink`], which gets it to the real terminal.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as VtColor, NamedColor, Processor};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::time::Instant;

pub struct Vt {
    term: Term<VoidListener>,
    parser: Processor,
}

impl Vt {
    pub fn new(rows: u16, cols: u16) -> Self {
        // No scrollback: tmux keeps the history, this is only the screen.
        let config = Config { scrolling_history: 0, ..Config::default() };
        Vt { term: Term::new(config, &term_size(rows, cols), VoidListener), parser: Processor::new() }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.term.resize(term_size(rows, cols));
    }

    /// `(rows, cols)`; the demo sizes its fixture screens by it.
    #[cfg(test)]
    pub fn size(&self) -> (u16, u16) {
        let grid = self.term.grid();
        (u16::try_from(grid.screen_lines()).unwrap_or(u16::MAX), u16::try_from(grid.columns()).unwrap_or(u16::MAX))
    }

    pub fn app_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn mouse_reporting(&self) -> bool {
        self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Paint the screen into `area`, cursor left to the caller: where it
    /// belongs (absolute cell) if shown.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        // A synchronized update (mode 2026) holds output back until it ends;
        // one that never ends is released after its timeout, which the
        // parser only notices when asked.
        if self.parser.sync_timeout().sync_timeout().is_some_and(|t| t <= Instant::now()) {
            self.parser.stop_sync(&mut self.term);
        }
        paint(&self.term, area, buf);
        if !self.term.mode().contains(TermMode::SHOW_CURSOR) {
            return None;
        }
        let Point { line, column } = self.term.grid().cursor.point;
        let (row, col) = (u16::try_from(line.0).ok()?, u16::try_from(column.0).ok()?);
        (row < area.height && col < area.width).then_some((area.x + col, area.y + row))
    }
}

fn term_size(rows: u16, cols: u16) -> TermSize {
    TermSize::new(usize::from(cols.max(2)), usize::from(rows.max(1)))
}

fn paint(term: &Term<VoidListener>, area: Rect, buf: &mut Buffer) {
    let grid = term.grid();
    let rows = area.height.min(u16::try_from(grid.screen_lines()).unwrap_or(u16::MAX));
    let cols = area.width.min(u16::try_from(grid.columns()).unwrap_or(u16::MAX));
    for row in 0..area.height {
        for col in 0..area.width {
            buf[(area.x + col, area.y + row)].reset();
        }
    }
    for row in 0..rows {
        let line = &grid[Line(i32::from(row))];
        for col in 0..cols {
            let cell = &line[Column(usize::from(col))];
            let out = &mut buf[(area.x + col, area.y + row)];
            let mut style = Style::reset().fg(color(cell.fg)).bg(color(cell.bg));
            for (flag, modifier) in [
                (Flags::BOLD, Modifier::BOLD),
                (Flags::DIM, Modifier::DIM),
                (Flags::ITALIC, Modifier::ITALIC),
                (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
                (Flags::INVERSE, Modifier::REVERSED),
                (Flags::HIDDEN, Modifier::HIDDEN),
                (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
            ] {
                if cell.flags.intersects(flag) {
                    style = style.add_modifier(modifier);
                }
            }
            out.set_style(style);
            // The right half of a wide character: its left half's symbol
            // covers it, and ratatui skips it when drawing.
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let mut symbol = String::from(cell.c);
            if let Some(zw) = cell.zerowidth() {
                symbol.extend(zw);
            }
            if let Some(link) = cell.hyperlink() {
                super::hyperlink::mark(&mut symbol, link.uri());
            }
            out.set_symbol(&symbol);
        }
    }
}

/// The 16 named colours go out as indexed ones, as `vt100` sent them, so the
/// outer terminal's palette decides what they look like.
fn color(c: VtColor) -> Color {
    match c {
        VtColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => Color::Indexed(i),
        VtColor::Named(n) => match n as usize {
            i @ 0..16 => Color::Indexed(i as u8),
            _ => match n {
                NamedColor::DimBlack => Color::Indexed(0),
                NamedColor::DimRed => Color::Indexed(1),
                NamedColor::DimGreen => Color::Indexed(2),
                NamedColor::DimYellow => Color::Indexed(3),
                NamedColor::DimBlue => Color::Indexed(4),
                NamedColor::DimMagenta => Color::Indexed(5),
                NamedColor::DimCyan => Color::Indexed(6),
                NamedColor::DimWhite => Color::Indexed(7),
                _ => Color::Reset,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(bytes: &[u8]) -> Buffer {
        let mut vt = Vt::new(3, 20);
        vt.process(bytes);
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        vt.render(area, &mut buf);
        buf
    }

    #[test]
    fn paints_text_colours_and_attributes() {
        let buf = screen(b"a\x1b[1;31mb\x1b[0;38;2;1;2;3mc");
        assert_eq!(buf[(0, 0)].symbol(), "a");
        assert_eq!(buf[(0, 0)].fg, Color::Reset);
        assert_eq!(buf[(1, 0)].fg, Color::Indexed(1));
        assert!(buf[(1, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(2, 0)].fg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let buf = screen("日x".as_bytes());
        assert_eq!(buf[(0, 0)].symbol(), "日");
        assert_eq!(buf[(2, 0)].symbol(), "x");
    }

    #[test]
    fn cells_under_an_osc8_link_carry_it() {
        let buf = screen(b"\x1b]8;;https://example.com\x1b\\ab\x1b]8;;\x1b\\c");
        let link = |x| crate::tui::hyperlink::split(buf[(x, 0)].symbol());
        assert_eq!(link(0), ("a", Some("https://example.com".to_string())));
        assert_eq!(link(1), ("b", Some("https://example.com".to_string())));
        assert_eq!(link(2), ("c", None));
    }

    #[test]
    fn modes_follow_the_program() {
        let mut vt = Vt::new(3, 20);
        assert!(!vt.app_cursor() && !vt.mouse_reporting() && !vt.bracketed_paste());
        vt.process(b"\x1b[?1h\x1b[?1000h\x1b[?1006h\x1b[?2004h");
        assert!(vt.app_cursor() && vt.mouse_reporting() && vt.bracketed_paste());
    }
}
