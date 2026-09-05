//! An embedded terminal: a command running in a pty, its output parsed by a
//! VT emulator into a screen the client paints into part of its own frame.
//! This is how `tenx client` shows the tmux session beside its own task
//! list — the same trick arta and Tattoy use.
//!
//! The emulator (`vt100`, via `tui-term`'s widget) is the one thing here
//! that could be swapped: every other piece — the pty, the reader thread,
//! key and mouse encoding, the scanner that forwards bells and OSC 52
//! clipboard writes to the real terminal — is emulator-agnostic.

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct EmbeddedTerminal {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    alive: Arc<AtomicBool>,
    /// Set by the reader when the program rang the bell; the client forwards
    /// it to the real terminal once, from the drawing thread.
    bell: Arc<AtomicBool>,
    /// OSC 52 payloads (`Pc;Pd`) seen in the output, for the client to
    /// re-emit to the real terminal so copies land in the local clipboard
    /// even over SSH.
    clipboard: Arc<Mutex<Vec<String>>>,
}

impl EmbeddedTerminal {
    /// Run `program args…` in a pty of `rows`×`cols`. `env` overrides are
    /// applied on top of this process's environment; `unset` are removed.
    pub fn spawn(program: &str, args: &[&str], env: &[(&str, &str)], unset: &[&str], rows: u16, cols: u16) -> Result<Self> {
        let pty = native_pty_system();
        let pair = pty.openpty(size(rows, cols)).context("open pty")?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        for k in unset {
            cmd.env_remove(k);
        }
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        let child = pair.slave.spawn_command(cmd).context("spawn in pty")?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer = pair.master.take_writer().context("pty writer")?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let alive = Arc::new(AtomicBool::new(true));
        let bell = Arc::new(AtomicBool::new(false));
        let clipboard = Arc::new(Mutex::new(Vec::new()));
        {
            let (parser, alive, bell, clipboard) = (parser.clone(), alive.clone(), bell.clone(), clipboard.clone());
            std::thread::spawn(move || reader_loop(reader, parser, alive, bell, clipboard));
        }
        Ok(EmbeddedTerminal { parser, writer, master: pair.master, child, alive, bell, clipboard })
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let _ = self.master.resize(size(rows, cols));
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    /// Paint the screen into `area`. Returns where the real terminal's
    /// cursor belongs (absolute cell), if the program shows one.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        let parser = self.parser.lock().ok()?;
        let screen = parser.screen();
        let mut cursor = tui_term::widget::Cursor::default();
        cursor.hide();
        ratatui::widgets::Widget::render(tui_term::widget::PseudoTerminal::new(screen).cursor(cursor), area, buf);
        if screen.hide_cursor() {
            return None;
        }
        let (row, col) = screen.cursor_position();
        (row < area.height && col < area.width).then_some((area.x + col, area.y + row))
    }

    /// Encode a key for the program, honouring its cursor-key mode.
    pub fn key_bytes(&self, key: &KeyEvent) -> Option<Vec<u8>> {
        let app_cursor = self.parser.lock().map(|p| p.screen().application_cursor()).unwrap_or(false);
        encode_key(key, app_cursor)
    }

    /// Encode a mouse event at `(col, row)` relative to the terminal's area
    /// as SGR (1006) — what tmux asks for — or `None` if the program has not
    /// asked for mouse reports.
    pub fn mouse_bytes(&self, m: &MouseEvent, col: u16, row: u16) -> Option<Vec<u8>> {
        let mode = self.parser.lock().map(|p| p.screen().mouse_protocol_mode()).ok()?;
        if mode == vt100::MouseProtocolMode::None {
            return None;
        }
        encode_mouse(m, col, row)
    }

    pub fn paste(&mut self, text: &str) {
        let bracketed = self.parser.lock().map(|p| p.screen().bracketed_paste()).unwrap_or(false);
        if bracketed {
            self.write(b"\x1b[200~");
            self.write(text.as_bytes());
            self.write(b"\x1b[201~");
        } else {
            self.write(text.as_bytes());
        }
    }

    /// Whether the program rang the bell since the last call.
    pub fn take_bell(&self) -> bool {
        self.bell.swap(false, Ordering::SeqCst)
    }

