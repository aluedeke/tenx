//! One browser tab: its grouped tmux session (`tenx-web-<id>`), `tmux attach`
//! to it in a pty, and a `Column` following that session — the TUI client's
//! loop (`tui::client::run_client`) with a WebSocket where the screen was.
//!
//! The column and the pty are synchronous (subprocesses, blocking reads), so
//! they live on a thread of their own, the [`Driver`]; the async socket only
//! passes messages. A tab outlives its socket for a grace period
//! (`Tabs::detach`), so a reload or a dropped phone connection comes back to
//! the same window, column state and jobs.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::tui::column::view::{Click, WebKey};
use crate::tui::column::{ClientRequest, Column, LinkAction};
use crate::tui::host::{Ticker, Unlock, UnlockStart, Views};

/// How often the driver looks at the column when no message arrives — the
/// TUI client's frame, so jobs animate and view switches land as promptly.
const FRAME: Duration = Duration::from_millis(33);

/// A message from the page (`docs/web-protocol.md`, "Browser → server").
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PageMsg {
    Key(WebKey),
    Click(Click),
    Action { name: crate::tui::column::view::Action },
    Form(crate::tui::column::view::FormOp),
    /// `width` / `height`: the terminal's size in CSS pixels, so programs
    /// that draw images (Sixel) can size them to the cells; 0 when unknown.
    Resize {
        cols: u16,
        rows: u16,
        #[serde(default)]
        width: u16,
        #[serde(default)]
        height: u16,
    },
    Viewport { cols: u16 },
    Focus { column: bool },
    Visible,
    /// What the page sends next on the terminal is a paste: out of copy-mode
    /// first, where tmux would drop it.
    Paste,
    /// A line for the server's log (`~/.config/tenx/web.log` under the
    /// service): what the page saw where only the device can see it — e.g.
    /// which clipboard types a paste found, or the error it got. Never
    /// content; trimmed to one short line.
    Log { message: String },
}

/// A page message and its `seq`, if the page numbered it: inputs that change
/// the column carry one, and every view says the highest one applied
/// (`ack`), so the page knows which of its predicted moves the view already
/// shows (`web/src/lib/predict.ts`).
pub(super) fn parse_page(text: &str) -> Result<(PageMsg, Option<u64>), serde_json::Error> {
    #[derive(Deserialize)]
    struct Numbered {
        seq: Option<u64>,
    }
    let msg: PageMsg = serde_json::from_str(text)?;
    let seq = serde_json::from_str::<Numbered>(text).ok().and_then(|n| n.seq);
    Ok((msg, seq))
}

