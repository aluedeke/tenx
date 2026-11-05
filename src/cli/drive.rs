//! Driving another task's agent from the outside: `task send` types a message
//! into its pane, `task wait` blocks until its turn is over, `task output`
//! prints what it said. Together with `task new --prompt` these are what an
//! orchestrating session (the `/orchestrate` skill, in the adhoc
//! workspace) is made of — and they work the same typed by a person.
//!
//! A message is pasted into the live agent, not handed to a second headless
//! process: the window stays the one source of truth, you can watch every
//! exchange and take over mid-turn. The rules — when a wait is over, when a
//! paste must not happen, which lines are the answer — are
//! `tenx_core::orchestrate`.

use anyhow::{bail, Result};
use std::path::Path;
use std::time::{Duration, Instant};

use tenx_core::orchestrate::{self, WaitOutcome};

use crate::workspace::{self, TaskState};

/// The task's state right now, from the registry and the window's bell —
/// the same resolve every front end does.
fn state(task_dir: &Path) -> TaskState {
    let sessions = workspace::sessions::sessions();
    let signals = crate::tmux::signals();
    workspace::resolve_task_state(task_dir, &sessions, &signals)
}

/// Poll until the task's agent reports a turn under way (or a dialog), up to
/// `limit`. What `send` and `new --prompt` end with, so a `wait` right after
/// them waits for *this* turn instead of returning on the quiet before it.
/// Best-effort: a turn short enough to start and finish between two polls
/// just runs out the limit, and an agent without tenx's integration (no
/// registry record ever appears) gets `START_LIMIT` to show up, then nothing.
pub fn await_turn(task_dir: &Path, limit: Duration) {
    let start = Instant::now();
    while start.elapsed() < limit {
        let st = state(task_dir);
        if st.status == workspace::TaskStatus::Working || st.status == workspace::TaskStatus::Blocked {
            return;
        }
        if st.sessions == 0 && start.elapsed() >= START_LIMIT {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// How long a task's agent gets to register at all before `await_turn` stops
/// expecting it to, and how long a sent message gets to show up as a turn.
const START_LIMIT: Duration = Duration::from_secs(5);
pub const TURN_LIMIT: Duration = Duration::from_secs(15);

/// `tenx task send`: deliver `text` to the task's agent as a message.
///
/// A task without a window gets one, with the message as its launch prompt
/// (the agent starts on it; nothing is typed). An open one has the text
/// pasted into its agent's pane and Enter pressed — unless the agent is
/// sitting on a dialog, where typed text would answer it (`--force` to do it
/// anyway).
pub fn send(ws_dir: Option<&str>, task: &str, text: &str, force: bool) -> Result<()> {
    let (ws, slug) = crate::cli::task::resolve_task(ws_dir, task)?;
    if send_in(&ws, &slug, text, force, true)? == Sent::Opened {
        println!("opened '{}' with the message", ws.find_task(&slug)?.display_name);
    }
    Ok(())
}

/// How [`send_in`] delivered a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// The task had no window: it was opened with the message as its prompt.
    Opened,
    /// Pasted into the running agent and submitted.
    Typed,
}

/// [`send`] for an explicit workspace and slug — also `tenx web`'s
/// `tenx_send_message` tool. The window opens detached, so a message from
/// elsewhere never moves the session's current window.
///
/// `await_start`: return only once the agent has started on it (or 15 s
/// passed), so a `task wait` right after sees this turn, not the last one.
/// `tenx web`'s MCP send returns at once and does that wait in `tenx_wait`.
pub fn send_in(ws: &workspace::Workspace, slug: &str, text: &str, force: bool, await_start: bool) -> Result<Sent> {
    let text = text.trim();
    if text.is_empty() {
        bail!("nothing to send");
    }
    if !crate::tmux::server_running() {
        bail!("the '{}' session isn't running — run 'tenx' to start it first", crate::tmux::SESSION);
    }
    let task = ws.find_task(slug)?;

    let Some(window) = crate::tmux::find_task_window(slug, &task.path)? else {
        std::fs::write(task.path.join(crate::cli::task::PROMPT_FILE), text)?;
        crate::cli::task::open_window(ws, slug, true)?;
        if await_start {
            await_turn(&task.path, TURN_LIMIT);
        }
        return Ok(Sent::Opened);
    };

    let st = state(&task.path);
    if !force && let Some(why) = orchestrate::send_refusal(st.status, st.waiting_for.as_deref()) {
        bail!(why);
    }
    // The agent's own pane, from its registry record; the first pane of the
    // window (where every built-in layout puts the agent) when there is none.
    let pane = st.pane.unwrap_or_else(|| format!("{}.0", window.id));
    crate::tmux::paste_text(&pane, text)?;
    // Let the TUI take the paste in before the Enter that submits it: sent
    // back to back, Enter can arrive inside the paste and become a newline.
    std::thread::sleep(Duration::from_millis(200));
    crate::tmux::send_keys(&pane, "Enter")?;
    // An agent that has never reported in won't report this turn either.
    if await_start && st.sessions > 0 {
        await_turn(&task.path, TURN_LIMIT);
    }
    Ok(Sent::Typed)
}

/// `tenx task wait`: block until the task's turn is over, then print its
/// status. Exits 0 when it settled, 2 when it stopped on something that needs
/// an answer (with the reason), 3 on `timeout` — `tenx_core::orchestrate`.
pub fn wait(ws_dir: Option<&str>, task: &str, timeout: Duration) -> Result<()> {
    let (ws, slug) = crate::cli::task::resolve_task(ws_dir, task)?;
    let task = ws.find_task(&slug)?;
    let (outcome, st) = wait_in(&task.path, timeout);
    let why = if outcome == WaitOutcome::TimedOut { None } else { st.waiting_for.as_deref() };
    finish(outcome, &task.display_name, st.status.token(), why)
}

/// Poll the task's state until its turn is over or `timeout` runs out: how
/// it ended, and the state it ended in.
pub fn wait_in(task_dir: &Path, timeout: Duration) -> (WaitOutcome, TaskState) {
    let start = Instant::now();
    loop {
        let st = state(task_dir);
        if let Some(outcome) = orchestrate::wait_outcome(st.status) {
            return (outcome, st);
        }
        if start.elapsed() >= timeout {
            return (WaitOutcome::TimedOut, st);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Answer the permission prompt the task's agent shows: `A`/`D` in the
/// column, and `tenx web`'s `tenx_answer_permission`. The registry is re-read
/// and the pane re-captured right before the key goes out, and the key goes
/// only when the session waits on a permission *and* the dialog is on screen
/// — anywhere else Enter or Escape means something else
/// (`tenx_core::dialog`). `Err` says why nothing was sent.
pub fn answer(task_dir: &Path, title: &str, signals: &workspace::Signals, answer: tenx_core::dialog::Answer) -> Result<()> {
    let sessions = workspace::sessions::sessions();
    let state = workspace::resolve_task_state(task_dir, &sessions, signals);
    if state.status != workspace::TaskStatus::Blocked {
        bail!("'{title}' is not waiting on a prompt");
    }
    if !state.waiting_for.as_deref().is_some_and(tenx_core::dialog::is_permission_reason) {
        let reason = state.waiting_for.unwrap_or_default();
        bail!("'{title}' is waiting on {reason} — open it to answer (⏎)");
    }
    let Some(pane) = state.pane else {
        bail!("'{title}': no pane known for its session");
    };
    let capture = crate::tmux::capture_pane(&pane)?;
    if !tenx_core::dialog::permission_dialog_visible(&capture) {
        bail!("'{title}': no permission dialog on screen — open it to check (⏎)");
    }
    crate::tmux::send_keys(&pane, answer.key())
}

fn finish(outcome: WaitOutcome, title: &str, status: &str, why: Option<&str>) -> Result<()> {
    let message = match (outcome, why) {
        (WaitOutcome::Settled, _) => {
            println!("{title}: {status}");
            return Ok(());
        }
        (WaitOutcome::NeedsYou, Some(why)) => format!("{title}: {status} — {why}"),
        (WaitOutcome::NeedsYou, None) => format!("{title}: {status}"),
        (WaitOutcome::TimedOut, _) => format!("{title}: still {status} — re-run to keep waiting"),
    };
    Err(crate::cli::secrets::Exit { code: outcome.exit_code(), message }.into())
}

/// `tenx task output`: what the task's agent said since the last prompt, read
/// from its transcript — the answer to a `send`, once `wait` says it's done.
/// `json` adds the prompt and the task's status, for a caller that parses.
pub fn output(ws_dir: Option<&str>, task: &str, json: bool) -> Result<()> {
    let (ws, slug) = crate::cli::task::resolve_task(ws_dir, task)?;
    let task = ws.find_task(&slug)?;
    let turn = last_turn(&ws, &task)?;
    if json {
        let st = state(&task.path);
        println!(
            "{}",
            serde_json::json!({
                "task": task.name,
                "title": task.display_name,
                "status": st.status.token(),
                "waiting_for": st.waiting_for,
                "prompt": turn.prompt,
                "replies": turn.replies,
            })
        );
    } else if turn.replies.is_empty() {
        eprintln!("(no reply yet)");
    } else {
        println!("{}", turn.replies.join("\n\n"));
    }
    Ok(())
}

/// The last exchange of the task's agent, from its transcript: the session
/// that runs in the task directory itself — not a background agent in a
/// subdirectory — and its agent; with none live, the newest transcript for
/// the directory, in the task's configured agent's format. Also what
/// `tenx web` reads aloud.
pub(crate) fn last_turn(ws: &workspace::Workspace, task: &workspace::Task) -> Result<orchestrate::Turn> {
    let cwd = task.path.to_string_lossy().into_owned();
    let sessions = workspace::sessions::sessions();
    let live = sessions.iter().find(|s| match &s.task_dir {
        Some(dir) => *dir == task.path,
        None => s.cwd == task.path && s.kind != "bg",
    });
    let agent = match live {
        Some(s) if !s.agent.is_empty() => s.agent.clone(),
        _ => crate::agent::agent_for(ws, &task.path).as_str().to_string(),
    };
    let session_id = live.and_then(|s| s.session_id.as_deref());
    let Some(path) = crate::cli::agentlog::locate_transcript(&agent, &cwd, session_id) else {
        bail!("'{}' has no transcript yet", task.display_name);
    };
    let text = std::fs::read_to_string(&path)?;
    let entries: Vec<_> = text.lines().filter_map(|l| tenx_core::transcript::parse_line(&agent, l)).collect();
    Ok(orchestrate::last_turn(&entries))
}