    pub fn take_clipboard(&self) -> Vec<String> {
        self.clipboard.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    alive: Arc<AtomicBool>,
    bell: Arc<AtomicBool>,
    clipboard: Arc<Mutex<Vec<String>>>,
) {
    let mut buf = [0u8; 8192];
    let mut scanner = Scanner::default();
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => {
                alive.store(false, Ordering::SeqCst);
                return;
            }
            Ok(n) => {
                let seen = scanner.feed(&buf[..n]);
                if seen.bell {
                    bell.store(true, Ordering::SeqCst);
                }
                if !seen.clipboard.is_empty()
                    && let Ok(mut c) = clipboard.lock()
                {
                    c.extend(seen.clipboard);
                }
                if let Ok(mut p) = parser.lock() {
                    p.process(&buf[..n]);
                }
            }
        }
    }
}

// ── Output scanner ────────────────────────────────────────────────────────────

/// Picks two things out of the raw output that the emulator swallows: a
/// real BEL (not one terminating an OSC string) and OSC 52 clipboard writes.
/// Robust to sequences split across reads.
#[derive(Default)]
struct Scanner {
    state: Scan,
    osc: Vec<u8>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Scan {
    #[default]
    Ground,
    Esc,
    Osc,
    OscEsc,
    /// DCS/SOS/PM/APC: skipped until ST.
    Str,
    StrEsc,
}

#[derive(Default)]
struct Seen {
    bell: bool,
    clipboard: Vec<String>,
}

const OSC_CAP: usize = 1 << 20;

impl Scanner {
    fn feed(&mut self, data: &[u8]) -> Seen {
        let mut seen = Seen::default();
        for &b in data {
            self.state = match (self.state, b) {
                (Scan::Ground, 0x07) => {
                    seen.bell = true;
                    Scan::Ground
                }
                (Scan::Ground, 0x1b) => Scan::Esc,
                (Scan::Ground, _) => Scan::Ground,
                (Scan::Esc, b']') => {
                    self.osc.clear();
                    Scan::Osc
                }
                (Scan::Esc, b'P' | b'X' | b'^' | b'_') => Scan::Str,
                (Scan::Esc, 0x1b) => Scan::Esc,
                (Scan::Esc, _) => Scan::Ground,
                (Scan::Osc, 0x07) => {
                    self.finish(&mut seen);
                    Scan::Ground
                }
                (Scan::Osc, 0x1b) => Scan::OscEsc,
                (Scan::Osc, b) => {
                    if self.osc.len() < OSC_CAP {
                        self.osc.push(b);
                    }
                    Scan::Osc
                }
                (Scan::OscEsc, b'\\') => {
                    self.finish(&mut seen);
                    Scan::Ground
                }
                (Scan::OscEsc, _) => Scan::Ground,
                (Scan::Str, 0x1b) => Scan::StrEsc,
                (Scan::Str, _) => Scan::Str,
                (Scan::StrEsc, b'\\') => Scan::Ground,
                (Scan::StrEsc, _) => Scan::Str,
            };
        }
        seen
    }

    fn finish(&mut self, seen: &mut Seen) {
        if let Some(rest) = self.osc.strip_prefix(b"52;")
            && !rest.ends_with(b";?")
        {
            seen.clipboard.push(String::from_utf8_lossy(rest).into_owned());
        }
        self.osc.clear();
    }
}

// ── Input encoding ────────────────────────────────────────────────────────────

/// xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier(m: KeyModifiers) -> u8 {
    1 + m.contains(KeyModifiers::SHIFT) as u8
        + 2 * m.contains(KeyModifiers::ALT) as u8
        + 4 * m.contains(KeyModifiers::CONTROL) as u8
}

