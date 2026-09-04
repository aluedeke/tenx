//! A task's activity state, derived from Claude Code's own session registry.
//!
//! The registry (`~/.claude/sessions/<pid>.json`, one file per live session,
//! rewritten by Claude Code on every status change) is read by the binary and
//! handed in here as a plain list of [`Session`]s; this module only decides
//! what those sessions *mean* for a task. tenx installs no hooks and writes no
//! state of its own — every variant below is a fact about live sessions in the
//! task's directory tree.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Statuses Claude Code writes (its own `["busy","shell","idle","waiting"]`).
/// `shell` is idle-with-a-background-shell; we treat it as idle. Anything
/// unrecognised is idle too, so a new status in a future version degrades to
/// "session present, nothing to report" rather than a wrong glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Busy,
    Waiting,
    Idle,
}

impl SessionStatus {
    pub fn from_token(token: &str) -> SessionStatus {
        match token {
            "busy" => SessionStatus::Busy,
            "waiting" => SessionStatus::Waiting,
            _ => SessionStatus::Idle,
        }
    }
}

/// One live Claude Code session.
#[derive(Debug, Clone)]
pub struct Session {
    pub pid: u32,
    /// Claude Code's session id — its transcript is
    /// `~/.claude/projects/<encoded cwd>/<session_id>.jsonl`.
    pub session_id: Option<String>,
    pub cwd: PathBuf,
    pub status: SessionStatus,
    /// Why the session is waiting, straight from Claude Code — "input needed",
    /// "sandbox request", or the open dialog's own label. `None` unless
    /// `status` is `Waiting`.
    pub waiting_for: Option<String>,
    /// When the status last changed. Drives the age column for a waiting task
    /// (how long it's been sitting on the prompt).
    pub status_updated_at: Option<SystemTime>,
    /// `interactive`, `bg`, `daemon`, … Background agents run in a task's
    /// subdirectory, which is why a task's session count can exceed the one
    /// pane you opened.
    pub kind: String,
}

/// Sessions running in `task_dir` or anywhere beneath it. Background agents get
/// their own subdirectory (`tasks/<slug>/ios-agent`), so this is a prefix
/// match on path components, not on the string — `tasks/foo` never claims
/// `tasks/foobar`.
pub fn sessions_for<'a>(sessions: &'a [Session], task_dir: &Path) -> Vec<&'a Session> {
    sessions
        .iter()
        .filter(|s| s.cwd == task_dir || s.cwd.starts_with(task_dir))
        .collect()
}

/// Only the sessions running inside tenx's own multiplexer: those whose pid is
/// one of `pane_pids` (Claude is usually the pane's command itself) or descends
/// from one (`tree` is `(pid, ppid)` pairs, as `ps -axo pid=,ppid=` prints).
///
/// The registry is per user, not per server, and `sessions_for` matches by
/// cwd alone — so without this, a Claude session started in a plain terminal,
/// in a second tmux server (`TENX_TMUX_SOCKET`), or left behind in an
/// abandoned multiplexer, is counted as the task's. The failure mode is not
/// cosmetic: a session nobody is attached to can sit on a permission prompt
/// forever, and its `waiting` pins the task to `Blocked` no matter what the
/// visible pane does.
///
/// With no panes (server down) every session is dropped: the rule is "in our
/// server", and nothing is.
pub fn in_panes(sessions: Vec<Session>, pane_pids: &[u32], tree: &[(u32, u32)]) -> Vec<Session> {
    if pane_pids.is_empty() {
        return vec![];
    }
    let mine = crate::live::descendants(pane_pids, tree);
    sessions.into_iter().filter(|s| mine.contains(&s.pid)).collect()
}

/// What the multiplexer knows about a task's window that Claude Code doesn't:
/// a process in one of its panes rang the bell (`printf '\a'`, a test runner,
/// anything) or produced output, since the window was last looked at. The
/// generic "look at me" channel — any program can use it, not just Claude.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Signal {
    pub bell: bool,
    pub activity: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// A session has a dialog open and cannot proceed until you answer it —
    /// permission prompt, elicitation, sandbox request (the 💬 indicator).
    Blocked,
    /// Something in the task's window rang the bell and nobody has looked
    /// since (the 🔔 indicator). Cleared by visiting the window.
    Signaled,
    /// A turn is in flight.
    Working,
    /// A session is live but quiet: the turn is over and it's your move.
    Done,
    /// No Claude session running in this task at all.
    Idle,
}

