//! The client — what `tenx` opens: the task list as a column beside the
//! tmux session, in one process that owns the terminal — the layout cmux
//! made familiar, done as a TUI outside tmux.
//!
//! The right-hand side is a tmux client running in a pty
//! (`term::EmbeddedTerminal`), so tmux stays the session layer untouched:
//! windows are tasks, the watcher, sweep and secrets all work as before,
//! and the session survives this client. The left-hand side is the column
//! on its `Surface::Client`: the same list, keys and commands, but the
//! window switch *is* the jump — the terminal shows it — and the selection
//! survives switching because nothing restarts. One client per terminal
//! (desktop, phone over SSH), each with its own list state *and its own
//! current task*: the pty attaches through a grouped tmux session of this
//! client's own (`tmux::client_session`), which shares the windows with
//! every other client but not the choice of which one is on screen. The
//! phone can sit on one task while the desktop works in another.
//!
//! Keys: Ctrl+w shows the column and focuses it, or hides it from inside;
//! everything else goes to whichever side has focus. On a narrow terminal
//! (a phone) the column is either the whole screen or not there at all:
//! hidden by default, Ctrl+w shows the list over the whole screen, and a
//! jump or `:hide` puts it away again. A terminal that changes width across
//! the threshold (a phone rotating, or reporting its real size a moment
//! after connecting) is re-laid out the same way.

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
        EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent,
    },
    execute,
    style::Print,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};
use std::io;
use std::time::{Duration, Instant};

use super::column::{self, ClientRequest, Column};
use super::term::{EmbeddedTerminal, TaskScreen};

/// How often the column's rows refresh.
const REFRESH: Duration = Duration::from_millis(500);
/// Frame pacing: the terminal side changes on its own, so redraw this often
/// even without input.
const FRAME: Duration = Duration::from_millis(33);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Focus {
    Terminal,
    Column,
}

pub(super) struct Client {
    pub(super) column: Column,
    term: Box<dyn TaskScreen>,
    pub(super) focus: Focus,
    column_shown: bool,
    column_width: u16,
    /// A terminal too narrow for a column: the list takes the whole screen
    /// while shown.
    narrow: bool,
    size: (u16, u16),
    last_refresh: Instant,
    quit: bool,
}

impl Client {
    /// A client over `term` for a `cols`×`rows` terminal: the column shown
    /// (unless the terminal is too narrow for one) with the keyboard in the
    /// task. `run` spawns the pty for `term`; the demo hands in a script.
    pub(super) fn new(column: Column, mut term: Box<dyn TaskScreen>, cols: u16, rows: u16, configured_width: u16) -> Client {
        let narrow = cols < crate::tmux::SMALL_CLIENT_COLS as u16;
        let column_width = tenx_core::column::width(cols, configured_width).min(cols / 2);
        let mut c = Client {
            column,
            term: Box::new(Idle),
            focus: Focus::Terminal,
            column_shown: !narrow,
            column_width,
            narrow,
            size: (cols, rows),
            last_refresh: Instant::now(),
            quit: false,
        };
        let (r, w) = c.term_size();
        term.resize(r, w);
        c.term = term;
        c
    }

    /// The column's and the terminal's areas for the current state.
    fn layout(&self, full: Rect) -> (Option<Rect>, Rect) {
        if !self.column_shown {
            return (None, full);
        }
        if self.narrow {
            return (Some(full), full);
        }
        let w = self.column_width.min(full.width / 2);
        let column = Rect { x: full.x, y: full.y, width: w, height: full.height };
        let term = Rect { x: full.x + w, y: full.y, width: full.width - w, height: full.height };
        (Some(column), term)
    }

    /// The pty's size for the current state — the terminal keeps the whole
    /// screen on a narrow client, where the column only covers it.
    fn term_size(&self) -> (u16, u16) {
        let full = Rect::new(0, 0, self.size.0, self.size.1);
        let (_, term) = self.layout(full);
        (term.height.max(1), term.width.max(1))
    }

    fn apply_size(&mut self, cols: u16, rows: u16) {
        self.size = (cols, rows);
        // Width alone decides: a column costs no rows, and a short wide
        // terminal (a laptop at 28 rows) still wants the column beside
        // the task, with tmux's status line under the task, not the whole
        // window.
        let was_narrow = self.narrow;
        self.narrow = cols < crate::tmux::SMALL_CLIENT_COLS as u16;
        self.column_width = tenx_core::column::width(cols, configured_width());
        // Crossing the threshold changes what "shown" means. Narrowing with
        // the keyboard in the task must not leave the list painted over a
        // task that is still taking the keys: the task keeps the screen and
        // the column goes, as it would have started. Widening brings the
        // column back beside the task, focus untouched.
        match (was_narrow, self.narrow) {
            (false, true) if self.column_shown && self.focus == Focus::Terminal => self.column_shown = false,
            (true, false) => self.column_shown = true,
            _ => {}
        }
        let (r, c) = self.term_size();
        self.term.resize(r, c);
    }

