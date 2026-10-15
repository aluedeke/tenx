//! `tenx agent restart`: restart the coding agent in every open task window —
//! after an agent update, or a change to its config — without losing what tenx
//! shows. Only the agent's pane is respawned, so the window, its other panes
//! and its layout stay; the conversation is resumed by its session id, not
//! "the newest one in this directory"; and a task that was waiting on you
//! still is, for as long as it was.
//!
//! The rules here: when a task's agent may be restarted, what it is told once
//! it is back, and whether the new session takes over the old one's status
//! time. The binary (`cli::restart`) does the tmux and registry work.

use crate::status::TaskStatus;

/// Why a task's agent must not be restarted yet, or `None` to restart it now.
///
/// A turn that is running would be cut off, and a permission prompt can't
/// survive a restart at all — the agent cancels the pending tool call — so
/// both are waited out unless the caller said `--now`. The agent that runs the
/// restart itself is always busy (it is running us), so it never waits.
pub fn hold_reason(status: TaskStatus, waiting_for: Option<&str>, is_self: bool, now: bool) -> Option<String> {
    if now || is_self {
        return None;
    }
    match status {
        TaskStatus::Working => Some("working".to_string()),
        TaskStatus::Blocked => Some(format!("waiting on you: {}", waiting_for.unwrap_or("input needed"))),
        TaskStatus::Signaled | TaskStatus::Done | TaskStatus::Idle => None,
    }
}

/// What to type into a restarted agent so it carries on, if anything.
///
/// An agent restarted mid-turn (itself, or any with `--now`) lost that turn
/// and is told to pick it up again. A `tenx pr wait` the agent ran in the
/// background was a child of the old process and died with it, so the agent
/// is told to start it again. Everyone else is left alone: typing into an
/// agent that was done would start a turn nobody asked for.
pub fn resume_message(was: TaskStatus, is_self: bool, watching: &[u64]) -> Option<String> {
    let mut parts = Vec::new();
    if is_self || matches!(was, TaskStatus::Working | TaskStatus::Blocked) {
        parts.push(
            "tenx restarted this agent (`tenx agent restart`) while a turn was running. Continue where you left off."
                .to_string(),
        );
    }
    if !watching.is_empty() {
        let prs = watching.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", ");
        parts.push(format!("The restart also stopped your `tenx pr wait` for PR {prs}: start it again."));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The `statusUpdatedAt` (ms) the restarted session should carry, if any.
///
/// A task that was done is still done after the restart — the resumed session
/// is a quiet live one — but its fresh record says it went quiet just now, so
/// "waiting on you 3h" would read "just now". Carry the old time over while
/// the new session is still idle; once it has changed status, its own time is
/// the true one.
pub fn carried_status_time(was: TaskStatus, was_changed_ms: Option<u64>, new_status: &str) -> Option<u64> {
    (was == TaskStatus::Done && new_status == "idle").then_some(was_changed_ms).flatten()
}

// ── `tenx tmux restart` ───────────────────────────────────────────────────────

/// What a pane gets when `tenx tmux restart` rebuilds its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneRole {
    /// The task's agent: resumed by its session, or launched with the
    /// agent's continue rule when it never reported in.
    Agent,
    /// A program the layout started in the pane (`nvim TASK.md`): run again,
    /// exactly as reopening the task would.
    Program(String),
    /// A plain shell; whatever was typed into it is gone.
    Shell,
    /// `tenx internal agent-log`, following a background agent: left out,
    /// the watcher opens it again for an agent that is still running.
    AgentLog,
}

/// Decide a pane's [`PaneRole`] from what tmux started in it (unquoted, see
/// [`unquote_start_command`]), whether a live agent session reports from it,
/// and the task's agent binary.
pub fn pane_role(start_command: &str, has_session: bool, agent_bin: &str) -> PaneRole {
    if has_session {
        return PaneRole::Agent;
    }
    let start = start_command.trim();
    if start.contains("internal agent-log") {
        return PaneRole::AgentLog;
    }
    if start.is_empty() {
        return PaneRole::Shell;
    }
    let first = start.split_whitespace().next().unwrap_or_default();
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).trim_matches('\'').to_string();
    if !agent_bin.is_empty() && base(first) == base(agent_bin) {
        // The agent's own launch, which may carry a first prompt: running it
        // again would send that prompt a second time.
        return PaneRole::Agent;
    }
    PaneRole::Program(start.to_string())
}

