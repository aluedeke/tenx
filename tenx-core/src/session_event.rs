//! What a coding agent's hook/extension event *means* for a task's session
//! record — pure logic, no I/O.
//!
//! tenx keeps one session record per live agent process (`~/.config/tenx/
//! sessions/<pid>.json`) and every agent feeds it the same way: the agent's own
//! hook (Codex, Claude Code) or extension (pi) fires on a lifecycle event, and
//! tenx maps that event to an absolute session status here. The binary does the
//! reading, pid resolution and file writing (`cli::session_event`); this module
//! only decides Set / Delete / Ignore.
//!
//! Absolute, not a delta: each event names the status the session is now in, so
//! the mapping is stateless and a missed event self-corrects on the next one.
//! `Ignore` means "this event says nothing about the status" (an idle-timer
//! notification, a subagent's tool call) — leave the record as it was.

use crate::status::SessionStatus;

/// What to do to a session's record in response to one hook event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    /// Overwrite the record's status (and its waiting reason — `None` clears it).
    Set { status: SessionStatus, waiting_for: Option<String> },
    /// The session ended — remove the record.
    Delete,
    /// This event carries no status information — leave the record untouched.
    Ignore,
}

fn busy() -> SessionAction {
    SessionAction::Set { status: SessionStatus::Busy, waiting_for: None }
}
fn idle() -> SessionAction {
    SessionAction::Set { status: SessionStatus::Idle, waiting_for: None }
}
fn waiting(reason: impl Into<String>) -> SessionAction {
    SessionAction::Set { status: SessionStatus::Waiting, waiting_for: Some(reason.into()) }
}

/// Claude Code tools whose "use" is really a blocking dialog to you, so a
/// `PreToolUse` for them means waiting, not working. (A `PermissionRequest` is
/// the general permission dialog; these two are tools that *are* the question.)
const CLAUDE_DIALOG_TOOLS: &[&str] = &["AskUserQuestion", "ExitPlanMode"];

/// Map a Claude Code hook event to a session action.
///
/// - `event` is `hook_event_name`.
/// - `tool_name` is set for the tool events.
/// - `notification_type` is the `Notification` event's type.
/// - `message` is the `Notification`/`Elicitation` human text, used as the
///   waiting reason when present.
/// - `is_subagent` is true when the payload carries an `agent_id` — a subagent's
///   events must never move the top-level session's record.
///
/// The one-turn latch the v0 hooks suffered (a "waiting" that nothing cleared)
/// can't happen here: every dialog outcome is followed by an event that sets a
/// non-waiting status — approve → `PostToolUse`, deny → `PermissionDenied`,
/// "tell Claude what to do differently" → `UserPromptSubmit`, and the turn's end
/// → `Stop`.
pub fn claude_action(
    event: &str,
    tool_name: Option<&str>,
    notification_type: Option<&str>,
    message: Option<&str>,
    is_subagent: bool,
) -> SessionAction {
    if is_subagent {
        return SessionAction::Ignore;
    }
    match event {
        "SessionStart" => idle(),
        "UserPromptSubmit" => busy(),
        "PreToolUse" => {
            if tool_name.is_some_and(|t| CLAUDE_DIALOG_TOOLS.contains(&t)) {
                waiting("input needed")
            } else {
                busy()
            }
        }
        "PermissionRequest" => waiting(format!("permission: {}", tool_name.unwrap_or("tool"))),
        "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => busy(),
        "Notification" => match notification_type {
            Some("permission_prompt") => waiting("permission"),
            Some("agent_needs_input") => waiting("input needed"),
            Some("elicitation_dialog") | Some("elicitation_url_dialog") => {
                waiting(message.unwrap_or("input needed").to_string())
            }
            // idle_prompt (a 30 s idle timer), auth_success, the elicitation
            // completions, quota notices — none of these change the status.
            _ => SessionAction::Ignore,
        },
        "Elicitation" => waiting("input needed"),
        "ElicitationResult" => busy(),
        "Stop" | "StopFailure" => idle(),
        "SessionEnd" => SessionAction::Delete,
        _ => SessionAction::Ignore,
    }
}

/// Map a Codex CLI hook event to a session action.
///
/// - `event` is `hook_event_name`.
/// - `command` is `tool_input.command` (the shell an approval is gating), used
///   as the waiting reason so the overlay shows *what* is awaiting approval.
/// - `is_subagent` guards subagent lifecycle events.
///
/// Codex has no separate "dialog tool" like Claude's plan mode; the one waiting
/// state is `PermissionRequest` (its approval prompt). `PreToolUse`/`PostToolUse`
/// bracket a tool that Codex is running, so both read as busy — and `PostToolUse`
/// after an approved command is exactly what clears the waiting state.
pub fn codex_action(event: &str, command: Option<&str>, is_subagent: bool) -> SessionAction {
    if is_subagent {
        return SessionAction::Ignore;
    }
    match event {
        "SessionStart" => idle(),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => busy(),
        "PermissionRequest" => match command {
            Some(cmd) if !cmd.is_empty() => waiting(format!("approval: {cmd}")),
            _ => waiting("approval"),
        },
        "Stop" | "Interrupt" => idle(),
        "SessionEnd" => SessionAction::Delete,
        _ => SessionAction::Ignore,
    }
}

