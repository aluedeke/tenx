//! A task's activity state, derived from tenx's session registry.
//!
//! The registry (`~/.config/tenx/sessions/<pid>.json`, one file per live agent
//! process, rewritten by every agent's hooks through `tenx internal
//! session-event`) is read by the binary and handed in here as a plain list of
//! [`Session`]s, each carrying its own subagents; this module only decides what
//! those sessions *mean* for a task. Every variant below is a fact about live
//! sessions in the task's directory tree.

use crate::subagent::{Subagent, SubagentStatus};
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

    /// The wire token written into a session record's `status` field. Inverse
    /// of [`from_token`](Self::from_token).
    pub fn token(self) -> &'static str {
        match self {
            SessionStatus::Busy => "busy",
            SessionStatus::Waiting => "waiting",
            SessionStatus::Idle => "idle",
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
    /// The tmux pane the session runs in (`%40`), from the registry's own
    /// `tmux` field. What the column previews and sends keys to.
    pub pane: Option<String>,
    /// A parked turn: the interactive session hands its running turn to a
    /// worker hosted by Claude Code's daemon (`claude bg-spare`) and records
    /// the worker's job here (`parkedJobId`). The worker keeps its own
    /// registry entry — `kind` "bg", `job_id` set, no `tmux` field, status
    /// `waiting` while the dialog it draws in the *interactive* pane is
    /// open — and the interactive entry reads `busy` throughout.
    pub parked_job_id: Option<String>,
    /// The worker side of a parked turn (`jobId`).
    pub job_id: Option<String>,
    /// Which harness this session runs — `claude`, `codex`, `pi`. Set from the
    /// record's `agent` field; drives the overlay's per-agent chip. Empty for
    /// a record that predates the field (read as the default agent).
    pub agent: String,
    /// Claude Code's `permission_mode` from the last hook payload (`default`,
    /// `auto`, `acceptEdits`, …). `None` for other agents and old records.
    /// Only `auto` matters: see [`confirm_permission_waits`].
    pub permission_mode: Option<String>,
    /// The subagents this session has spawned that are still worth listing
    /// (`crate::subagent::visible`), in display order.
    pub subagents: Vec<Subagent>,
    /// What the session was asked — its first prompt, as the agent reported
    /// it (pi's extension does). The label it gets when it turns out to be
    /// another session's subagent ([`nest_child_sessions`]).
    pub label: Option<String>,
}

/// Turn every session that runs *under* another session's process into that
/// session's subagent — the shape of a pi subagent, which an extension runs as
/// a child `pi` process that reports to the registry like any other.
///
/// `tree` is `(pid, ppid)` pairs; `nestable` says which sessions may be
/// nested (the binary passes pi's: Claude Code's and Codex's subagents are
/// reported by their hooks, and a `claude --bg` a session started is a
/// background agent of the task, not of that session). A nested session is
/// removed from the list and added to its nearest ancestor session's
/// `subagents`: busy → running, waiting → waiting (with its reason), idle →
/// finished. Its id is `pid-<pid>`, so a view keeps it across ticks.
pub fn nest_child_sessions(sessions: Vec<Session>, tree: &[(u32, u32)], nestable: &dyn Fn(&Session) -> bool) -> Vec<Session> {
    let pids: Vec<u32> = sessions.iter().map(|s| s.pid).collect();
    let parent_of = |pid: u32| tree.iter().find(|(p, _)| *p == pid).map(|(_, pp)| *pp);
    let owner_of = |s: &Session| -> Option<u32> {
        let mut pid = s.pid;
        for _ in 0..16 {
            pid = parent_of(pid).filter(|pp| *pp > 1 && *pp != pid)?;
            if pids.contains(&pid) {
                return Some(pid);
            }
        }
        None
    };
    let mut children: Vec<(u32, Session)> = Vec::new();
    let mut kept: Vec<Session> = Vec::new();
    for s in sessions {
        match owner_of(&s).filter(|_| nestable(&s)) {
            Some(owner) => children.push((owner, s)),
            None => kept.push(s),
        }
    }
    for (owner, c) in children {
        // A grandchild whose parent was itself nested goes to the outermost
        // session still listed; with no such session, it stays a session.
        let mut owner = owner;
        while !kept.iter().any(|k| k.pid == owner) {
            match parent_of(owner).filter(|pp| *pp > 1 && *pp != owner) {
                Some(pp) => owner = pp,
                None => break,
            }
        }
        let Some(parent) = kept.iter_mut().find(|k| k.pid == owner) else {
            kept.push(c);
            continue;
        };
        parent.subagents.push(Subagent {
            id: format!("pid-{}", c.pid),
            session_pid: owner,
            agent: c.agent.clone(),
            agent_type: c.agent.clone(),
            description: c.label.clone(),
            status: match c.status {
                SessionStatus::Busy => SubagentStatus::Running,
                SessionStatus::Waiting => SubagentStatus::Waiting,
                SessionStatus::Idle => SubagentStatus::Finished,
            },
            waiting_for: c.waiting_for.clone(),
            started_at: c.status_updated_at,
            updated_at: c.status_updated_at,
            transcript_path: None,
            background: false,
        });
    }
    kept
}