/// `#{pane_start_command}` as tmux prints it — the command as one argument,
/// double-quoted with `"`, `\` and `$` escaped when it needs quoting — back
/// to the shell string that was started.
pub fn unquote_start_command(raw: &str) -> String {
    let raw = raw.trim();
    let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return raw.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A program running in a shell pane that the restart stops — listed for the
/// user, never started again by itself (a `git push` must not run twice).
/// `None` when the shell itself is in front.
pub fn stopped_program<'a>(role: &PaneRole, current_command: &'a str, shells: &[&str]) -> Option<&'a str> {
    if *role != PaneRole::Shell {
        return None;
    }
    let cur = current_command.trim().trim_start_matches('-');
    (!cur.is_empty() && !shells.contains(&cur)).then_some(current_command.trim())
}

/// Shell names a pane's `#{pane_current_command}` shows while it is idle.
pub const SHELLS: &[&str] = &["zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "elvish", "xonsh"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_roles() {
        assert_eq!(pane_role("", true, "claude"), PaneRole::Agent);
        assert_eq!(pane_role("claude --name 'x' 'do the thing'", false, "claude"), PaneRole::Agent);
        assert_eq!(pane_role("/opt/bin/claude --name 'x'", false, "claude"), PaneRole::Agent);
        assert_eq!(pane_role("nvim TASK.md", false, "claude"), PaneRole::Program("nvim TASK.md".into()));
        assert_eq!(pane_role("", false, "claude"), PaneRole::Shell);
        assert_eq!(pane_role("'/x/tenx' internal agent-log '/t' 12", false, "claude"), PaneRole::AgentLog);
    }

    #[test]
    fn start_commands_are_unquoted() {
        assert_eq!(unquote_start_command("\"claude --name 'x' --continue\""), "claude --name 'x' --continue");
        assert_eq!(unquote_start_command("nvim"), "nvim");
        assert_eq!(unquote_start_command("\"echo \\\"hi\\\" \\$HOME\""), "echo \"hi\" $HOME");
        assert_eq!(unquote_start_command(""), "");
    }

    #[test]
    fn only_a_program_in_a_shell_is_reported_stopped() {
        assert_eq!(stopped_program(&PaneRole::Shell, "zsh", SHELLS), None);
        assert_eq!(stopped_program(&PaneRole::Shell, "-zsh", SHELLS), None);
        assert_eq!(stopped_program(&PaneRole::Shell, "npm", SHELLS), Some("npm"));
        assert_eq!(stopped_program(&PaneRole::Program("nvim".into()), "nvim", SHELLS), None);
        assert_eq!(stopped_program(&PaneRole::Agent, "node", SHELLS), None);
    }

    #[test]
    fn running_turns_and_open_prompts_are_waited_out() {
        assert_eq!(hold_reason(TaskStatus::Working, None, false, false).as_deref(), Some("working"));
        assert_eq!(
            hold_reason(TaskStatus::Blocked, Some("permission: Bash"), false, false).as_deref(),
            Some("waiting on you: permission: Bash")
        );
        assert_eq!(
            hold_reason(TaskStatus::Blocked, None, false, false).as_deref(),
            Some("waiting on you: input needed")
        );
    }

    #[test]
    fn quiet_tasks_restart_at_once() {
        for s in [TaskStatus::Done, TaskStatus::Idle, TaskStatus::Signaled] {
            assert!(hold_reason(s, None, false, false).is_none(), "{s:?}");
        }
    }

    #[test]
    fn now_and_the_caller_itself_never_wait() {
        assert!(hold_reason(TaskStatus::Working, None, false, true).is_none());
        assert!(hold_reason(TaskStatus::Blocked, None, false, true).is_none());
        assert!(hold_reason(TaskStatus::Working, None, true, false).is_none());
    }

    #[test]
    fn only_an_interrupted_turn_or_a_lost_pr_wait_gets_a_message() {
        assert!(resume_message(TaskStatus::Done, false, &[]).is_none());
        assert!(resume_message(TaskStatus::Idle, false, &[]).is_none());
        assert!(resume_message(TaskStatus::Working, false, &[]).unwrap().contains("Continue where you left off"));
        assert!(resume_message(TaskStatus::Blocked, false, &[]).is_some());
        assert!(resume_message(TaskStatus::Done, true, &[]).is_some());
        let m = resume_message(TaskStatus::Done, false, &[12, 7]).unwrap();
        assert!(m.contains("PR #12, #7") && !m.contains("Continue"));
    }

    #[test]
    fn a_done_task_keeps_its_age_while_the_new_session_is_quiet() {
        assert_eq!(carried_status_time(TaskStatus::Done, Some(5), "idle"), Some(5));
        assert_eq!(carried_status_time(TaskStatus::Done, Some(5), "busy"), None);
        assert_eq!(carried_status_time(TaskStatus::Done, None, "idle"), None);
        assert_eq!(carried_status_time(TaskStatus::Working, Some(5), "idle"), None);
    }
}
