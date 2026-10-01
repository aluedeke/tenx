//! What one agent session needs to drive another's: a title for a session
//! started from a bare question (`tenx ask`), when a `task wait` is over, when
//! a `task send` must not type into a pane, and which part of a transcript is
//! the answer to the last prompt (`task output`).
//!
//! The binary does the polling, the pasting and the file reading; the rules
//! are here.

use crate::status::TaskStatus;
use crate::transcript::{Entry, Role};

/// Longest title `ask_title` produces, in characters (before the ellipsis).
pub const ASK_TITLE_MAX: usize = 60;

/// A task title for a session started from `prompt`: its first non-blank
/// line, whitespace collapsed, cut at a word boundary to [`ASK_TITLE_MAX`]
/// characters with an ellipsis when anything was cut. Empty for a prompt with
/// no visible text — the caller then needs a title from somewhere else.
pub fn ask_title(prompt: &str) -> String {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        let extra = if out.is_empty() { 0 } else { 1 } + w.chars().count();
        if out.chars().count() + extra > ASK_TITLE_MAX {
            if out.is_empty() {
                // A single word longer than the limit: cut inside it.
                out = w.chars().take(ASK_TITLE_MAX).collect();
            }
            out.push('…');
            return out;
        }
        if i > 0 {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// How a `task wait` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The turn is over (or nothing is running): read the output.
    Settled,
    /// The task stopped on something only a human (or its orchestrator, with
    /// `task send`) can answer: a dialog, a question, a bell.
    NeedsYou,
    /// The timeout passed with the turn still running.
    TimedOut,
}

impl WaitOutcome {
    /// The process exit code `task wait` reports it with. 1 is left for
    /// ordinary errors (no such task), as everywhere else in tenx.
    pub fn exit_code(self) -> i32 {
        match self {
            WaitOutcome::Settled => 0,
            WaitOutcome::NeedsYou => 2,
            WaitOutcome::TimedOut => 3,
        }
    }
}

/// Whether a task in `status` is still worth waiting on, and if not, how the
/// wait ends. `None` = keep polling.
pub fn wait_outcome(status: TaskStatus) -> Option<WaitOutcome> {
    match status {
        TaskStatus::Working => None,
        TaskStatus::Blocked | TaskStatus::Signaled => Some(WaitOutcome::NeedsYou),
        TaskStatus::Done | TaskStatus::Idle => Some(WaitOutcome::Settled),
    }
}

/// Why `task send` must not paste into a task's agent right now, if it must
/// not. A blocked session has a dialog on screen — a permission prompt, a
/// multiple-choice question — and typed text lands *in the dialog*: a stray
/// `y` or Enter answers it. Those are answered deliberately (`A`/`D` in the
/// column), never as a side effect of a message.
pub fn send_refusal(status: TaskStatus, waiting_for: Option<&str>) -> Option<String> {
    match status {
        TaskStatus::Blocked => Some(match waiting_for {
            Some(r) if !r.is_empty() => format!("the task is waiting on a dialog ({r}) — answer it first, or pass --force"),
            _ => "the task is waiting on a dialog — answer it first, or pass --force".to_string(),
        }),
        _ => None,
    }
}

/// The last exchange in a transcript: the most recent prompt and everything
/// the assistant said after it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Turn {
    pub prompt: Option<String>,
    /// The assistant's prose after the prompt, one item per message, empty
    /// ones (pure tool calls) left out.
    pub replies: Vec<String>,
}

/// Split the last turn out of a parsed transcript. A user entry with no text
/// is a tool result (Claude records those as `user` lines), not a prompt, so
/// it doesn't start a turn.
pub fn last_turn(entries: &[Entry]) -> Turn {
    let start = entries.iter().rposition(|e| e.role == Role::User && !e.text.is_empty());
    let prompt = start.map(|i| entries[i].text.clone());
    let from = start.map_or(0, |i| i + 1);
    let replies = entries[from..]
        .iter()
        .filter(|e| e.role == Role::Assistant && !e.text.is_empty())
        .map(|e| e.text.clone())
        .collect();
    Turn { prompt, replies }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: Role, text: &str) -> Entry {
        Entry { hm: String::new(), iso: None, role, text: text.into(), tools: vec![], title: None }
    }

    #[test]
    fn ask_title_takes_the_first_line_and_cuts_on_a_word() {
        assert_eq!(ask_title("  \n how do  I rebase?\nmore"), "how do I rebase?");
        let long = "why does the watcher keep a pidfile instead of a lock and what happens when it dies";
        let t = ask_title(long);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= ASK_TITLE_MAX + 1);
        assert!(long.starts_with(t.trim_end_matches('…')));
        assert_eq!(ask_title(&"x".repeat(80)).chars().count(), ASK_TITLE_MAX + 1);
        assert_eq!(ask_title("   "), "");
    }

    #[test]
    fn wait_ends_on_anything_but_working() {
        assert_eq!(wait_outcome(TaskStatus::Working), None);
        assert_eq!(wait_outcome(TaskStatus::Done), Some(WaitOutcome::Settled));
        assert_eq!(wait_outcome(TaskStatus::Idle), Some(WaitOutcome::Settled));
        assert_eq!(wait_outcome(TaskStatus::Blocked), Some(WaitOutcome::NeedsYou));
        assert_eq!(wait_outcome(TaskStatus::Signaled), Some(WaitOutcome::NeedsYou));
        assert_eq!(WaitOutcome::TimedOut.exit_code(), 3);
    }

    #[test]
    fn send_refuses_only_a_blocked_task() {
        assert!(send_refusal(TaskStatus::Blocked, Some("permission")).unwrap().contains("permission"));
        assert!(send_refusal(TaskStatus::Blocked, None).is_some());
        for s in [TaskStatus::Working, TaskStatus::Done, TaskStatus::Idle, TaskStatus::Signaled] {
            assert_eq!(send_refusal(s, None), None);
        }
    }

    #[test]
    fn last_turn_skips_tool_results_and_silent_messages() {
        let entries = vec![
            entry(Role::User, "first"),
            entry(Role::Assistant, "old answer"),
            entry(Role::User, "second"),
            entry(Role::Assistant, ""),
            entry(Role::User, ""), // a tool result
            entry(Role::Assistant, "looking"),
            entry(Role::Other, "x"),
            entry(Role::Assistant, "done"),
        ];
        let t = last_turn(&entries);
        assert_eq!(t.prompt.as_deref(), Some("second"));
        assert_eq!(t.replies, vec!["looking", "done"]);
        assert_eq!(last_turn(&[]), Turn::default());
    }
}