    fn show_column(&mut self) {
        self.column_shown = true;
        self.focus_column();
        let (r, c) = self.term_size();
        self.term.resize(r, c);
    }

    /// Keyboard into the task; the column shows no cursor meanwhile.
    fn focus_terminal(&mut self) {
        self.focus = Focus::Terminal;
        self.column.blur();
    }

    /// Keyboard into the column, cursor on the task you are in.
    fn focus_column(&mut self) {
        self.focus = Focus::Column;
        self.column.select_current();
    }

    fn hide_column(&mut self) {
        self.column_shown = false;
        self.focus_terminal();
        let (r, c) = self.term_size();
        self.term.resize(r, c);
    }

    /// Ctrl+w: from the terminal, bring up the column (focused); from the
    /// column, put it away.
    fn cycle(&mut self) {
        match (self.column_shown, self.focus) {
            (true, Focus::Column) => self.hide_column(),
            (true, Focus::Terminal) => self.focus_column(),
            (false, _) => self.show_column(),
        }
    }

    fn handle_request(&mut self, req: ClientRequest) {
        match req {
            ClientRequest::FocusTerminal => {
                self.focus_terminal();
                if self.narrow {
                    self.hide_column();
                }
            }
            ClientRequest::Hide => self.hide_column(),
            ClientRequest::Quit => self.quit = true,
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.cycle();
            self.trace("key Ctrl+w");
            return Ok(());
        }
        match self.focus {
            Focus::Terminal => {
                if let Some(bytes) = self.term.key_bytes(&key) {
                    self.term.write(&bytes);
                }
            }
            Focus::Column => {
                self.column.handle_key(key)?;
                if let Some(req) = self.column.take_request() {
                    self.handle_request(req);
                }
                self.trace(&format!("key {:?} {:?}", key.modifiers, key.code));
            }
        }
        Ok(())
    }

    fn handle_mouse(&mut self, m: MouseEvent) -> Result<()> {
        let full = Rect::new(0, 0, self.size.0, self.size.1);
        let (column, term) = self.layout(full);
        // Only a click moves the keyboard; the pointer resting or scrolling
        // over the column must not.
        let click = matches!(m.kind, event::MouseEventKind::Down(_));
        if let Some(c) = column
            && super::mouse::hit(c, m.column, m.row)
        {
            if click {
                self.focus = Focus::Column;
            }
            self.column.handle_mouse(m)?;
            if let Some(req) = self.column.take_request() {
                self.handle_request(req);
            }
            self.trace(&format!("mouse {:?} in column", m.kind));
            return Ok(());
        }
        if super::mouse::hit(term, m.column, m.row) {
            if click {
                self.focus_terminal();
            }
            if let Some(bytes) = self.term.mouse_bytes(&m, m.column - term.x, m.row - term.y) {
                self.term.write(&bytes);
            }
        }
        Ok(())
    }

    /// Append a line to `$TENX_CLIENT_LOG` (when set): what the client did
    /// and where the column stands afterwards. For chasing input trouble in
    /// a real session, where a headless harness can't follow.
    fn trace(&self, what: &str) {
        let Some(path) = std::env::var_os("TENX_CLIENT_LOG") else { return };
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(
                f,
                "{} focus={} {what} | {}",
                chrono_stamp(),
                if self.focus == Focus::Column { "column" } else { "terminal" },
                self.column.trace_state()
            );
        }
    }

    fn tick(&mut self) {
        if self.last_refresh.elapsed() >= REFRESH {
            self.last_refresh = Instant::now();
            if self.column.in_list_mode() {
                self.column.refresh_statuses();
                // A status change moves its task to the right section at
                // once; the selection follows its task, so this is safe
                // under a moving cursor too.
                if self.column.sections_stale() {
                    self.column.tidy();
                }
            }
        }
    }

    pub(super) fn draw(&mut self, f: &mut ratatui::Frame) {
        let full = f.area();
        let (column, term) = self.layout(full);
        // Moving onto a closed task shows an empty screen in its place; the
        // session keeps running behind it and returns the moment focus does.
        let closed = if self.focus == Focus::Column { self.column.selected_closed() } else { None };
        let cursor = match closed {
            Some(title) => {
                f.render_widget(ratatui::widgets::Clear, term);
                render_closed(f, term, &title);
                None
            }
            None => self.term.render(term, f.buffer_mut()),
        };
        if let Some(c) = column {
            // The column paints its ground but keeps whatever symbols are
            // there; on a narrow client it covers the terminal, so wipe first.
            f.render_widget(ratatui::widgets::Clear, c);
            column::render_in(f, &mut self.column, c);
        }
        // The column sets the cursor for its search field when it draws;
        // the terminal's wins only while it has focus.
        if self.focus == Focus::Terminal
            && let Some((x, y)) = cursor
            && (column.is_none() || !self.narrow)
        {
            f.set_cursor_position((x, y));
        }
    }
}

/// A placeholder task side while `Client::new` sizes the real one.
struct Idle;