/// Map a pi extension event to a session action.
///
/// pi reports in-process (`~/.pi/agent/extensions/tenx.ts`): `agent_start` when
/// a run begins, `agent_settled` when pi will not continue on its own, and
/// `ui_prompt_start`/`ui_prompt_end` around a blocking prompt to you — the only
/// signal pi gives for "waiting on the user". `message` is the prompt title.
pub fn pi_action(event: &str, message: Option<&str>) -> SessionAction {
    match event {
        "session_start" => idle(),
        "agent_start" => busy(),
        "ui_prompt_start" => waiting(message.filter(|m| !m.is_empty()).unwrap_or("input needed").to_string()),
        "ui_prompt_end" => busy(),
        "agent_settled" => idle(),
        "session_shutdown" => SessionAction::Delete,
        _ => SessionAction::Ignore,
    }
}

/// Merge tenx's command hook into a JSON hooks config in place, idempotently.
///
/// Works for both Claude Code's `~/.claude/settings.json` and Codex's
/// `~/.codex/hooks.json`: each keeps a top-level `hooks` object mapping an event
/// name to an array of `{ matcher?, hooks: [{ type, command, timeout }] }`
/// groups. For every event in `events` this ensures a group exists whose
/// `hooks[].command` equals `command`, without disturbing other events or the
/// user's own hooks. Returns whether anything changed (so the caller only
/// rewrites the file when needed).
pub fn ensure_command_hooks(root: &mut serde_json::Value, events: &[&str], command: &str, timeout: u64) -> bool {
    use serde_json::json;
    if !root.is_object() {
        *root = json!({});
    }
    let obj = root.as_object_mut().expect("just ensured object");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().expect("just ensured object");
    let mut changed = false;
    for event in events {
        let arr = hooks.entry((*event).to_string()).or_insert_with(|| json!([]));
        if !arr.is_array() {
            *arr = json!([]);
        }
        let arr = arr.as_array_mut().expect("just ensured array");
        let present = arr.iter().any(|g| group_has_command(g, command));
        if !present {
            arr.push(json!({ "hooks": [{ "type": "command", "command": command, "timeout": timeout }] }));
            changed = true;
        }
    }
    changed
}

/// Whether any event in `root`'s hooks already runs `command`.
pub fn has_command_hook(root: &serde_json::Value, command: &str) -> bool {
    root.get("hooks")
        .and_then(|h| h.as_object())
        .is_some_and(|events| events.values().any(|arr| arr.as_array().is_some_and(|groups| groups.iter().any(|g| group_has_command(g, command)))))
}