/// A view message: the column, and the highest input `seq` it reflects.
fn view_message(view: &str, ack: u64) -> String {
    format!(r#"{{"type":"view","ack":{ack},"view":{view}}}"#)
}

/// A patch message: what changed since the last view or patch on this
/// socket (`tenx_core::viewpatch`), and the ack as in a view.
fn patch_message(patch: &serde_json::Value, ack: u64) -> String {
    let mut msg = serde_json::json!({ "type": "patch", "ack": ack });
    if let (Some(msg), Some(patch)) = (msg.as_object_mut(), patch.as_object()) {
        msg.extend(patch.clone());
    }
    msg.to_string()
}

pub(super) enum Input {
    Page(PageMsg, Option<u64>),
    /// Keyboard input for the terminal.
    Term(Vec<u8>),
    /// A socket (re)attached: send everything afresh. `patch`: the page
    /// takes `patch` messages after the first view.
    Attached { patch: bool },
    /// The grace period ran out: end the tab.
    Close,
}

pub(super) enum Output {
    Text(String),
    Binary(Vec<u8>),
}

/// A tab as the server holds it.
pub(super) struct Tab {
    pub(super) id: String,
    pub(super) input: mpsc::Sender<Input>,
    /// What the driver sends the page. A socket holds the lock for as long
    /// as it is attached — which is also how a second socket asking for the
    /// same id (a duplicated browser tab) is told it can't have it.
    pub(super) output: Arc<tokio::sync::Mutex<UnboundedReceiver<Output>>>,
    /// Bumped on every attach, so a grace timer can tell whether the tab was
    /// picked up again after the socket it was started for went away.
    pub(super) generation: AtomicU64,
    /// The driver has ended (the attach died): never re-attach to it.
    pub(super) dead: Arc<AtomicBool>,
}

/// Every tab this server has, by id.
pub(super) struct Tabs {
    map: Mutex<HashMap<String, Arc<Tab>>>,
    /// One Work tab for every tab: a job started in one browser tab is listed
    /// in all of them, and outlives the tab that started it.
    jobs: crate::tui::Jobs,
    pub(super) grace: Duration,
}

impl Tabs {
    pub(super) fn new(grace: Duration) -> Tabs {
        Tabs { map: Mutex::new(HashMap::new()), jobs: crate::tui::Jobs::default(), grace }
    }

    /// The tab `want` names, if it is still here and free, else a new one;
    /// with the lock on its output held for the socket.
    pub(super) fn attach(
        &self,
        want: Option<&str>,
    ) -> Result<(Arc<Tab>, tokio::sync::OwnedMutexGuard<UnboundedReceiver<Output>>)> {
        if let Some(id) = want.filter(|id| tenx_core::web::valid_session_id(id)) {
            let found = self.map.lock().unwrap().get(id).cloned();
            if let Some(tab) = found
                && !tab.dead.load(Ordering::SeqCst)
                && let Ok(guard) = tab.output.clone().try_lock_owned()
            {
                tab.generation.fetch_add(1, Ordering::SeqCst);
                return Ok((tab, guard));
            }
        }
        let tab = Arc::new(Tab::spawn(&super::token::random_hex(8)?, self.jobs.clone())?);
        let guard = tab.output.clone().try_lock_owned().expect("a new tab's output is free");
        self.map.lock().unwrap().insert(tab.id.clone(), tab.clone());
        Ok((tab, guard))
    }

    /// The socket for `tab` has gone: end the tab once the grace period is
    /// over, unless a socket has attached to it again by then.
    pub(super) fn detach(self: &Arc<Self>, tab: Arc<Tab>) {
        let generation = tab.generation.load(Ordering::SeqCst);
        let tabs = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tabs.grace).await;
            if tab.generation.load(Ordering::SeqCst) == generation && tab.output.try_lock().is_ok() {
                tabs.map.lock().unwrap().remove(&tab.id);
                let _ = tab.input.send(Input::Close);
            }
        });
    }

    /// Every tab, told to end now — the server is stopping. Returned so the
    /// caller can wait for their drivers to finish.
    pub(super) fn close_all(&self) -> Vec<Arc<Tab>> {
        let tabs: Vec<Arc<Tab>> = self.map.lock().unwrap().drain().map(|(_, t)| t).collect();
        for tab in &tabs {
            let _ = tab.input.send(Input::Close);
        }
        tabs
    }
}

impl Tab {
    /// A new tab `id`: its grouped session, and the driver thread that will
    /// attach to it once the page says how big its terminal is.
    fn spawn(id: &str, jobs: crate::tui::Jobs) -> Result<Tab> {
        let session = tenx_core::web::session_name(id);
        if !crate::tmux::has_session(&session) {
            crate::tmux::new_grouped_session(&session).context("create the tab's tmux session")?;
        }
        let (input, input_rx) = mpsc::channel();
        let (out, output) = unbounded_channel();
        let dead = Arc::new(AtomicBool::new(false));
        let driver = Driver {
            column: Column::in_session(&session).with_jobs(jobs),
            session,
            views: Views::default(),
            unlock: Unlock::default(),
            ticker: Ticker::default(),
            pty: None,
            size: None,
            server: None,
            out,
            last_view: None,
            patch: false,
            ack: 0,
            sent_ack: 0,
        };
        {
            let dead = dead.clone();
            std::thread::spawn(move || {
                driver.run(input_rx);
                dead.store(true, Ordering::SeqCst);
            });
        }
        Ok(Tab {
            id: id.to_string(),
            input,
            output: Arc::new(tokio::sync::Mutex::new(output)),
            generation: AtomicU64::new(0),
            dead,
        })
    }
}