/// Second-guess a `waiting` on a permission dialog against the screen.
///
/// The registry alone gets these wrong in two ways, both measured against
/// Claude Code 2.1.267. In auto mode, `PermissionRequest` fires before the
/// classifier decides and nothing follows an *allow* until the tool
/// finishes, so a long `Bash` call the classifier waved through reads as
/// `waiting` for its whole run (the `permission_prompt` notification
/// doesn't help: it arrives seconds later whether or not a dialog was
/// shown). In every mode, a dialog the user *denies* — Escape, or the "No"
/// option — fires no hook at all, not even `Stop`, so the record says
/// `waiting` until the next prompt.
///
/// `activity(pane)` reads the pane (`crate::dialog::pane_activity` on a
/// capture): the dialog on screen keeps the wait; "esc to interrupt" means
/// the turn went on (allowed, tool running) → busy; neither means the turn
/// is over (denied) → idle. Sessions on any other reason or without a pane,
/// and a failed capture (`None`), are left alone — fail closed, so a real
/// prompt is never hidden.
///
/// A subagent's permission wait is drawn in its session's pane and is checked
/// against the same capture: dialog → still waiting; otherwise it went on
/// (allowed or denied, the subagent carries on) → running — except that a
/// quiet pane means the turn is over, which a foreground subagent cannot
/// outlive → finished.
pub fn confirm_permission_waits(sessions: &mut [Session], activity: &dyn Fn(&str) -> Option<crate::dialog::PaneActivity>) {
    use crate::dialog::PaneActivity;
    for s in sessions.iter_mut() {
        let sub_waits = s
            .subagents
            .iter()
            .any(|a| a.status == SubagentStatus::Waiting && a.waiting_for.as_deref().is_some_and(crate::dialog::is_permission_reason));
        if sub_waits && let Some(pane) = s.pane.as_deref() {
            let seen = activity(pane);
            for a in s.subagents.iter_mut() {
                if a.status != SubagentStatus::Waiting || !a.waiting_for.as_deref().is_some_and(crate::dialog::is_permission_reason) {
                    continue;
                }
                match seen {
                    Some(PaneActivity::Running) => a.status = SubagentStatus::Running,
                    Some(PaneActivity::Idle) if a.background => a.status = SubagentStatus::Running,
                    Some(PaneActivity::Idle) => a.status = SubagentStatus::Finished,
                    Some(PaneActivity::Dialog) | None => continue,
                }
                a.waiting_for = None;
            }
        }
        if s.status != SessionStatus::Waiting {
            continue;
        }
        if !s.waiting_for.as_deref().is_some_and(crate::dialog::is_permission_reason) {
            continue;
        }
        let Some(pane) = s.pane.as_deref() else { continue };
        match activity(pane) {
            Some(PaneActivity::Running) => {
                s.status = SessionStatus::Busy;
                s.waiting_for = None;
            }
            Some(PaneActivity::Idle) => {
                s.status = SessionStatus::Idle;
                s.waiting_for = None;
            }
            Some(PaneActivity::Dialog) | None => {}
        }
    }
}

/// True if `s` is the worker half of a parked turn whose interactive session
/// is in `sessions`: its `job_id` is some session's `parked_job_id`.
pub fn is_parked_worker(s: &Session, sessions: &[&Session]) -> bool {
    match &s.job_id {
        Some(job) => sessions.iter().any(|o| o.pid != s.pid && o.parked_job_id.as_deref() == Some(job)),
        None => false,
    }
}