fn group_has_command(group: &serde_json::Value, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|hs| hs.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ensure_command_hooks_is_idempotent_and_preserves_foreign() {
        let mut root = json!({
            "model": "x",
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "other" }] }] }
        });
        let cmd = "tenx internal session-event --agent claude";
        assert!(ensure_command_hooks(&mut root, &["SessionStart", "Stop"], cmd, 5));
        // Second run changes nothing.
        assert!(!ensure_command_hooks(&mut root, &["SessionStart", "Stop"], cmd, 5));
        assert!(has_command_hook(&root, cmd));
        // Foreign keys and the user's own Stop hook survive.
        assert_eq!(root["model"], json!("x"));
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "kept the user's Stop hook, added ours");
        assert!(stop.iter().any(|g| group_has_command(g, "other")));
        assert!(stop.iter().any(|g| group_has_command(g, cmd)));
    }

    #[test]
    fn ensure_command_hooks_builds_from_empty() {
        let mut root = json!(null);
        assert!(ensure_command_hooks(&mut root, &["SessionStart"], "cmd", 5));
        assert!(has_command_hook(&root, "cmd"));
        assert!(!has_command_hook(&json!({}), "cmd"));
    }

    fn status(a: &SessionAction) -> Option<SessionStatus> {
        match a {
            SessionAction::Set { status, .. } => Some(*status),
            _ => None,
        }
    }

    #[test]
    fn lifecycle_maps_to_absolute_status() {
        assert_eq!(status(&claude_action("SessionStart", None, None, None, false)), Some(SessionStatus::Idle));
        assert_eq!(status(&claude_action("UserPromptSubmit", None, None, None, false)), Some(SessionStatus::Busy));
        assert_eq!(status(&claude_action("PreToolUse", Some("Bash"), None, None, false)), Some(SessionStatus::Busy));
        assert_eq!(status(&claude_action("Stop", None, None, None, false)), Some(SessionStatus::Idle));
        assert_eq!(claude_action("SessionEnd", None, None, None, false), SessionAction::Delete);
        assert_eq!(claude_action("MessageDisplay", None, None, None, false), SessionAction::Ignore);
    }

    #[test]
    fn dialog_tools_and_permission_are_waiting_with_reason() {
        let a = claude_action("PreToolUse", Some("ExitPlanMode"), None, None, false);
        assert_eq!(status(&a), Some(SessionStatus::Waiting));
        let a = claude_action("PermissionRequest", Some("Bash"), None, None, false);
        assert_eq!(a, SessionAction::Set { status: SessionStatus::Waiting, waiting_for: Some("permission: Bash".into()) });
    }

    #[test]
    fn notification_types_split_waiting_from_noise() {
        assert_eq!(status(&claude_action("Notification", None, Some("permission_prompt"), None, false)), Some(SessionStatus::Waiting));
        assert_eq!(status(&claude_action("Notification", None, Some("agent_needs_input"), None, false)), Some(SessionStatus::Waiting));
        assert_eq!(
            claude_action("Notification", None, Some("elicitation_dialog"), Some("Pick one"), false),
            SessionAction::Set { status: SessionStatus::Waiting, waiting_for: Some("Pick one".into()) }
        );
        // idle_prompt fires after 30 s of no input — it must not flip the status.
        assert_eq!(claude_action("Notification", None, Some("idle_prompt"), None, false), SessionAction::Ignore);
        assert_eq!(claude_action("Notification", None, Some("auth_success"), None, false), SessionAction::Ignore);
    }

    #[test]
    fn every_dialog_outcome_clears_waiting() {
        // The v0 latch: once "waiting", nothing moved it back. Each real outcome now does.
        assert_eq!(status(&claude_action("PostToolUse", Some("Bash"), None, None, false)), Some(SessionStatus::Busy)); // approve
        assert_eq!(status(&claude_action("PermissionDenied", Some("Bash"), None, None, false)), Some(SessionStatus::Busy)); // deny
        assert_eq!(status(&claude_action("UserPromptSubmit", None, None, None, false)), Some(SessionStatus::Busy)); // redirect
        assert_eq!(status(&claude_action("Stop", None, None, None, false)), Some(SessionStatus::Idle)); // end of turn
    }

    #[test]
    fn subagent_events_never_touch_the_session() {
        assert_eq!(claude_action("PreToolUse", Some("Bash"), None, None, true), SessionAction::Ignore);
        assert_eq!(claude_action("Stop", None, None, None, true), SessionAction::Ignore);
    }

    #[test]
    fn codex_map_covers_the_lifecycle_and_approval() {
        assert_eq!(status(&codex_action("SessionStart", None, false)), Some(SessionStatus::Idle));
        assert_eq!(status(&codex_action("UserPromptSubmit", None, false)), Some(SessionStatus::Busy));
        assert_eq!(status(&codex_action("PreToolUse", None, false)), Some(SessionStatus::Busy));
        assert_eq!(
            codex_action("PermissionRequest", Some("touch x"), false),
            SessionAction::Set { status: SessionStatus::Waiting, waiting_for: Some("approval: touch x".into()) }
        );
        // Approving runs the command → PostToolUse → busy (clears waiting).
        assert_eq!(status(&codex_action("PostToolUse", None, false)), Some(SessionStatus::Busy));
        assert_eq!(status(&codex_action("Stop", None, false)), Some(SessionStatus::Idle));
        assert_eq!(status(&codex_action("Interrupt", None, false)), Some(SessionStatus::Idle));
        assert_eq!(codex_action("SessionEnd", None, false), SessionAction::Delete);
        assert_eq!(codex_action("PreToolUse", None, true), SessionAction::Ignore);
    }

    #[test]
    fn pi_map_covers_run_and_prompt_lifecycle() {
        assert_eq!(status(&pi_action("session_start", None)), Some(SessionStatus::Idle));
        assert_eq!(status(&pi_action("agent_start", None)), Some(SessionStatus::Busy));
        assert_eq!(
            pi_action("ui_prompt_start", Some("Overwrite file?")),
            SessionAction::Set { status: SessionStatus::Waiting, waiting_for: Some("Overwrite file?".into()) }
        );
        assert_eq!(status(&pi_action("ui_prompt_end", None)), Some(SessionStatus::Busy));
        assert_eq!(status(&pi_action("agent_settled", None)), Some(SessionStatus::Idle));
        assert_eq!(pi_action("session_shutdown", None), SessionAction::Delete);
        assert_eq!(pi_action("turn_start", None), SessionAction::Ignore);
    }
}
