//! What a front end does *around* the column, shared by the terminal client
//! (`client`) and `tenx web`: the work the column asks for but can't do on
//! its own key path, because it takes a moment or needs the front end's own
//! tmux client.
//!
//! - [`Views`] puts a view in front of you (`column::AgentView`): switches
//!   Claude Code's view in a task's pane on a thread, or opens a subagent's
//!   transcript window.
//! - [`Unlock`] answers a task's pending secrets in a tmux popup over the
//!   front end's own tmux client.
//!
//! Both report back as a [`ClientRequest`] for the front end to apply to its
//! own focus and layout; outcomes worth reading go to the column's footer.

use std::sync::mpsc;

use super::column::{AgentView, ClientRequest, Column};

/// The agent-view switches of one front end. The keystrokes that switch
/// Claude's view take a moment, so they run on a thread
/// (`cli::agentview::open_in_claude`) and [`Views::poll`] picks up the
/// outcome; one runs at a time, and a request made meanwhile waits, replaced
/// by any later one — arrowing past five agents switches to the one you stop
/// on, not through all five.
#[derive(Default)]
pub(crate) struct Views {
    /// The switch in flight: the pane it opened the subagent in, or why it
    /// couldn't.
    in_flight: Option<(AgentView, mpsc::Receiver<Result<String, String>>)>,
    /// The request to run once the one in flight is done (the latest wins).
    queued: Option<AgentView>,
}

impl Views {
    pub(crate) fn start(&mut self, column: &mut Column, v: AgentView) -> Option<ClientRequest> {
        if !v.in_claude {
            return start_transcript(column, v);
        }
        if self.in_flight.is_some() {
            self.queued = Some(v);
            return None;
        }
        let (tx, rx) = mpsc::channel();
        let (pid, main, label, agent_type, nth, peers) =
            (v.session_pid, v.main, v.label.clone(), v.agent_type.clone(), v.nth, v.peers);
        std::thread::spawn(move || {
            use crate::cli::agentview::{open_in_claude, AgentRef, Target};
            let target =
                if main { Target::Main } else { Target::Agent(AgentRef { label: &label, agent_type: &agent_type, nth, peers }) };
            let _ = tx.send(open_in_claude(pid, target));
        });
        self.in_flight = Some((v, rx));
        None
    }

    /// Once a view switch is over: a waiting request goes next; ⏎'s gets the
    /// keyboard handed to the pane (in the column's session); a subagent
    /// Claude can't be switched to is opened as its transcript on ⏎, and only
    /// named in the footer when the cursor merely landed on it.
    pub(crate) fn poll(&mut self, column: &mut Column) -> Option<ClientRequest> {
        let (_, rx) = self.in_flight.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("the agent view thread went away".into()),
        };
        let (v, _) = self.in_flight.take()?;
        if let Some(next) = self.queued.take() {
            return self.start(column, next);
        }
        match result {
            Ok(pane) if v.focus => {
                let _ = crate::tmux::focus_pane_in(column.session(), &pane);
                Some(ClientRequest::FocusTerminal)
            }
            Ok(_) => None,
            Err(e) if v.focus && v.transcript.is_some() => {
                column.set_status(format!("{e} — showing its transcript"));
                start_transcript(column, v)
            }
            Err(e) if !v.focus && !v.main && v.transcript.is_some() => {
                column.set_status(format!("{e} · t for its transcript"));
                None
            }
            Err(e) => {
                column.set_status(e);
                None
            }
        }
    }
}

/// Follow a subagent's transcript in a tmux window of its own
/// (`tmux::open_agent_window`, `↳ <label>`), and hand it the keyboard;
/// `q` in it closes it and tmux goes back to the window before.
fn start_transcript(column: &mut Column, v: AgentView) -> Option<ClientRequest> {
    let transcript = v.transcript.clone()?;
    let bin = crate::tmux::self_bin().ok()?;
    let cwd = v.task_path.to_string_lossy().into_owned();
    let args = crate::tmux::subagent_log_args(&cwd, v.session_pid, &v.agent, &transcript.to_string_lossy(), &v.title);
    let name = format!("↳ {}", v.label.chars().take(24).collect::<String>());
    match crate::tmux::open_agent_window(&bin.to_string_lossy(), &cwd, &name, &args) {
        Ok(()) => Some(ClientRequest::FocusTerminal),
        Err(e) => {
            column.set_status(format!("couldn't open the agent: {e}"));
            None
        }
    }
}