/// Collapse every parked turn into one session. While a turn is parked the
/// interactive entry is a viewer: its status field is not rewritten again
/// (observed: `busy` with the park-time timestamp, unchanged through the
/// worker's busy → waiting → busy → idle), so it must not be read. The worker
/// is the truth for status, reason and age; the interactive session is the
/// truth for the pane (that is where the worker's dialog is drawn) and for
/// identity. So the interactive session takes the worker's status fields
/// and the worker is dropped — the pair is one session, not an agent plus
/// its parent. A parked session whose worker is missing (not alive, or not
/// ours) reads as `Idle`: the one thing certain about its own status is that
/// it is stale.
pub fn fold_parked(sessions: Vec<Session>) -> Vec<Session> {
    let workers: Vec<Session> = sessions.iter().filter(|s| s.job_id.is_some()).cloned().collect();
    let claimed: Vec<u32> = sessions
        .iter()
        .filter_map(|s| s.parked_job_id.as_deref())
        .filter_map(|job| workers.iter().find(|w| w.job_id.as_deref() == Some(job)))
        .map(|w| w.pid)
        .collect();
    sessions
        .into_iter()
        .filter(|s| !claimed.contains(&s.pid))
        .map(|mut s| {
            let Some(job) = s.parked_job_id.as_deref() else {
                return s;
            };
            match workers.iter().find(|w| w.job_id.as_deref() == Some(job)) {
                Some(w) => {
                    s.status = w.status;
                    s.waiting_for = w.waiting_for.clone();
                    s.status_updated_at = w.status_updated_at;
                }
                None => {
                    s.status = SessionStatus::Idle;
                    s.waiting_for = None;
                }
            }
            s
        })
        .collect()
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
/// The one exception is a parked turn's worker (see [`Session::parked_job_id`]):
/// it runs under Claude Code's daemon, off `init`, never under a pane — yet it
/// is the entry that says `waiting` while the permission dialog it draws in
/// the interactive pane is open. It is kept when the interactive session that
/// parked the job is itself in our panes; without that, the task reads
/// `Working` while a prompt sits on screen. [`fold_parked`] then merges the
/// pair; callers run both, in that order.
///
/// With no panes (server down) every session is dropped: the rule is "in our
/// server", and nothing is.
pub fn in_panes(sessions: Vec<Session>, pane_pids: &[u32], tree: &[(u32, u32)]) -> Vec<Session> {
    if pane_pids.is_empty() {
        return vec![];
    }
    let mine = crate::live::descendants(pane_pids, tree);
    let in_tree: Vec<&Session> = sessions.iter().filter(|s| mine.contains(&s.pid)).collect();
    let workers: Vec<u32> = sessions.iter().filter(|s| is_parked_worker(s, &in_tree)).map(|s| s.pid).collect();
    sessions.into_iter().filter(|s| mine.contains(&s.pid) || workers.contains(&s.pid)).collect()
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
    /// `tenx task list --json` or the status pushes.
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

    /// The one glyph table — the column and the status line both draw from
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
    /// The pane to look at for this task: the waiting session's when
    /// `Blocked`, else the interactive session's, else any session's.
    pub pane: Option<String>,
    /// Every listed subagent of the task's sessions, waiting ones first — the
    /// task's child items. `TaskState::subagents[i].session_pid` names the
    /// session it belongs to.
    pub subagents: Vec<Subagent>,
}

/// Resolve a task's state from the session list plus the window's signal.
/// Precedence:
///
/// - any session or subagent `waiting` → `Blocked`, with the agent's own
///   reason. A live value — it clears
///   itself the moment you answer the prompt.
/// - else the window's bell flag → `Signaled`. Also live: tmux clears it when
///   the window is visited. Outranks `Working` because a bell from the shell
///   pane (tests finished, a build broke) is for you even while an agent is
///   mid-turn in the pane next to it.
/// - else any session `busy` or subagent running → `Working`. A background
///   subagent keeps its task working after its session's turn has ended: the
///   session will pick up again when it reports back.
/// - else a session exists and is quiet → `Done`: the turn is over and it's
///   your move; `changed` is the latest busy→idle transition.
/// - no live session at all → `Idle`. This is the line that matters: `Done`
///   means an agent is sitting there waiting on you, `Idle` means nothing is
///   running.
pub fn resolve_task_state(task_dir: &Path, sessions: &[Session], signal: Signal) -> TaskState {
    let live = sessions_for(sessions, task_dir);
    let count = live.len();
    let agents = live.iter().filter(|s| s.kind != "interactive").count();
    let pane = live
        .iter()
        .find(|s| s.kind == "interactive")
        .or(live.first())
        .and_then(|s| s.pane.clone());
    let mut subagents: Vec<Subagent> = live.iter().flat_map(|s| s.subagents.iter().cloned()).collect();
    subagents.sort_by_key(|a| match a.status {
        SubagentStatus::Waiting => 0,
        SubagentStatus::Running => 1,
        SubagentStatus::Finished => 2,
    });
    let state = |status, changed, waiting_for, pane| TaskState {
        status,
        changed,
        waiting_for,
        sessions: count,
        agents,
        pane,
        subagents: subagents.clone(),
    };
    if let Some(s) = live.iter().find(|s| s.status == SessionStatus::Waiting) {
        return state(TaskStatus::Blocked, s.status_updated_at, s.waiting_for.clone(), s.pane.clone().or(pane));
    }
    let owner = |a: &Subagent| live.iter().find(|s| s.pid == a.session_pid);
    if let Some(a) = subagents.iter().find(|a| a.status == SubagentStatus::Waiting) {
        // The reason stays the agent's own ("permission: Bash"), so `A`/`D`
        // recognise a subagent's permission dialog like any other; which
        // subagent is waiting is its own line's glyph.
        let reason = a.waiting_for.clone().unwrap_or_else(|| "input needed".to_string());
        let s_pane = owner(a).and_then(|s| s.pane.clone());
        return state(TaskStatus::Blocked, a.updated_at, Some(reason), s_pane.or(pane));
    }
    if signal.bell {
        // tmux records that a bell rang, not when; the age column stays blank.
        return state(TaskStatus::Signaled, None, Some("bell".to_string()), pane);
    }
    if let Some(s) = live.iter().find(|s| s.status == SessionStatus::Busy) {
        return state(TaskStatus::Working, s.status_updated_at, None, pane);
    }
    if let Some(a) = subagents.iter().find(|a| a.status == SubagentStatus::Running) {
        return state(TaskStatus::Working, a.updated_at, None, pane);
    }
    if live.is_empty() {
        return TaskState {
            status: TaskStatus::Idle,
            changed: None,
            waiting_for: None,
            sessions: 0,
            agents: 0,
            pane: None,
            subagents: vec![],
        };
    }
    let quiet_since = live.iter().filter_map(|s| s.status_updated_at).max();
    state(TaskStatus::Done, quiet_since, None, pane)
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
            pane: None,
            parked_job_id: None,
            job_id: None,
            agent: "claude".to_string(),
            permission_mode: None,
            subagents: vec![],
            label: None,
        }
    }

    #[test]
    fn a_child_pi_process_is_its_sessions_subagent() {
        // pane 100 runs pi (session 200); its extension spawned pi 300 via a
        // shell (250), and pi 300 spawned pi 400. A claude session (500) under
        // the same pane is not nested: only pi is.
        let pi = |pid: u32, status| {
            let mut s = session(TASK, status, if pid == 200 { "interactive" } else { "bg" }, 10);
            s.pid = pid;
            s.agent = "pi".into();
            s.label = Some(format!("task {pid}"));
            s
        };
        let mut claude = session(TASK, SessionStatus::Busy, "bg", 10);
        claude.pid = 500;
        let tree = vec![(200, 100), (250, 200), (300, 250), (400, 300), (500, 200)];
        let out = nest_child_sessions(
            vec![pi(200, SessionStatus::Busy), pi(300, SessionStatus::Waiting), pi(400, SessionStatus::Idle), claude],
            &tree,
            &|s| s.agent == "pi",
        );
        let pids: Vec<u32> = out.iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![200, 500]);
        let subs: Vec<(String, SubagentStatus, u32)> =
            out[0].subagents.iter().map(|a| (a.id.clone(), a.status, a.session_pid)).collect();
        assert_eq!(
            subs,
            vec![("pid-300".into(), SubagentStatus::Waiting, 200), ("pid-400".into(), SubagentStatus::Finished, 200)]
        );
        assert_eq!(out[0].subagents[0].description.as_deref(), Some("task 300"));
        assert_eq!(out[0].subagents[0].agent, "pi");
        // The task reads Blocked on the child's prompt.
        let st = resolve_task_state(Path::new(TASK), &out, QUIET);
        assert_eq!(st.status, TaskStatus::Blocked);
        // A pi with no session above it stays a session.
        let alone = nest_child_sessions(vec![pi(300, SessionStatus::Busy)], &tree, &|s| s.agent == "pi");
        assert_eq!(alone.len(), 1);
    }

    fn subagent(id: &str, pid: u32, status: SubagentStatus, updated: u64) -> Subagent {
        Subagent {
            id: id.into(),
            session_pid: pid,
            agent: "claude".into(),
            agent_type: "Explore".into(),
            description: Some(format!("agent {id}")),
            status,
            waiting_for: (status == SubagentStatus::Waiting).then(|| "permission: Bash".to_string()),
            started_at: Some(at(updated)),
            updated_at: Some(at(updated)),
            transcript_path: None,
            background: false,
        }
    }

    #[test]
    fn a_waiting_subagent_blocks_its_task_on_its_sessions_pane() {
        let mut s = session(TASK, SessionStatus::Busy, "interactive", 10);
        s.pid = 7;
        s.pane = Some("%3".into());
        s.subagents = vec![subagent("a", 7, SubagentStatus::Running, 11), subagent("b", 7, SubagentStatus::Waiting, 12)];
        let st = resolve_task_state(Path::new(TASK), &[s], QUIET);
        assert_eq!(st.status, TaskStatus::Blocked);
        // The plain reason, so `A`/`D` treat it as the permission dialog it is.
        assert_eq!(st.waiting_for.as_deref(), Some("permission: Bash"));
        assert!(crate::dialog::is_permission_reason(st.waiting_for.as_deref().unwrap()));
        assert_eq!(st.changed, Some(at(12)));
        assert_eq!(st.pane.as_deref(), Some("%3"));
        // Listed waiting first.
        let ids: Vec<&str> = st.subagents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn a_background_subagent_keeps_a_quiet_session_working() {
        let mut s = session(TASK, SessionStatus::Idle, "interactive", 10);
        s.subagents = vec![subagent("a", 1, SubagentStatus::Running, 20)];
        let st = resolve_task_state(Path::new(TASK), &[s.clone()], QUIET);
        assert_eq!((st.status, st.changed), (TaskStatus::Working, Some(at(20))));
        // Finished ones are listed but don't change the status.
        s.subagents = vec![subagent("a", 1, SubagentStatus::Finished, 20)];
        let st = resolve_task_state(Path::new(TASK), &[s], QUIET);
        assert_eq!(st.status, TaskStatus::Done);
        assert_eq!(st.subagents.len(), 1);
    }

    #[test]
    fn a_subagents_permission_wait_follows_the_screen() {
        use crate::dialog::PaneActivity::*;
        let mut s = session(TASK, SessionStatus::Busy, "interactive", 1);
        s.pane = Some("%1".into());
        let mut bg = subagent("bg", 1, SubagentStatus::Waiting, 1);
        bg.background = true;
        s.subagents = vec![subagent("fg", 1, SubagentStatus::Waiting, 1), bg];
        let run = |a: Option<crate::dialog::PaneActivity>| {
            let mut v = vec![s.clone()];
            confirm_permission_waits(&mut v, &|_| a);
            v[0].subagents.iter().map(|a| a.status).collect::<Vec<_>>()
        };
        assert_eq!(run(Some(Dialog)), vec![SubagentStatus::Waiting, SubagentStatus::Waiting]);
        assert_eq!(run(None), vec![SubagentStatus::Waiting, SubagentStatus::Waiting]);
        assert_eq!(run(Some(Running)), vec![SubagentStatus::Running, SubagentStatus::Running]);
        // The turn is over: a foreground subagent went with it, a background one runs on.
        assert_eq!(run(Some(Idle)), vec![SubagentStatus::Finished, SubagentStatus::Running]);
    }

    #[test]
    fn permission_waits_follow_the_screen() {
        use crate::dialog::PaneActivity::*;
        let mut s = session("/w/t", SessionStatus::Waiting, "interactive", 1);
        s.waiting_for = Some("permission: Bash".into());
        s.pane = Some("%1".into());
        let run = |s: &Session, a: Option<crate::dialog::PaneActivity>| {
            let mut v = vec![s.clone()];
            confirm_permission_waits(&mut v, &|_| a);
            (v[0].status, v[0].waiting_for.clone())
        };
        // The dialog is up: still waiting.
        assert_eq!(run(&s, Some(Dialog)), (SessionStatus::Waiting, Some("permission: Bash".into())));
        // Allowed (by the classifier, or by you in the pane), tool running.
        assert_eq!(run(&s, Some(Running)), (SessionStatus::Busy, None));
        // Denied in the pane: the turn is over and no hook said so.
        assert_eq!(run(&s, Some(Idle)), (SessionStatus::Idle, None));
        // Capture failed: fail closed.
        assert_eq!(run(&s, None), (SessionStatus::Waiting, Some("permission: Bash".into())));
        // The notification's reason is checked the same way; other waits never.
        let mut n = s.clone();
        n.waiting_for = Some("permission".into());
        assert_eq!(run(&n, Some(Idle)), (SessionStatus::Idle, None));
        let mut n = s.clone();
        n.waiting_for = Some("input needed".into());
        assert_eq!(run(&n, Some(Idle)), (SessionStatus::Waiting, Some("input needed".into())));
        // No pane to look at: untouched.
        let mut n = s.clone();
        n.pane = None;
        assert_eq!(run(&n, Some(Idle)), (SessionStatus::Waiting, Some("permission: Bash".into())));
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

    /// A parked turn: the interactive session (in a pane, `busy`) hands the
    /// turn to a daemon-hosted worker (off `init`, `waiting`, no pane). The
    /// worker is kept because its parent parked it, and the task is Blocked
    /// on the interactive session's pane; it is not an agent.
    #[test]
    fn a_parked_turns_worker_is_kept_and_blocks_on_the_interactive_pane() {
        let mut parent = session(TASK, SessionStatus::Busy, "interactive", 10);
        parent.pid = 200;
        parent.pane = Some("%57".into());
        parent.parked_job_id = Some("2f32".into());
        let mut worker = session(TASK, SessionStatus::Waiting, "bg", 20);
        worker.pid = 900;
        worker.job_id = Some("2f32".into());
        worker.waiting_for = Some("permission prompt".into());
        let mut stray = session(TASK, SessionStatus::Waiting, "bg", 30);
        stray.pid = 901; // a worker whose parent is not in our panes
        stray.job_id = Some("other".into());
        let tree = vec![(200, 1), (900, 1), (901, 1)];
        let kept = in_panes(vec![parent, worker, stray], &[200], &tree);
        let pids: Vec<u32> = kept.iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![200, 900]);

        let folded = fold_parked(kept);
        assert_eq!(folded.len(), 1);
        let st = resolve_task_state(Path::new(TASK), &folded, QUIET);
        assert_eq!(st.status, TaskStatus::Blocked);
        assert_eq!(st.waiting_for.as_deref(), Some("permission prompt"));
        assert_eq!(st.changed, Some(at(20)));
        assert_eq!(st.pane.as_deref(), Some("%57"));
        assert_eq!((st.sessions, st.agents), (1, 0));
    }

    /// The interactive entry is frozen at `busy` from the moment it parked;
    /// the worker finishing is what makes the task Done, with the worker's
    /// timestamp as the age. A parked session with no worker reads idle.
    #[test]
    fn a_parked_sessions_own_status_is_never_read() {
        let mut parent = session(TASK, SessionStatus::Busy, "interactive", 10);
        parent.pid = 200;
        parent.pane = Some("%57".into());
        parent.parked_job_id = Some("2f32".into());
        let mut worker = session(TASK, SessionStatus::Idle, "bg", 40);
        worker.pid = 900;
        worker.job_id = Some("2f32".into());
        let st = resolve_task_state(Path::new(TASK), &fold_parked(vec![parent.clone(), worker]), QUIET);
        assert_eq!(st.status, TaskStatus::Done);
        assert_eq!(st.changed, Some(at(40)));
        assert_eq!(st.pane.as_deref(), Some("%57"));

        let alone = fold_parked(vec![parent]);
        assert_eq!(alone[0].status, SessionStatus::Idle);
        assert_eq!(resolve_task_state(Path::new(TASK), &alone, QUIET).status, TaskStatus::Done);

        // An unrelated bg agent with a job id nobody parked is untouched.
        let mut agent = session("/ws/tasks/foo/agent", SessionStatus::Busy, "bg", 5);
        agent.job_id = Some("zzzz".into());
        let plain = session(TASK, SessionStatus::Idle, "interactive", 1);
        let st = resolve_task_state(Path::new(TASK), &fold_parked(vec![plain, agent]), QUIET);
        assert_eq!((st.status, st.sessions, st.agents), (TaskStatus::Working, 2, 1));
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