impl TaskScreen for Idle {
    fn render(&self, _: Rect, _: &mut ratatui::buffer::Buffer) -> Option<(u16, u16)> {
        None
    }
    fn resize(&mut self, _: u16, _: u16) {}
    fn write(&mut self, _: &[u8]) {}
    fn key_bytes(&self, _: &KeyEvent) -> Option<Vec<u8>> {
        None
    }
    fn mouse_bytes(&self, _: &MouseEvent, _: u16, _: u16) -> Option<Vec<u8>> {
        None
    }
    fn paste(&mut self, _: &str) {}
    fn alive(&self) -> bool {
        false
    }
    fn take_bell(&self) -> bool {
        false
    }
    fn take_clipboard(&self) -> Vec<String> {
        Vec::new()
    }
    fn kill(&mut self) {}
}

/// The task area while the column rests on a task with no window: its
/// title, and the one thing to do.
fn render_closed(f: &mut ratatui::Frame, area: Rect, title: &str) {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Paragraph};
    let p = &crate::palette::GROUND;
    f.render_widget(Block::default().style(Style::default().bg(p.color())), area);
    if area.height < 3 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(title.to_string(), Style::default().fg(crate::palette::BRIGHT.color()).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("no window open", Style::default().fg(crate::palette::MUTED.color()))),
        Line::from(""),
        Line::from(vec![
            Span::styled("⏎", Style::default().fg(crate::palette::ACCENT.color()).add_modifier(Modifier::BOLD)),
            Span::styled(" open it here", Style::default().fg(crate::palette::TEXT.color())),
        ]),
    ];
    let y = area.y + area.height / 2 - 2;
    let block = Rect { x: area.x, y, width: area.width, height: 4.min(area.height) };
    f.render_widget(Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center), block);
}

/// The user's `column_width` from the global config (0 = automatic).
fn configured_width() -> u16 {
    crate::workspace::load_global().map(|g| g.column_width).unwrap_or(0)
}

/// Seconds since the epoch with millis — enough to line a trace up with
/// what you saw.
fn chrono_stamp() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:03}", d.as_secs() % 100_000, d.subsec_millis())
}

pub fn run() -> Result<()> {
    crate::tmux::ensure_session()?;
    // This client's own session (see the module doc): every "current
    // window" question and switch from here on is about it, and the pty
    // creates it on attach.
    let session = crate::tmux::client_session(std::process::id());
    crate::tmux::set_view_session(&session);
    // A fresh grouped session starts on the group's first window, the home
    // shell — but the column is the list; landing there would show the list
    // twice. Start on a task window when there is one.
    let start_on = crate::tmux::list_windows()
        .ok()
        .and_then(|ws| ws.into_iter().find(|w| w.name != crate::tmux::HOME_WINDOW))
        .map(|w| w.id);

    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange, DisableBracketedPaste);
        orig(info);
    }));
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableFocusChange, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_client(&mut terminal, &session, start_on.as_deref());

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange, DisableBracketedPaste)?;
    terminal.show_cursor()?;
    result
}

fn run_client(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, session: &str, start_on: Option<&str>) -> Result<()> {
    let (cols, rows) = crossterm::terminal::size().context("terminal size")?;

    // The inner tmux must not think it is nested: `$TMUX` is this client's
    // secret, not its child's. `TERM` passes through so colours match.
    let (tmux, args) = crate::tmux::attach_command(session, start_on);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let term = EmbeddedTerminal::spawn(&tmux.to_string_lossy(), &args, &[], &["TMUX", "TMUX_PANE"], rows.max(1), cols.max(1))?;
    let mut client = Client::new(Column::new(), Box::new(term), cols, rows, configured_width());

    loop {
        terminal.draw(|f| client.draw(f))?;
        if !client.term.alive() {
            break;
        }
        if event::poll(FRAME)? {
            match event::read()? {
                Event::Key(key) => client.handle_key(key)?,
                Event::Mouse(m) => client.handle_mouse(m)?,
                Event::Paste(text) => {
                    if client.focus == Focus::Terminal {
                        client.term.paste(&text);
                    }
                }
                Event::Resize(c, r) => client.apply_size(c, r),
                // No rebuild on focus: the column tidies itself on its own
                // clock (`tick`), so a click elsewhere changes nothing. Coming
                // back is when idle windows get swept, rate-limited.
                Event::FocusGained => client.column.maybe_sweep(),
                Event::FocusLost => {}
            }
        }
        client.tick();
        if client.term.take_bell() {
            let _ = execute!(io::stdout(), Print("\x07"));
        }
        for payload in client.term.take_clipboard() {
            let _ = execute!(io::stdout(), Print(format!("\x1b]52;{payload}\x07")));
        }
        if let Some((ws_idx, slug)) = client.column.take_unlock() {
            column::run_unlock(terminal, &mut client.column, ws_idx, &slug)?;
        }
        if client.quit {
            client.term.kill();
            break;
        }
    }
    Ok(())
}
