//! `tenx client`: the task list as a column beside the tmux session, in
//! one process that owns the terminal — the layout cmux made familiar,
//! done as a TUI outside tmux.
//!
//! The right-hand side is `tmux attach` running in a pty
//! (`term::EmbeddedTerminal`), so tmux stays the session layer untouched:
//! windows are tasks, the watcher, sweep and secrets all work as before,
//! and the session survives this client. The left-hand side is the overlay
//! on its `Surface::Client`: the same list, keys and commands, but the
//! window switch *is* the jump — the terminal shows it — and the selection
//! survives switching because nothing restarts. One client per terminal
//! (desktop, phone over SSH), each with its own list state.
//!
//! Keys: Ctrl+w shows the column and focuses it, or hides it from inside;
//! everything else goes to whichever side has focus. On a narrow terminal
//! (a phone) the column is hidden by default and Ctrl+w shows the list over
//! the whole screen instead.

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

use super::overlay::{self, ClientRequest, Overlay};
use super::term::EmbeddedTerminal;
use super::Surface;

/// How often the column's rows refresh (the sidebar's cadence).
const REFRESH: Duration = Duration::from_millis(500);
/// Frame pacing: the terminal side changes on its own, so redraw this often
/// even without input.
const FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Terminal,
    Column,
}

struct Client {
    overlay: Overlay,
    term: EmbeddedTerminal,
    focus: Focus,
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
        self.narrow = cols < crate::tmux::SMALL_CLIENT_COLS as u16;
        self.column_width = tenx_core::sidebar::width(cols, crate::cli::sidebar::configured_width());
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
        self.overlay.blur();
    }

    /// Keyboard into the column, cursor on the task you are in.
    fn focus_column(&mut self) {
        self.focus = Focus::Column;
        self.overlay.select_current();
    }

    fn hide_column(&mut self) {
        self.column_shown = false;
        self.focus_terminal();
        let (r, c) = self.term_size();
        self.term.resize(r, c);
    }

    /// Ctrl+w: from the terminal, bring up the column (focused); from the
    /// column, put it away — the same cycle as the sidebar pane's.
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

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
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
                self.overlay.handle_key(key)?;
                if let Some(req) = self.overlay.take_request() {
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
            self.overlay.handle_mouse(m)?;
            if let Some(req) = self.overlay.take_request() {
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
                self.overlay.trace_state()
            );
        }
    }

    fn tick(&mut self) {
        if self.last_refresh.elapsed() >= REFRESH {
            self.last_refresh = Instant::now();
            if self.overlay.in_list_mode() {
                self.overlay.refresh_statuses();
                // Re-group while the keyboard is in the task, never while
                // it is moving through the column.
                self.overlay.focused = self.focus == Focus::Column;
                if self.focus == Focus::Terminal && self.overlay.sections_stale() {
                    self.overlay.tidy();
                }
            }
        }
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        let full = f.area();
        let (column, term) = self.layout(full);
        // Moving onto a closed task shows an empty screen in its place; the
        // session keeps running behind it and returns the moment focus does.
        let closed = if self.focus == Focus::Column { self.overlay.selected_closed() } else { None };
        let cursor = match closed {
            Some(title) => {
                f.render_widget(ratatui::widgets::Clear, term);
                render_closed(f, term, &title);
                None
            }
            None => self.term.render(term, f.buffer_mut()),
        };
        if let Some(c) = column {
            // The overlay paints its ground but keeps whatever symbols are
            // there; on a narrow client it covers the terminal, so wipe first.
            f.render_widget(ratatui::widgets::Clear, c);
            overlay::render_in(f, &mut self.overlay, c);
        }
        // The overlay sets the cursor for its search field when it draws;
        // the terminal's wins only while it has focus.
        if self.focus == Focus::Terminal
            && let Some((x, y)) = cursor
            && (column.is_none() || !self.narrow)
        {
            f.set_cursor_position((x, y));
        }
    }
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

/// Seconds since the epoch with millis — enough to line a trace up with
/// what you saw.
fn chrono_stamp() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:03}", d.as_secs() % 100_000, d.subsec_millis())
}

pub fn run(tenx_bin: &str) -> Result<()> {
    crate::tmux::ensure_session(tenx_bin)?;
    // The column is the list; landing on the home window would show the
    // list twice. Start on a task window when there is one.
    if let Ok(windows) = crate::tmux::list_windows()
        && windows.iter().any(|w| w.active && w.name == crate::tmux::HOME_WINDOW)
        && let Some(task) = windows.iter().find(|w| w.name != crate::tmux::HOME_WINDOW)
    {
        let _ = crate::tmux::select_window(&task.id);
    }

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

    // While a client draws the column, new windows must not grow a pane
    // sidebar too (`cli::sidebar::wanted`).
    let _ = crate::tmux::set_global_option(crate::tmux::CLIENT_OPTION, "1");
    let result = run_client(&mut terminal, tenx_bin);
    let _ = crate::tmux::unset_global_option(crate::tmux::CLIENT_OPTION);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange, DisableBracketedPaste)?;
    terminal.show_cursor()?;
    result
}

fn run_client(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, _tenx_bin: &str) -> Result<()> {
    let (cols, rows) = crossterm::terminal::size().context("terminal size")?;
    let narrow = cols < crate::tmux::SMALL_CLIENT_COLS as u16;
    let column_width = tenx_core::sidebar::width(cols, crate::cli::sidebar::configured_width()).min(cols / 2);
    let term_cols = if narrow { cols } else { cols - column_width };

    // The inner tmux must not think it is nested: `$TMUX` is this client's
    // secret, not its child's. `TERM` passes through so colours match.
    let (tmux, args) = crate::tmux::attach_command();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let term = EmbeddedTerminal::spawn(&tmux.to_string_lossy(), &args, &[], &["TMUX", "TMUX_PANE"], rows.max(1), term_cols.max(1))?;

    let mut client = Client {
        overlay: Overlay::new(Surface::Client),
        term,
        focus: Focus::Terminal,
        column_shown: !narrow,
        column_width,
        narrow,
        size: (cols, rows),
        last_refresh: Instant::now(),
        quit: false,
    };

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
                // clock (`tick`), so a click elsewhere changes nothing.
                Event::FocusGained | Event::FocusLost => {}
            }
        }
        client.tick();
        if client.term.take_bell() {
            let _ = execute!(io::stdout(), Print("\x07"));
        }
        for payload in client.term.take_clipboard() {
            let _ = execute!(io::stdout(), Print(format!("\x1b]52;{payload}\x07")));
        }
        if let Some((ws_idx, slug)) = client.overlay.take_unlock() {
            overlay::run_unlock(terminal, &mut client.overlay, ws_idx, &slug)?;
        }
        if client.quit {
            client.term.kill();
            break;
        }
    }
    Ok(())
}