/// `tmux attach` running in a pty.
struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    alive: Arc<AtomicBool>,
}

impl Pty {
    fn spawn(session: &str, at: PtySize, out: UnboundedSender<Output>) -> Result<Pty> {
        let pair = native_pty_system().openpty(at).context("open pty")?;
        let (tmux, args) = crate::tmux::attach_command_to(session);
        let mut cmd = CommandBuilder::new(tmux);
        // xterm.js draws Sixel (its image addon): say so, or tmux shows a
        // picture a program draws as a box of `+`. tmux only finds this out
        // by itself from 3.8 on.
        cmd.args(["-T", "sixel"]);
        cmd.args(&args);
        // Not nested, whatever started `tenx web`; and the terminal is
        // xterm.js, whatever this process's own is.
        cmd.env_remove("TMUX");
        cmd.env_remove("TMUX_PANE");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Ok(home) = crate::workspace::home_dir() {
            cmd.cwd(home);
        }
        let child = pair.slave.spawn_command(cmd).context("spawn tmux attach")?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer = pair.master.take_writer().context("pty writer")?;
        let alive = Arc::new(AtomicBool::new(true));
        {
            let alive = alive.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            // Nobody attached: the bytes queue until the grace
                            // period ends or a socket takes them; a new socket
                            // starts with a full redraw anyway.
                            let _ = out.send(Output::Binary(buf[..n].to_vec()));
                        }
                    }
                }
                alive.store(false, Ordering::SeqCst);
            });
        }
        Ok(Pty { master: pair.master, writer, child, alive })
    }

    fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }
}

/// The pty's size; tmux hands the pixels (its cell size) on to the panes,
/// where an image program reads them with TIOCGWINSZ.
fn size(cols: u16, rows: u16, width: u16, height: u16) -> PtySize {
    PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: width, pixel_height: height }
}

/// The tab's thread: everything that touches the column or the pty.
struct Driver {
    session: String,
    column: Column,
    views: Views,
    unlock: Unlock,
    ticker: Ticker,
    /// Spawned on the page's first `resize`, so the attach starts at the
    /// page's size rather than resizing every window to 80×24 first.
    pty: Option<Pty>,
    /// The page's last terminal size, for attaching again after `tenx tmux
    /// restart` ended the attach.
    size: Option<PtySize>,
    /// The pid of the server the attach was made to (`attach_ended`).
    server: Option<String>,
    out: UnboundedSender<Output>,
    /// The last view sent, to send only a changed one — or only what
    /// changed, to a page that takes patches. `None`: send the next whole.
    last_view: Option<serde_json::Value>,
    /// The socket's page applies `patch` messages.
    patch: bool,
    /// The highest input `seq` applied, and the one the last view carried: a
    /// view goes out when either the column or the ack changed, so a page
    /// learns its input was applied even when it changed nothing.
    ack: u64,
    sent_ack: u64,
}