/// A key as the bytes a terminal would send. `app_cursor`: the program set
/// DECCKM (nvim, less…), so plain arrows are `ESC O x` not `ESC [ x`.
pub fn encode_key(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let mods = key.modifiers;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let modified = mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    let seq = |code: &str| -> Vec<u8> {
        // `ESC [ 1 ; m code` when modified, else the plain (or application) form.
        if modified {
            format!("\x1b[1;{}{code}", modifier(mods)).into_bytes()
        } else if app_cursor {
            format!("\x1bO{code}").into_bytes()
        } else {
            format!("\x1b[{code}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modified { format!("\x1b[{n};{}~", modifier(mods)).into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    Some(match key.code {
        KeyCode::Char(c) if ctrl => {
            let base = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' => 0,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' | '/' => 0x1f,
                _ => return None,
            };
            if alt { vec![0x1b, base] } else { vec![base] }
        }
        KeyCode::Char(c) => {
            let mut out = Vec::with_capacity(5);
            if alt {
                out.push(0x1b);
            }
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            out
        }
        KeyCode::Enter => {
            if alt { vec![0x1b, b'\r'] } else { vec![b'\r'] }
        }
        KeyCode::Backspace => {
            if alt { vec![0x1b, 0x7f] } else { vec![0x7f] }
        }
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => seq("A"),
        KeyCode::Down => seq("B"),
        KeyCode::Right => seq("C"),
        KeyCode::Left => seq("D"),
        KeyCode::Home => seq("H"),
        KeyCode::End => seq("F"),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let code = ["P", "Q", "R", "S"][n as usize - 1];
            if modified { format!("\x1b[1;{}{code}", modifier(mods)).into_bytes() } else { format!("\x1bO{code}").into_bytes() }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        _ => return None,
    })
}

/// An SGR mouse report (`ESC [ < b ; x ; y M/m`), 1-based.
pub fn encode_mouse(m: &MouseEvent, col: u16, row: u16) -> Option<Vec<u8>> {
    let (button, release) = match m.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => (button_code(b), true),
        MouseEventKind::Drag(b) => (button_code(b) + 32, false),
        MouseEventKind::Moved => (35, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    let mut b = button;
    if m.modifiers.contains(KeyModifiers::SHIFT) {
        b += 4;
    }
    if m.modifiers.contains(KeyModifiers::ALT) {
        b += 8;
    }
    if m.modifiers.contains(KeyModifiers::CONTROL) {
        b += 16;
    }
    Some(format!("\x1b[<{b};{};{}{}", col + 1, row + 1, if release { 'm' } else { 'M' }).into_bytes())
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn keys_encode_like_xterm() {
        let none = KeyModifiers::NONE;
        assert_eq!(encode_key(&k(KeyCode::Char('a'), none), false), Some(b"a".to_vec()));
        assert_eq!(encode_key(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), false), Some(vec![3]));
        assert_eq!(encode_key(&k(KeyCode::Char('b'), KeyModifiers::ALT), false), Some(vec![0x1b, b'b']));
        assert_eq!(encode_key(&k(KeyCode::Up, none), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(&k(KeyCode::Up, none), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode_key(&k(KeyCode::Up, KeyModifiers::CONTROL), true), Some(b"\x1b[1;5A".to_vec()));
        assert_eq!(encode_key(&k(KeyCode::Delete, KeyModifiers::SHIFT), false), Some(b"\x1b[3;2~".to_vec()));
        assert_eq!(encode_key(&k(KeyCode::F(5), none), false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(encode_key(&k(KeyCode::Enter, none), false), Some(b"\r".to_vec()));
    }

    #[test]
    fn mouse_encodes_sgr_one_based() {
        let m = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 0, row: 0, modifiers: KeyModifiers::NONE };
        assert_eq!(encode_mouse(&m, 4, 2), Some(b"\x1b[<0;5;3M".to_vec()));
        let m = MouseEvent { kind: MouseEventKind::ScrollUp, column: 0, row: 0, modifiers: KeyModifiers::NONE };
        assert_eq!(encode_mouse(&m, 0, 0), Some(b"\x1b[<64;1;1M".to_vec()));
    }

    #[test]
    fn scanner_sees_bells_and_clipboard_but_not_osc_terminators() {
        let mut s = Scanner::default();
        let seen = s.feed(b"hi\x07 \x1b]0;title\x07 \x1b]52;c;Zm9v\x1b\\ \x1b]52;c;?\x07");
        assert!(seen.bell);
        assert_eq!(seen.clipboard, vec!["c;Zm9v".to_string()]);
        // Split across reads.
        let mut s = Scanner::default();
        let a = s.feed(b"\x1b]52;c;Zm");
        let b = s.feed(b"9v\x07\x07");
        assert!(a.clipboard.is_empty() && !a.bell);
        assert_eq!(b.clipboard, vec!["c;Zm9v".to_string()]);
        assert!(b.bell);
    }
}
