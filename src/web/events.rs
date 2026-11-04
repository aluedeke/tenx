//! `/mcp`'s push half (`tenx_core::mcppush`): the MCP sessions, their event
//! streams, and the thread that notices what happens. One resolve pass
//! serves both the phone notifications (`push`) and the MCP events — the
//! same `cli::watch::Attention` edges, never a second copy of the rule.
//!
//! - `attention`: those edges.
//! - `turn` / `reply`: `tenx_core::mcppush::Turns` over the same pass; the
//!   reply text is `cli::drive::last_turn`, read on a thread of its own.
//! - `focus`: each `tenx web` tab reports its view's `current` when it
//!   changes (`tab::Driver`); the terminal side is the tenx session's
//!   current window, looked at every `FOCUS_POLL`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde_json::Value;
use tenx_core::mcppush::{self, EventKind, Focus, Replies, Screen, Sessions, TaskRef, Turns};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// One event on a stream: its id and the notification's JSON.
pub(super) type Event = (u64, String);

#[derive(Default)]
struct Inner {
    sessions: Sessions,
    /// The open stream of each session that has one.
    streams: HashMap<String, UnboundedSender<Event>>,
    focus: Focus,
    replies: Replies,
}

/// Every MCP session, and what each waits for.
#[derive(Default)]
pub(super) struct Hub {
    inner: Mutex<Inner>,
    messages: AtomicU64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn now_at() -> String {
    mcppush::rfc3339(SystemTime::now())
}

impl Hub {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new session (`initialize`): its id, 128 random bits as hex.
    pub(super) fn open(&self) -> Result<String> {
        let id = super::token::random_hex(16)?;
        let mut inner = self.lock();
        if let Some(evicted) = inner.sessions.open(&id, now_secs()) {
            inner.streams.remove(&evicted);
            inner.replies.forget(&evicted);
        }
        Ok(id)
    }

    /// Whether `id` is a live session; a request marks it active.
    pub(super) fn touch(&self, id: &str) -> bool {
        self.lock().sessions.touch(id, now_secs())
    }

    /// `DELETE /mcp`: end it, and its stream.
    pub(super) fn close(&self, id: &str) -> bool {
        let mut inner = self.lock();
        inner.streams.remove(id);
        inner.replies.forget(id);
        inner.sessions.close(id)
    }

    pub(super) fn subscribe(&self, id: &str, events: BTreeSet<EventKind>) -> bool {
        self.lock().sessions.subscribe(id, events)
    }

    /// `GET /mcp`: the events to replay (after `last`), then the live ones.
    /// Replaces an earlier stream of the session, which then ends. `None`:
    /// no such session.
    pub(super) fn stream(&self, id: &str, last: Option<u64>) -> Option<(Vec<Event>, UnboundedReceiver<Event>)> {
        let mut inner = self.lock();
        if !inner.sessions.touch(id, now_secs()) {
            return None;
        }
        let replay = inner.sessions.replay(id, last);
        let (tx, rx) = unbounded_channel();
        inner.streams.insert(id.to_string(), tx);
        Some((replay, rx))
    }

    /// Number, buffer and send an event to every session subscribed to its
    /// kind (or to `to` only).
    fn publish(&self, kind: EventKind, params: &Value, to: Option<&str>) {
        let mut inner = self.lock();
        for (session, n, data) in inner.sessions.publish(kind, params, to) {
            if let Some(tx) = inner.streams.get(&session) {
                let _ = tx.send((n, data));
            }
        }
    }

    /// A `tenx_send_message`'s id; a session's gets the task's next turn as
    /// a `reply`.
    pub(super) fn message(&self, session: Option<&str>, task_id: &str) -> String {
        let id = mcppush::message_id(self.messages.fetch_add(1, Ordering::SeqCst) + 1);
        if let Some(s) = session {
            self.lock().replies.expect(task_id, s, &id);
        }
        id
    }

    /// The task a screen showed last: `(screen, row id)`.
    pub(super) fn current(&self, screen: Option<&Screen>) -> (Screen, Option<String>) {
        self.lock().focus.current(screen)
    }

    /// What `screen` shows now (`row`: `<workspace>/<slug>`); a change goes
    /// out as `focus`. `prime`: the first look at start, no event.
    pub(super) fn focus(&self, screen: &Screen, row: Option<&str>, prime: bool) {
        if !self.lock().focus.update(screen, row, prime) {
            return;
        }
        let task = row.and_then(task_ref);
        self.publish(EventKind::Focus, &mcppush::focus(&now_at(), screen, task.as_ref()), None);
    }

    /// A tab that ended shows nothing any more.
    pub(super) fn screen_gone(&self, screen: &Screen) {
        self.lock().focus.remove(screen);
    }