/// The section a status is listed under. Coarser than `TaskStatus` on purpose:
/// `Blocked` and `Done` are both *waiting on you* — one has a dialog open, the
/// other finished a turn — and splitting them put two near-identical headers
/// back to back. They share a section; the row's glyph (💬 vs ✅) and Claude's
/// waiting reason carry the difference, and `TaskStatus::rank` floats the
/// blocked ones to the top of it.
///
/// `SecretsPending` is not derived from `TaskStatus`: a task can be `Idle` and
/// still belong here, because a pending secrets request outlives the session
/// that made it. Callers that know about pending secrets override
/// `TaskStatus::group()` with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskGroup {
    /// A pending secrets request — needs a specific action (typing a
    /// passphrase), ranked above ordinary waiting for that reason.
    SecretsPending,
    /// An agent is waiting on you — a prompt to answer, or a finished turn.
    Waiting,
    /// A turn is in flight. Nothing for you to do.
    Working,
    /// No Claude session running at all.
    Inactive,
}

impl TaskGroup {
    /// Section order: secrets pending (needs a specific action from you),
    /// then what needs you, then what's running, then what isn't.
    pub fn rank(self) -> u8 {
        match self {
            TaskGroup::SecretsPending => 0,
            TaskGroup::Waiting => 1,
            TaskGroup::Working => 2,
            TaskGroup::Inactive => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskGroup::SecretsPending => "SECRETS PENDING",
            TaskGroup::Waiting => "WAITING FOR INPUT",
            TaskGroup::Working => "WORKING",
            TaskGroup::Inactive => "INACTIVE",
        }
    }
}

impl TaskStatus {
    /// The wire token for this status, as consumed by anything reading
    /// `tenx overlay --json` or the status pushes.
    pub fn token(self) -> &'static str {
        match self {
            TaskStatus::Working => "working",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Signaled => "signaled",
            TaskStatus::Done => "done",
            TaskStatus::Idle => "idle",
        }
    }

    /// Inverse of [`token`](Self::token); unknown tokens read as idle, the
    /// same degradation `SessionStatus::from_token` applies.
    pub fn from_token(token: &str) -> TaskStatus {
        match token {
            "working" => TaskStatus::Working,
            "blocked" => TaskStatus::Blocked,
            "signaled" => TaskStatus::Signaled,
            "done" => TaskStatus::Done,
            _ => TaskStatus::Idle,
        }
    }

    /// The one glyph table — the overlay and the status line both draw from
    /// it, so a new status can't render on one surface and not the other.
    /// Plain one-column text glyphs, coloured by the caller: they render in
    /// the terminal's own font at its own weight, where emoji are bitmaps
    /// that ignore both and sit heavy next to text.
    pub fn glyph(self) -> &'static str {
        match self {
            TaskStatus::Blocked => "●",
            TaskStatus::Signaled => "▲",
            TaskStatus::Done => "✔",
            TaskStatus::Working => "◐",
            TaskStatus::Idle => "·",
        }
    }

    /// Which section this status is listed under.
    pub fn group(self) -> TaskGroup {
        match self {
            TaskStatus::Blocked | TaskStatus::Signaled | TaskStatus::Done => TaskGroup::Waiting,
            TaskStatus::Working => TaskGroup::Working,
            TaskStatus::Idle => TaskGroup::Inactive,
        }
    }

    /// Ordering *within* a section. An agent parked on a prompt goes above a
    /// bell, which goes above one that merely finished: seconds of yours
    /// restart minutes of its work.
    pub fn rank(self) -> u8 {
        match self {
            TaskStatus::Blocked => 0,
            TaskStatus::Signaled => 1,
            TaskStatus::Working => 2,
            TaskStatus::Done => 3,
            TaskStatus::Idle => 4,
        }
    }

    /// Whether this state is "waiting on you" — what the watcher notifies on
    /// and what `sweep` must never close.
    pub fn needs_you(self) -> bool {
        matches!(self, TaskStatus::Blocked | TaskStatus::Signaled)
    }
}

/// A task's resolved activity state.
#[derive(Debug, Clone)]
pub struct TaskState {
    pub status: TaskStatus,
    /// When the state last changed: the session's `statusUpdatedAt`. For a
    /// quiet session that's the busy→idle transition, so the age reads "waiting
    /// on you this long".
    pub changed: Option<SystemTime>,
    /// Claude Code's reason for waiting ("input needed", "sandbox request", the
    /// open dialog's label). Only set for `Blocked`.
    pub waiting_for: Option<String>,
    /// Live sessions in this task's directory tree.
    pub sessions: usize,
    /// How many of those are background agents (`--bg`) rather than the
    /// interactive session in the task's pane.
    pub agents: usize,
}