impl Driver {
    fn run(mut self, input: mpsc::Receiver<Input>) {
        'run: loop {
            match input.recv_timeout(FRAME) {
                Ok(first) => {
                    let mut next = Some(first);
                    while let Some(msg) = next {
                        if !self.handle(msg) {
                            break 'run;
                        }
                        next = input.try_recv().ok();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.after_input();
            if self.pty.as_ref().is_some_and(|p| !p.alive.load(Ordering::SeqCst)) && !self.reattach() {
                self.send_json(serde_json::json!({ "type": "error", "message": "the tmux session ended" }));
                break;
            }
        }
        if let Some(mut pty) = self.pty.take() {
            let _ = pty.child.kill();
        }
        let _ = crate::tmux::kill_session(&self.session);
    }

    /// The attach ended: if `tenx tmux restart` ended it — rebuilding now,
    /// or done already and a new server running (`attach_ended`) — wait for
    /// the new server and attach to it again; this tab's grouped session went
    /// with the old one, so it is made again. `false` when the tab should
    /// end. Messages from the page wait meanwhile.
    fn reattach(&mut self) -> bool {
        use tenx_core::web::{attach_ended, AttachEnded};
        let restarting = crate::cli::tmuxrestart::in_progress();
        match attach_ended(restarting, crate::tmux::server_pid().as_deref(), self.server.as_deref()) {
            AttachEnded::End => return false,
            AttachEnded::Reattach => {}
            AttachEnded::Wait => {
                let start = std::time::Instant::now();
                while crate::cli::tmuxrestart::in_progress() && start.elapsed() < Duration::from_secs(120) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                if crate::cli::tmuxrestart::in_progress() || !crate::tmux::server_running() {
                    return false;
                }
            }
        }
        let Some(at) = self.size else { return false };
        if !crate::tmux::has_session(&self.session) && crate::tmux::new_grouped_session(&self.session).is_err() {
            return false;
        }
        if let Some(mut old) = self.pty.take() {
            let _ = old.child.kill();
        }
        match Pty::spawn(&self.session, at, self.out.clone()) {
            Ok(pty) => {
                self.pty = Some(pty);
                self.server = crate::tmux::server_pid();
                true
            }
            Err(_) => false,
        }
    }

    /// One message; `false` ends the tab.
    fn handle(&mut self, input: Input) -> bool {
        match input {
            Input::Page(msg, seq) => {
                self.page(msg);
                if let Some(seq) = seq {
                    self.ack = self.ack.max(seq);
                }
            }
            Input::Term(bytes) => {
                if let Some(pty) = &mut self.pty {
                    let _ = pty.writer.write_all(&bytes);
                    let _ = pty.writer.flush();
                }
            }
            Input::Attached { patch } => {
                self.last_view = None;
                self.patch = patch;
                // A new socket numbers its inputs from 1 again.
                self.ack = 0;
                // A new page starts from an empty terminal; tmux only sends
                // what changes, so ask it for the whole screen.
                if let Some(client) = self.pty.as_ref().and_then(Pty::pid).and_then(crate::tmux::client_by_pid) {
                    let _ = crate::tmux::refresh_client(&client);
                }
            }
            Input::Close => return false,
        }
        true
    }

    fn page(&mut self, msg: PageMsg) {
        match msg {
            PageMsg::Key(key) => {
                if let Err(e) = self.column.handle_web_key(&key) {
                    self.column.set_status(e.to_string());
                }
            }
            PageMsg::Click(click) => self.column.handle_click(&click),
            PageMsg::Form(op) => {
                if let Err(e) = self.column.handle_form(&op) {
                    self.column.set_status(e.to_string());
                }
            }
            PageMsg::Action { name } => {
                if let Err(e) = self.column.handle_action(name) {
                    self.column.set_status(e.to_string());
                }
            }
            PageMsg::Focus { column: true } => self.column.select_current(),
            PageMsg::Focus { column: false } => self.column.blur(),
            PageMsg::Visible => self.column.maybe_sweep(),
            PageMsg::Paste => {
                let _ = crate::tmux::leave_mode_in(&self.session);
            }
            PageMsg::Log { message } => {
                let line: String = message.chars().filter(|c| !c.is_control()).take(300).collect();
                eprintln!("page {}: {line}", self.session);
            }
            PageMsg::Viewport { cols } => {
                let configured = crate::workspace::load_global().map(|g| g.column_width).unwrap_or(0);
                let (column_cols, narrow) =
                    tenx_core::web::layout(cols, configured, crate::tmux::SMALL_CLIENT_COLS as u16);
                self.send_json(serde_json::json!({ "type": "layout", "column_cols": column_cols, "narrow": narrow }));
            }
            PageMsg::Resize { cols, rows, width, height } => match &mut self.pty {
                Some(pty) => {
                    let at = size(cols, rows, width, height);
                    let _ = pty.master.resize(at);
                    self.size = Some(at);
                }
                None => match Pty::spawn(&self.session, size(cols, rows, width, height), self.out.clone()) {
                    Ok(pty) => {
                        self.pty = Some(pty);
                        self.size = Some(size(cols, rows, width, height));
                        self.server = crate::tmux::server_pid();
                    }
                    Err(e) => self.send_json(serde_json::json!({ "type": "error", "message": format!("{e:#}") })),
                },
            },
        }
    }

    /// What the TUI client does after every event: the column's requests,
    /// the work it asked for, its clock — and the view, if it changed.
    fn after_input(&mut self) {
        // The unlock names its workspace by index into the column's list;
        // serve it before the tick, whose slow refresh may renumber it.
        if let Some((ws_idx, slug)) = self.column.take_unlock() {
            let pid = self.pty.as_ref().and_then(Pty::pid);
            match self.unlock.start(&mut self.column, ws_idx, &slug, pid) {
                UnlockStart::Started(req) => self.request(req),
                UnlockStart::NoClient => {
                    self.column.set_status("no tmux client for this tab to open the unlock in — tenx secrets fulfill".into())
                }
            }
        }
        if self.unlock.poll(&mut self.column) {
            self.send_json(serde_json::json!({ "type": "request", "request": "focus_column" }));
        }
        if let Some(v) = self.column.take_agent_view() {
            let req = self.views.start(&mut self.column, v);
            self.request(req);
        }
        let req = self.views.poll(&mut self.column);
        self.request(req);
        self.ticker.tick(&mut self.column);
        let req = self.column.take_request();
        self.request(req);
        // The page opens links itself, inside the key or tap that asked (a
        // browser blocks a window opened later); this is for one that got
        // past it.
        if let Some(a) = self.column.take_link_action() {
            let (action, link) = match a {
                LinkAction::Open(l) => ("open", l),
                LinkAction::Copy(l) => ("copy", l),
            };
            self.send_json(serde_json::json!({ "type": "link", "action": action, "url": link.url, "label": link.label }));
        }

        let Ok(view) = serde_json::to_value(self.column.view()) else { return };
        let patch = match &self.last_view {
            Some(last) if *last == view && self.ack == self.sent_ack => return,
            Some(last) if self.patch => tenx_core::viewpatch::view_patch(last, &view),
            _ => None,
        };
        let whole = || view_message(&view.to_string(), self.ack);
        let text = match patch {
            // One that rewrote most rows can come out longer than the view.
            Some(p) => {
                let (patch, whole) = (patch_message(&p, self.ack), whole());
                if patch.len() < whole.len() { patch } else { whole }
            }
            // Only the ack moved: an empty patch says so.
            None if self.patch && self.last_view.is_some() => patch_message(&serde_json::json!({}), self.ack),
            None => whole(),
        };
        let _ = self.out.send(Output::Text(text));
        self.last_view = Some(view);
        self.sent_ack = self.ack;
    }

    fn request(&mut self, req: Option<ClientRequest>) {
        let Some(req) = req else { return };
        let name = match req {
            ClientRequest::FocusTerminal => "focus_terminal",
            ClientRequest::Hide => "hide",
            ClientRequest::Quit => "quit",
        };
        self.send_json(serde_json::json!({ "type": "request", "request": name }));
    }

    fn send_json(&self, v: serde_json::Value) {
        let _ = self.out.send(Output::Text(v.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_carry_an_optional_seq_and_views_an_ack() {
        let (msg, seq) = parse_page(r#"{"type":"key","key":"j","ctrl":false,"alt":false,"shift":false,"seq":17}"#).unwrap();
        assert!(matches!(msg, PageMsg::Key(k) if k.key == "j"));
        assert_eq!(seq, Some(17));
        let (msg, seq) = parse_page(r#"{"type":"click","kind":"tab","index":1}"#).unwrap();
        assert!(matches!(msg, PageMsg::Click(Click::Tab { index: 1 })));
        assert_eq!(seq, None, "an old page's unnumbered input still parses");
        assert!(parse_page(r#"{"type":"nope"}"#).is_err());
        let v: serde_json::Value = serde_json::from_str(&view_message(r#"{"a":1}"#, 5)).unwrap();
        assert_eq!(v["type"], "view");
        assert_eq!(v["ack"], 5);
        assert_eq!(v["view"]["a"], 1);
    }

    #[test]
    fn a_patch_message_carries_the_ack_and_the_patch_parts() {
        let patch = serde_json::json!({ "set": { "focus": "list" }, "items": [0, 1] });
        let v: serde_json::Value = serde_json::from_str(&patch_message(&patch, 9)).unwrap();
        assert_eq!(v, serde_json::json!({ "type": "patch", "ack": 9, "set": { "focus": "list" }, "items": [0, 1] }));
    }

    #[test]
    fn page_messages_parse() {
        let key: PageMsg = serde_json::from_str(r#"{"type":"key","key":"j","ctrl":false,"alt":false,"shift":false}"#).unwrap();
        assert!(matches!(key, PageMsg::Key(k) if k.key == "j"));
        let click: PageMsg = serde_json::from_str(r#"{"type":"click","kind":"task","id":"acme/fix","sub":"a1"}"#).unwrap();
        assert!(matches!(click, PageMsg::Click(Click::Task { ref id, sub: Some(ref s) }) if id == "acme/fix" && s == "a1"));
        let tab: PageMsg = serde_json::from_str(r#"{"type":"click","kind":"tab","index":1}"#).unwrap();
        assert!(matches!(tab, PageMsg::Click(Click::Tab { index: 1 })));
        let resize: PageMsg = serde_json::from_str(r#"{"type":"resize","cols":120,"rows":40}"#).unwrap();
        assert!(matches!(resize, PageMsg::Resize { cols: 120, rows: 40, width: 0, height: 0 }));
        let pixels: PageMsg = serde_json::from_str(r#"{"type":"resize","cols":120,"rows":40,"width":1000,"height":700}"#).unwrap();
        assert!(matches!(pixels, PageMsg::Resize { width: 1000, height: 700, .. }));
        assert!(matches!(serde_json::from_str(r#"{"type":"visible"}"#).unwrap(), PageMsg::Visible));
        assert!(matches!(serde_json::from_str(r#"{"type":"paste"}"#).unwrap(), PageMsg::Paste));
        let set: PageMsg = serde_json::from_str(r#"{"type":"form","op":"set","field":"name","value":"Fix"}"#).unwrap();
        assert!(matches!(set, PageMsg::Form(crate::tui::column::view::FormOp::Set { ref field, ref value }) if field == "name" && value == "Fix"));
        let pick: PageMsg = serde_json::from_str(r#"{"type":"form","op":"pick","field":"agent","index":2}"#).unwrap();
        assert!(matches!(pick, PageMsg::Form(crate::tui::column::view::FormOp::Pick { index: 2, .. })));
        assert!(matches!(
            serde_json::from_str(r#"{"type":"form","op":"submit"}"#).unwrap(),
            PageMsg::Form(crate::tui::column::view::FormOp::Submit)
        ));
        assert!(matches!(serde_json::from_str(r#"{"type":"focus","column":true}"#).unwrap(), PageMsg::Focus { column: true }));
    }
}