/// An unlock popup of one front end.
#[derive(Default)]
pub(crate) struct Unlock {
    /// The popup in flight: the task's slug, and where its thread reports the
    /// popup's exit status once it closes.
    in_flight: Option<(String, mpsc::Receiver<Result<i32, String>>)>,
}

/// How [`Unlock::start`] went.
pub(crate) enum UnlockStart {
    /// The popup is up (or one already was): hand the keyboard to the
    /// terminal, which is where the popup takes its keys.
    Started(Option<ClientRequest>),
    /// No popup could be aimed — no tmux client found for this front end's
    /// attach — so the caller unlocks some other way.
    NoClient,
}

impl Unlock {
    /// Answer a task's pending secrets in a tmux popup over the tmux client
    /// whose process is `attach_pid` (`cli::secrets::fulfill` with `--hold`),
    /// the column staying live behind it. `display-popup` blocks until it
    /// closes, so it runs on a thread; [`Unlock::poll`] picks up the result.
    pub(crate) fn start(&mut self, column: &mut Column, ws_idx: usize, slug: &str, attach_pid: Option<u32>) -> UnlockStart {
        if self.in_flight.is_some() {
            column.set_status("an unlock is already open".into());
            return UnlockStart::Started(None);
        }
        let Some(task) = column.unlock_task(ws_idx, slug) else { return UnlockStart::NoClient };
        let Some(tmux_client) = attach_pid.and_then(crate::tmux::client_by_pid) else { return UnlockStart::NoClient };
        let Ok(bin) = crate::tmux::self_bin() else { return UnlockStart::NoClient };
        let (tx, rx) = mpsc::channel();
        let title = format!(" secrets · {} ", task.display_name);
        std::thread::spawn(move || {
            let result = crate::tmux::popup_tenx(
                &tmux_client,
                &task.path.to_string_lossy(),
                &title,
                &bin.to_string_lossy(),
                "secrets fulfill --hold",
            );
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        self.in_flight = Some((slug.to_string(), rx));
        UnlockStart::Started(Some(ClientRequest::FocusTerminal))
    }

    /// Once the popup has closed: the rows rebuilt so the task leaves SECRETS
    /// PENDING, and the outcome in the footer. `true` then — the front end
    /// brings the column back.
    pub(crate) fn poll(&mut self, column: &mut Column) -> bool {
        let Some((slug, rx)) = &self.in_flight else { return false };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err("the popup thread went away".into()),
        };
        let slug = slug.clone();
        self.in_flight = None;
        column.rebuild_rows();
        column.set_status(match result {
            Ok(0) => format!("secrets answered for '{slug}'"),
            Ok(_) => format!("unlock for '{slug}' didn't finish — tenx secrets status"),
            Err(e) => format!("unlock popup failed: {e}"),
        });
        true
    }
}

/// How often the column's rows refresh.
pub(crate) const REFRESH: std::time::Duration = std::time::Duration::from_millis(500);

/// The column's clock: call [`Ticker::tick`] every frame.
pub(crate) struct Ticker {
    last_refresh: std::time::Instant,
}

impl Default for Ticker {
    fn default() -> Self {
        Ticker { last_refresh: std::time::Instant::now() }
    }
}

impl Ticker {
    pub(crate) fn tick(&mut self, column: &mut Column) {
        // Every frame, not on the slow clock: a job's panel animates, and its
        // events arrive as fast as git writes them. This is also the only
        // place a finished job's effects reach the column — on this thread,
        // never on the worker's.
        column.drain_job();
        if self.last_refresh.elapsed() >= REFRESH {
            self.last_refresh = std::time::Instant::now();
            if column.in_list_mode() {
                column.refresh_statuses();
                // A status change moves its task to the right section at
                // once; the selection follows its task, so this is safe
                // under a moving cursor too.
                if column.sections_stale() {
                    column.tidy();
                }
            }
        }
    }
}