/// Resolve a task's state from the session list plus the window's signal.
/// Precedence:
///
/// - any session `waiting` → `Blocked`, with Claude Code's own reason. A live
///   value — it clears itself the moment you answer the prompt.
/// - else the window's bell flag → `Signaled`. Also live: tmux clears it when
///   the window is visited. Outranks `Working` because a bell from the shell
///   pane (tests finished, a build broke) is for you even while an agent is
///   mid-turn in the pane next to it.
/// - else any session `busy` → `Working`.
/// - else a session exists and is quiet → `Done`: the turn is over and it's
///   your move; `changed` is the latest busy→idle transition.
/// - no live session at all → `Idle`. This is the line that matters: `Done`
///   means an agent is sitting there waiting on you, `Idle` means nothing is
///   running.
pub fn resolve_task_state(task_dir: &Path, sessions: &[Session], signal: Signal) -> TaskState {
    let live = sessions_for(sessions, task_dir);
    let count = live.len();
    let agents = live.iter().filter(|s| s.kind != "interactive").count();
    if let Some(s) = live.iter().find(|s| s.status == SessionStatus::Waiting) {
        return TaskState {
            status: TaskStatus::Blocked,
            changed: s.status_updated_at,
            waiting_for: s.waiting_for.clone(),
            sessions: count,
            agents,
        };
    }
    if signal.bell {
        // tmux records that a bell rang, not when; the age column stays blank.
        return TaskState {
            status: TaskStatus::Signaled,
            changed: None,
            waiting_for: Some("bell".to_string()),
            sessions: count,
            agents,
        };
    }
    if let Some(s) = live.iter().find(|s| s.status == SessionStatus::Busy) {
        return TaskState {
            status: TaskStatus::Working,
            changed: s.status_updated_at,
            waiting_for: None,
            sessions: count,
            agents,
        };
    }
    if live.is_empty() {
        return TaskState { status: TaskStatus::Idle, changed: None, waiting_for: None, sessions: 0, agents: 0 };
    }
    let quiet_since = live.iter().filter_map(|s| s.status_updated_at).max();
    TaskState { status: TaskStatus::Done, changed: quiet_since, waiting_for: None, sessions: count, agents }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn session(cwd: &str, status: SessionStatus, kind: &str, updated: u64) -> Session {
        Session {
            pid: 1,
            session_id: None,
            cwd: PathBuf::from(cwd),
            status,
            waiting_for: (status == SessionStatus::Waiting).then(|| "input needed".to_string()),
            status_updated_at: Some(at(updated)),
            kind: kind.to_string(),
        }
    }

    const TASK: &str = "/ws/tasks/foo";
    const QUIET: Signal = Signal { bell: false, activity: false };
    const BELL: Signal = Signal { bell: true, activity: true };

    #[test]
    fn no_sessions_is_idle() {
        let st = resolve_task_state(Path::new(TASK), &[], QUIET);
        assert_eq!(st.status, TaskStatus::Idle);
        assert_eq!((st.sessions, st.agents), (0, 0));
        assert!(st.changed.is_none());
    }

    #[test]
    fn waiting_beats_busy_and_carries_reason() {
        let s = [
            session(TASK, SessionStatus::Busy, "interactive", 10),
            session("/ws/tasks/foo/agent", SessionStatus::Waiting, "bg", 20),
        ];
        let st = resolve_task_state(Path::new(TASK), &s, QUIET);
        assert_eq!(st.status, TaskStatus::Blocked);
        assert_eq!(st.waiting_for.as_deref(), Some("input needed"));
        assert_eq!(st.changed, Some(at(20)));
        assert_eq!((st.sessions, st.agents), (2, 1));
    }

    #[test]
    fn busy_beats_quiet() {
        let s = [
            session(TASK, SessionStatus::Idle, "interactive", 10),
            session("/ws/tasks/foo/agent", SessionStatus::Busy, "bg", 5),
        ];
        let st = resolve_task_state(Path::new(TASK), &s, QUIET);
        assert_eq!(st.status, TaskStatus::Working);
        assert!(st.waiting_for.is_none());
    }

    #[test]
    fn quiet_session_is_done_with_latest_transition() {
        let s = [
            session(TASK, SessionStatus::Idle, "interactive", 10),
            session("/ws/tasks/foo/agent", SessionStatus::Idle, "bg", 30),
        ];
        let st = resolve_task_state(Path::new(TASK), &s, QUIET);
        assert_eq!(st.status, TaskStatus::Done);
        assert_eq!(st.changed, Some(at(30)));
    }

    #[test]
    fn only_sessions_under_our_panes_survive() {
        let mut a = session(TASK, SessionStatus::Waiting, "interactive", 1);
        a.pid = 300; // grandchild of pane 100
        let mut b = session(TASK, SessionStatus::Busy, "interactive", 2);
        b.pid = 200; // the pane's own process
        let mut c = session(TASK, SessionStatus::Waiting, "interactive", 3);
        c.pid = 900; // alive, same cwd, but in another server / a plain terminal
        // pane 100 → 250 → 300; pane 200; 900 hangs off init like a stray.
        let tree = vec![(100, 1), (250, 100), (300, 250), (200, 1), (900, 1)];
        let kept: Vec<u32> = in_panes(vec![a.clone(), b.clone(), c.clone()], &[100, 200], &tree).iter().map(|s| s.pid).collect();
        assert_eq!(kept, vec![300, 200]);
        // Server down: nothing is "ours", even a session that would otherwise match.
        assert!(in_panes(vec![a, b, c], &[], &tree).is_empty());
    }

    #[test]
    fn prefix_match_is_by_path_component() {
        let s = [session("/ws/tasks/foobar", SessionStatus::Busy, "interactive", 1)];
        assert_eq!(resolve_task_state(Path::new(TASK), &s, QUIET).status, TaskStatus::Idle);
        let s = [session("/ws/tasks/foo/repo/sub", SessionStatus::Busy, "interactive", 1)];
        assert_eq!(resolve_task_state(Path::new(TASK), &s, QUIET).status, TaskStatus::Working);
    }

    #[test]
    fn bell_outranks_working_and_idle_but_not_a_prompt() {
        let st = resolve_task_state(Path::new(TASK), &[], BELL);
        assert_eq!(st.status, TaskStatus::Signaled);
        assert_eq!(st.waiting_for.as_deref(), Some("bell"));
        assert!(st.changed.is_none());

        let busy = [session(TASK, SessionStatus::Busy, "interactive", 10)];
        let st = resolve_task_state(Path::new(TASK), &busy, BELL);
        assert_eq!(st.status, TaskStatus::Signaled);
        assert_eq!(st.sessions, 1);

        let waiting = [session(TASK, SessionStatus::Waiting, "interactive", 10)];
        assert_eq!(resolve_task_state(Path::new(TASK), &waiting, BELL).status, TaskStatus::Blocked);
    }

    #[test]
    fn tokens_round_trip_and_every_status_has_a_glyph() {
        for s in [TaskStatus::Blocked, TaskStatus::Signaled, TaskStatus::Working, TaskStatus::Done, TaskStatus::Idle] {
            assert_eq!(TaskStatus::from_token(s.token()), s);
            assert!(!s.glyph().is_empty());
        }
        assert_eq!(TaskStatus::from_token("whatever"), TaskStatus::Idle);
    }

    #[test]
    fn signaled_waits_on_you() {
        assert!(TaskStatus::Signaled.needs_you() && TaskStatus::Blocked.needs_you());
        assert!(!TaskStatus::Done.needs_you() && !TaskStatus::Working.needs_you());
        assert_eq!(TaskStatus::Signaled.group(), TaskGroup::Waiting);
        assert!(TaskStatus::Blocked.rank() < TaskStatus::Signaled.rank());
        assert!(TaskStatus::Signaled.rank() < TaskStatus::Working.rank());
    }

    #[test]
    fn unknown_tokens_read_as_idle() {
        assert_eq!(SessionStatus::from_token("shell"), SessionStatus::Idle);
        assert_eq!(SessionStatus::from_token("something-new"), SessionStatus::Idle);
        assert_eq!(SessionStatus::from_token("busy"), SessionStatus::Busy);
    }

    #[test]
    fn groups_and_ranks_are_ordered() {
        assert_eq!(TaskStatus::Blocked.group(), TaskGroup::Waiting);
        assert_eq!(TaskStatus::Done.group(), TaskGroup::Waiting);
        assert!(TaskStatus::Blocked.rank() < TaskStatus::Done.rank());
        assert!(TaskGroup::SecretsPending.rank() < TaskGroup::Waiting.rank());
        assert!(TaskGroup::Working.rank() < TaskGroup::Inactive.rank());
    }
}
