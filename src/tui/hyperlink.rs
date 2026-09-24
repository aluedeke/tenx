//! OSC 8 hyperlinks from the embedded terminal to the real one.
//!
//! ratatui has no notion of a link: a cell is a symbol and a style, and the
//! buffer diff sizes a cell by its symbol's display width. Putting the escape
//! sequence itself in the symbol, as ratatui's own example does, makes that
//! width the URL's length, and the diff then skips the cells after it. So a
//! linked cell instead carries its URL in characters that have no width at
//! all: U+E0001 LANGUAGE TAG, then the URL spelled in the tag block
//! (U+E0020–U+E007E mirror printable ASCII). A changed link still changes the
//! cell, so the diff redraws it, and [`LinkBackend`] turns the tags back into
//! OSC 8 around the character on the way out. The language tag is the marker
//! because nothing uses it any more; emoji tag sequences (flags) use the
//! others and pass through untouched.

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use std::io::{self, Write};

const MARK: char = '\u{E0001}';
const TAG_BASE: u32 = 0xE0000;

/// Attach `uri` to a cell's symbol. Characters outside printable ASCII are
/// percent-encoded, which also keeps a hostile URL from carrying escapes.
pub fn mark(symbol: &mut String, uri: &str) {
    symbol.push(MARK);
    for b in uri.bytes() {
        if (0x21..0x7f).contains(&b) {
            symbol.extend(char::from_u32(TAG_BASE + u32::from(b)));
        } else {
            for hex in format!("%{b:02X}").bytes() {
                symbol.extend(char::from_u32(TAG_BASE + u32::from(hex)));
            }
        }
    }
}

/// A cell's symbol without its link, and the link.
pub fn split(symbol: &str) -> (&str, Option<String>) {
    match symbol.split_once(MARK) {
        None => (symbol, None),
        Some((text, tags)) => {
            let uri = tags.chars().filter_map(|c| u8::try_from(u32::from(c).wrapping_sub(TAG_BASE)).ok()).map(char::from).collect();
            (text, Some(uri))
        }
    }
}

/// A backend that writes the links [`mark`] put in cells as OSC 8. Everything
/// else goes to the wrapped backend unchanged.
pub struct LinkBackend<B> {
    inner: B,
}

impl<B: Backend + Write> LinkBackend<B> {
    pub fn new(inner: B) -> Self {
        LinkBackend { inner }
    }
}

impl<B: Backend + Write> Backend for LinkBackend<B> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let content: Vec<_> = content.collect();
        if !content.iter().any(|(_, _, c)| c.symbol().contains(MARK)) {
            return self.inner.draw(content.into_iter());
        }
        // Runs of cells sharing a link (or none), each drawn on its own so
        // the link opens before its first cell and closes after its last.
        let mut i = 0;
        while i < content.len() {
            let link = split(content[i].2.symbol()).1;
            let mut j = i + 1;
            while j < content.len() && split(content[j].2.symbol()).1 == link {
                j += 1;
            }
            match &link {
                None => self.inner.draw(content[i..j].iter().copied())?,
                Some(uri) => {
                    let cells: Vec<(u16, u16, Cell)> = content[i..j]
                        .iter()
                        .map(|&(x, y, c)| {
                            let mut c = c.clone();
                            let text = split(c.symbol()).0.to_string();
                            c.set_symbol(&text);
                            (x, y, c)
                        })
                        .collect();
                    write!(self.inner, "\x1b]8;;{uri}\x1b\\")?;
                    self.inner.draw(cells.iter().map(|(x, y, c)| (*x, *y, c)))?;
                    write!(self.inner, "\x1b]8;;\x1b\\")?;
                }
            }
            i = j;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

/// `execute!` on `terminal.backend_mut()` writes straight through.
impl<B: Write> Write for LinkBackend<B> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::CrosstermBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use unicode_width::UnicodeWidthStr;

    #[derive(Clone, Default)]
    struct Sink(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn linked(text: &str, uri: &str) -> String {
        let mut s = text.to_string();
        mark(&mut s, uri);
        s
    }

    #[test]
    fn a_link_round_trips_and_has_no_width() {
        let s = linked("a", "https://example.com/?q=1#x");
        assert_eq!(s.width(), 1, "the diff must still see one cell");
        assert_eq!(split(&s), ("a", Some("https://example.com/?q=1#x".to_string())));
        assert_eq!(split("a"), ("a", None));
    }

    #[test]
    fn controls_and_non_ascii_are_percent_encoded() {
        let s = linked("a", "https://x/\x1b]ü");
        assert_eq!(split(&s).1.as_deref(), Some("https://x/%1B]%C3%BC"));
    }

    #[test]
    fn backend_wraps_linked_runs_in_osc8() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf[(0, 0)].set_symbol("x");
        buf[(1, 0)].set_symbol(&linked("a", "https://e.com"));
        buf[(2, 0)].set_symbol(&linked("b", "https://e.com"));
        buf[(3, 0)].set_symbol("y");
        let sink = Sink::default();
        let mut backend = LinkBackend::new(CrosstermBackend::new(sink.clone()));
        backend.draw(buf.content().iter().enumerate().map(|(i, c)| (i as u16, 0, c))).unwrap();
        let out = String::from_utf8(sink.0.borrow().clone()).unwrap();
        let open = out.find("\x1b]8;;https://e.com\x1b\\").expect("opens");
        let close = out.find("\x1b]8;;\x1b\\").expect("closes");
        assert!(out.find('x').unwrap() < open);
        assert!(open < out.find('a').unwrap() && out.find('b').unwrap() < close);
        assert!(close < out.find('y').unwrap());
        assert!(!out.contains(MARK));
    }
}
