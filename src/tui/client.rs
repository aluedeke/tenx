//! The client — what `tenx` opens: the task list as a column beside the
//! tmux session, in one process that owns the terminal — the layout cmux
//! made familiar, done as a TUI outside tmux.
//!
//! The right-hand side is `tmux attach` running in a pty
//! (`term::EmbeddedTerminal`), so tmux stays the session layer untouched:
//! windows are tasks, the watcher, sweep and secrets all work as before,
//! and the session survives this client. The left-hand side is the column
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
        EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags, MouseEvent,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    style::Print,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};
use std::io;
use std::time::Duration;

use super::column::{self, ClientRequest, Column};
use super::host::{Ticker, Unlock, UnlockStart, Views};
use super::hyperlink::LinkBackend;
use super::term::{EmbeddedTerminal, TaskScreen};

/// The real terminal as the client draws it: links in the embedded screen
/// go out as OSC 8 ([`super::hyperlink`]).
pub(super) type ClientTerminal = Terminal<LinkBackend<CrosstermBackend<io::Stdout>>>;

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
    ticker: Ticker,
    quit: bool,
    unlock: Unlock,
    views: Views,
    /// The task screen's top line as last drawn: in a tabbed window the tab
    /// strip, which a click is matched against (`tenx_core::tabs::tab_at`).
    top_row: String,
    /// The sidebar right of the task is wanted (`s`, `:sidebar`); it shows
    /// when the terminal is wide enough (`tenx_core::sidebar::width`). Hiding
    /// it is remembered (`SIDEBAR_HIDDEN`).
    sidebar_shown: bool,
    sidebar_scroll: u16,
    /// The task whose sidebar `sidebar_scroll` scrolled: another task's
    /// starts at its top.
    sidebar_task: Option<String>,
    /// The sidebar's rows as last drawn that open a link on a click.
    sidebar_hits: Vec<(u16, tenx_core::links::Link)>,
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
            ticker: Ticker::default(),
            quit: false,
            unlock: Unlock::default(),
            views: Views::default(),
            top_row: String::new(),
            sidebar_shown: !sidebar_hidden_path().is_some_and(|p| p.exists()),
            sidebar_scroll: 0,
            sidebar_task: None,
            sidebar_hits: Vec::new(),
        };
        let (r, w) = c.term_size();
        term.resize(r, w);
        c.term = term;
        c
    }

    /// The column's, the terminal's and the sidebar's areas for the current
    /// state.
    fn layout(&self, full: Rect) -> (Option<Rect>, Rect, Option<Rect>) {
        if self.column_shown && self.narrow {
            return (Some(full), full, None);
        }
        let w = if self.column_shown { self.column_width.min(full.width / 2) } else { 0 };
        let side = if self.sidebar_shown && !self.narrow { tenx_core::sidebar::width(full.width, w) } else { 0 };
        let column = (w > 0).then_some(Rect { x: full.x, y: full.y, width: w, height: full.height });
        let term = Rect { x: full.x + w, y: full.y, width: full.width - w - side, height: full.height };
        let sidebar = (side > 0).then_some(Rect { x: term.right(), y: full.y, width: side, height: full.height });
        (column, term, sidebar)
    }

    /// The pty's size for the current state — the terminal keeps the whole
    /// screen on a narrow client, where the column only covers it.
    fn term_size(&self) -> (u16, u16) {
        let full = Rect::new(0, 0, self.size.0, self.size.1);
        let (_, term, _) = self.layout(full);
        (term.height.max(1), term.width.max(1))
    }

    fn apply_size(&mut self, cols: u16, rows: u16) {
        self.size = (cols, rows);
        // Width alone decides: a column costs no rows, and a short wide
        // terminal (a laptop at 28 rows) still wants the column beside
        // the task, with tmux's status line under the task, not the whole
        // window.
        self.narrow = cols < crate::tmux::SMALL_CLIENT_COLS as u16;
        self.column_width = tenx_core::column::width(cols, configured_width());
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

    /// Answer a task's pending secrets in a tmux popup over this client's
    /// own tmux attach (`host::Unlock`), the column staying drawn and live
    /// behind it. The popup is a terminal of its own — none of the modes the
    /// client set on the real one (bracketed paste, mouse, the alternate
    /// screen) reach its prompts. `false` when a popup can't be aimed (no
    /// tmux client found for the embedded terminal), so the caller falls
    /// back to `column::run_unlock`.
    fn start_unlock(&mut self, ws_idx: usize, slug: &str) -> bool {
        match self.unlock.start(&mut self.column, ws_idx, slug, self.term.pid()) {
            UnlockStart::Started(req) => {
                if let Some(req) = req {
                    self.handle_request(req);
                }
                true
            }
            UnlockStart::NoClient => false,
        }
    }

    /// Once the unlock popup has closed: back to the column, with the
    /// outcome in the footer.
    fn poll_unlock(&mut self) {
        if !self.unlock.poll(&mut self.column) {
            return;
        }
        if !self.column_shown {
            self.show_column();
        } else {
            self.focus_column();
        }
    }

    /// Put a view in front of you (`host::Views`).
    /// `L` or `f` picked a link: open it in this machine's browser — over
    /// SSH there is none of yours here, so copy it to your terminal's
    /// clipboard instead (OSC 52, which the terminal on your side honours).
    fn link(&mut self, action: column::LinkAction) {
        let over_ssh = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        let msg = match action {
            column::LinkAction::Open(l) if !over_ssh => {
                let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                let spawned = std::process::Command::new(opener)
                    .arg(&l.url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                match spawned {
                    Ok(mut child) => {
                        // Reaped off the loop: the opener exits at once.
                        std::thread::spawn(move || child.wait());
                        format!("opened {}", l.label)
                    }
                    Err(e) => format!("{opener}: {e}"),
                }
            }
            column::LinkAction::Open(l) => {
                let _ = execute!(io::stdout(), Print(tenx_core::links::osc52(&l.url)));
                format!("copied {} (no browser over SSH)", l.label)
            }
            column::LinkAction::Copy(l) => {
                let _ = execute!(io::stdout(), Print(tenx_core::links::osc52(&l.url)));
                format!("copied {}", l.label)
            }
        };
        self.column.set_status(msg);
    }

    fn start_view(&mut self, v: column::AgentView) {
        if let Some(req) = self.views.start(&mut self.column, v) {
            self.handle_request(req);
        }
    }

    fn poll_claude_view(&mut self) {
        if let Some(req) = self.views.poll(&mut self.column) {
            self.handle_request(req);
        }
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
            ClientRequest::Sidebar => {
                let full = Rect::new(0, 0, self.size.0, self.size.1);
                // Wanted but not on screen: hiding it would change nothing
                // you can see, so say why it isn't there instead.
                let visible = self.layout(full).2.is_some();
                if visible || !self.sidebar_shown {
                    self.sidebar_shown = !visible;
                    if let Some(p) = sidebar_hidden_path() {
                        let _ = if self.sidebar_shown { std::fs::remove_file(&p) } else { std::fs::write(&p, "") };
                    }
                }
                if self.sidebar_shown && self.layout(full).2.is_none() {
                    self.column.set_status("no room for the sidebar: the terminal is too narrow".to_string());
                }
                let (r, c) = self.term_size();
                self.term.resize(r, c);
            }
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.cycle();
            self.trace("key Ctrl+w");
            return Ok(());
        }
        // Ctrl+n (Ctrl+Shift+n: a quick task) is the column's new-task form
        // from the task too: the list comes up with the form in it.
        if self.focus == Focus::Terminal
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('n' | 'N'))
        {
            if self.column_shown {
                self.focus_column();
            } else {
                self.show_column();
            }
        }
        match self.focus {
            // Ctrl+1/2/3: a tabbed task window's tab. Most terminals send no
            // such key, so tmux can't bind it; the same tab's Alt key can.
            Focus::Terminal
                if key.modifiers == KeyModifiers::CONTROL
                    && let KeyCode::Char(c @ '1'..='3') = key.code =>
            {
                self.term.write(&[0x1b, c as u8]);
            }
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
        let (column, term, sidebar) = self.layout(full);
        // Only a click moves the keyboard; the pointer resting or scrolling
        // over the column must not.
        let click = matches!(m.kind, event::MouseEventKind::Down(_));
        if let Some(s) = sidebar
            && super::mouse::hit(s, m.column, m.row)
        {
            match m.kind {
                event::MouseEventKind::Down(event::MouseButton::Left) => {
                    let link = self.sidebar_hits.iter().find(|(y, _)| *y == m.row).map(|(_, l)| l.clone());
                    if let Some(l) = link {
                        self.link(column::LinkAction::Open(l));
                    }
                }
                event::MouseEventKind::ScrollDown => {
                    let max = super::sidebar::max_scroll(self.column.sidebar().as_ref(), s);
                    self.sidebar_scroll = (self.sidebar_scroll + 3).min(max);
                }
                event::MouseEventKind::ScrollUp => self.sidebar_scroll = self.sidebar_scroll.saturating_sub(3),
                _ => {}
            }
            return Ok(());
        }
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
            // tmux sends a click on the tab strip to no binding: switch here.
            if matches!(m.kind, event::MouseEventKind::Down(event::MouseButton::Left))
                && m.row == term.y
                && let Some(tab) = tenx_core::tabs::tab_at(&self.top_row, (m.column - term.x) as usize)
            {
                let session = self.column.session().to_string();
                std::thread::spawn(move || {
                    if let Some(window) = crate::tmux::current_window_id_in(&session) {
                        crate::tmux::tab_command(tab.name(), &window);
                    }
                });
                self.trace(&format!("click on the {} tab", tab.name()));
                return Ok(());
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
        self.ticker.tick(&mut self.column);
    }

    pub(super) fn draw(&mut self, f: &mut ratatui::Frame) {
        let full = f.area();
        let (column, term, sidebar) = self.layout(full);
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
        let buf = f.buffer_mut();
        self.top_row = (term.x..term.right()).map(|x| buf[(x, term.y)].symbol().chars().next().unwrap_or(' ')).collect();
        if let Some(s) = sidebar {
            let view = self.column.sidebar();
            let task = view.as_ref().map(|v| v.id.clone());
            if task != self.sidebar_task {
                self.sidebar_task = task;
                self.sidebar_scroll = 0;
            }
            let max = super::sidebar::max_scroll(view.as_ref(), s);
            self.sidebar_scroll = self.sidebar_scroll.min(max);
            self.sidebar_hits = super::sidebar::render(f, s, view.as_ref(), self.sidebar_scroll);
        } else {
            self.sidebar_hits.clear();
        }
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
        let _ = execute!(io::stderr(), PopKeyboardEnhancementFlags);
        let _ = execute!(io::stderr(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange, DisableBracketedPaste);
        orig(info);
    }));
    enable_raw_mode()?;
    // A legacy terminal sends Shift+Enter as a bare `\r`, indistinguishable
    // from Enter, so agents in the task pane could never get a newline. The
    // kitty keyboard protocol's first level reports the modifier; terminals
    // without it (Terminal.app) keep sending `\r`.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    let terminal_name = std::env::var("TERM_PROGRAM").or_else(|_| std::env::var("TERM")).unwrap_or_default();
    let keyboard = format!("{} {terminal_name}", if enhanced { "kitty" } else { "legacy" });
    let _ = crate::tmux::set_global_option(crate::tmux::KEYBOARD_OPTION, keyboard.trim_end());
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableFocusChange, EnableBracketedPaste)?;
    if enhanced {
        execute!(stdout, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    let backend = LinkBackend::new(CrosstermBackend::new(stdout));
    let mut terminal = Terminal::new(backend)?;

    let result = run_client(&mut terminal);

    disable_raw_mode()?;
    if enhanced {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange, DisableBracketedPaste)?;
    terminal.show_cursor()?;
    result
}

/// A fresh attach once a `tenx tmux restart` has rebuilt the server, or
/// `None` when the attach ended for any other reason (the server is gone for
/// good). Waits while the restart runs, up to two minutes.
fn reattach() -> Result<Option<EmbeddedTerminal>> {
    let start = std::time::Instant::now();
    while crate::cli::tmuxrestart::in_progress() && start.elapsed() < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if crate::cli::tmuxrestart::in_progress() || !crate::tmux::server_running() {
        return Ok(None);
    }
    let (cols, rows) = crossterm::terminal::size().context("terminal size")?;
    let (tmux, args) = crate::tmux::attach_command();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(Some(EmbeddedTerminal::spawn(&tmux.to_string_lossy(), &args, &[], &["TMUX", "TMUX_PANE"], rows.max(1), cols.max(1))?))
}

fn run_client(terminal: &mut ClientTerminal) -> Result<()> {
    let (cols, rows) = crossterm::terminal::size().context("terminal size")?;

    // The inner tmux must not think it is nested: `$TMUX` is this client's
    // secret, not its child's. `TERM` passes through so colours match.
    let (tmux, args) = crate::tmux::attach_command();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let term = EmbeddedTerminal::spawn(&tmux.to_string_lossy(), &args, &[], &["TMUX", "TMUX_PANE"], rows.max(1), cols.max(1))?;
    let mut client = Client::new(Column::new(), Box::new(term), cols, rows, configured_width());

    loop {
        terminal.draw(|f| client.draw(f))?;
        if !client.term.alive() {
            // `tenx tmux restart` ends every attach; wait for the new server
            // and attach to it rather than quitting.
            match reattach()? {
                Some(term) => {
                    client.term = Box::new(term);
                    let (cols, rows) = crossterm::terminal::size().context("terminal size")?;
                    client.apply_size(cols, rows);
                    continue;
                }
                None => break,
            }
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
        // The unlock names its workspace by index into the column's list;
        // serve it before the tick, whose slow refresh may reload that list
        // (a workspace registered meanwhile) and renumber it.
        if let Some((ws_idx, slug)) = client.column.take_unlock()
            && !client.start_unlock(ws_idx, &slug)
        {
            column::run_unlock(terminal, &mut client.column, ws_idx, &slug)?;
        }
        client.poll_unlock();
        if let Some(v) = client.column.take_agent_view() {
            client.start_view(v);
        }
        if let Some(a) = client.column.take_link_action() {
            client.link(a);
        }
        client.poll_claude_view();
        client.tick();
        if client.term.take_bell() {
            let _ = execute!(io::stdout(), Print("\x07"));
        }
        for payload in client.term.take_clipboard() {
            let _ = execute!(io::stdout(), Print(format!("\x1b]52;{payload}\x07")));
        }
        if client.quit {
            client.term.kill();
            break;
        }
    }
    Ok(())
}
/// Present while the sidebar is hidden, so a new client starts as the last
/// one was left.
const SIDEBAR_HIDDEN: &str = "sidebar-hidden";

fn sidebar_hidden_path() -> Option<std::path::PathBuf> {
    Some(crate::workspace::home_dir().ok()?.join(".config").join("tenx").join(SIDEBAR_HIDDEN))
}