    /// End the sessions idle too long; drop the streams whose client left.
    fn sweep(&self) {
        let mut inner = self.lock();
        inner.streams.retain(|_, tx| !tx.is_closed());
        let Inner { sessions, streams, replies, .. } = &mut *inner;
        for gone in sessions.expire(now_secs(), |id| streams.contains_key(id)) {
            replies.forget(&gone);
        }
    }
}

/// A row id (`<workspace>/<slug>`) as an event names it, title included.
pub(super) fn task_ref(row: &str) -> Option<TaskRef> {
    let (ws, slug) = tenx_core::web::task_id(row)?;
    let ws = crate::workspace::registered_workspaces().into_iter().find(|w| w.config.name == ws)?;
    let task = ws.find_task(slug).ok()?;
    Some(TaskRef { workspace: ws.config.name.clone(), task: task.name.clone(), title: task.display_name.clone() })
}

/// The row id of the task whose directory is `dir`.
fn row_for_dir(dir: &Path) -> Option<String> {
    let slug = dir.file_name()?.to_str()?;
    let parent = dir.parent()?;
    let ws = crate::workspace::registered_workspaces().into_iter().find(|w| w.tasks_dir() == parent)?;
    Some(format!("{}/{slug}", ws.config.name))
}

/// The task directory the tenx session's current window was opened for.
fn terminal_task_dir() -> Option<PathBuf> {
    let dir = crate::tmux::window_option_in(crate::tmux::SESSION, crate::tmux::TASK_DIR_OPTION)?;
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

/// `TENX_WEB_NO_PUSH=1`: compute every edge, deliver no Web Push. For a
/// second `tenx web` run to test with, which would otherwise buzz the
/// user's phone with every notification a second time.
fn push_disabled() -> bool {
    std::env::var("TENX_WEB_NO_PUSH").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// The notifier thread: every `POLL`, the watcher's resolve pass — its
/// attention edges pushed to every phone and published to MCP sessions,
/// finished turns published — and every `FOCUS_POLL` the terminal's task.
/// Runs for the server's life.
pub(super) fn notifier(push: Arc<super::push::Push>, hub: Arc<Hub>) {
    const POLL: Duration = Duration::from_secs(2);
    const FOCUS_POLL: Duration = Duration::from_millis(500);
    let per_poll = (POLL.as_millis() / FOCUS_POLL.as_millis()) as u32;
    let no_push = push_disabled();
    if no_push {
        eprintln!("tenx web: TENX_WEB_NO_PUSH is set — no Web Push is delivered");
    }

    let snapshot = crate::cli::watch::resolve_all();
    let mut attention = crate::cli::watch::Attention::primed(&snapshot);
    let mut turns = Turns::default();
    step_turns(&mut turns, &snapshot);
    let mut terminal_dir = terminal_task_dir();
    hub.focus(&Screen::Terminal, terminal_dir.as_deref().and_then(row_for_dir).as_deref(), true);

    let mut tick = 0u32;
    loop {
        std::thread::sleep(FOCUS_POLL);
        tick = tick.wrapping_add(1);

        let dir = terminal_task_dir();
        if dir != terminal_dir {
            hub.focus(&Screen::Terminal, dir.as_deref().and_then(row_for_dir).as_deref(), false);
            terminal_dir = dir;
        }

        if !tick.is_multiple_of(per_poll) {
            continue;
        }
        let snapshot = crate::cli::watch::resolve_all();
        let notes = attention.step(&snapshot);
        let at = now_at();
        for (note, _) in &notes {
            let task = TaskRef { workspace: note.workspace.clone(), task: note.slug.clone(), title: note.task.clone() };
            hub.publish(EventKind::Attention, &mcppush::attention(&at, &task, note.status, note.reason.as_deref()), None);
        }
        for finished in step_turns(&mut turns, &snapshot) {
            let hub = hub.clone();
            std::thread::spawn(move || announce_turn(&hub, finished));
        }
        hub.sweep();
        if !no_push {
            for (note, _) in notes {
                push.send_all(&super::push::message(note));
            }
        }
    }
}

/// The tasks of `snapshot` that finished a turn since the last step.
fn step_turns(turns: &mut Turns, snapshot: &crate::cli::watch::Snapshot) -> Vec<TaskRef> {
    let now: Vec<(&str, &str, Option<SystemTime>)> =
        snapshot.turns.iter().map(|t| (t.id.as_str(), t.status.token(), t.changed)).collect();
    let done = turns.step(&now);
    snapshot
        .turns
        .iter()
        .filter(|t| done.contains(&t.id.as_str()))
        .map(|t| TaskRef { workspace: t.workspace.clone(), task: t.slug.clone(), title: t.title.clone() })
        .collect()
}

/// A finished turn: `turn` to its subscribers, `reply` to each session
/// that sent the task a message. Reads the transcript, so off the poll.
fn announce_turn(hub: &Hub, task: TaskRef) {
    let row = format!("{}/{}", task.workspace, task.task);
    let reply = super::mcp::settled_reply(&task.workspace, &task.task).unwrap_or_default();
    let at = now_at();
    hub.publish(EventKind::Turn, &mcppush::turn(&at, &task, &reply), None);
    let waiting = hub.lock().replies.take(&row);
    for (session, message_id) in waiting {
        hub.publish(EventKind::Reply, &mcppush::reply(&at, &task, &message_id, &reply), Some(&session));
    }
}
