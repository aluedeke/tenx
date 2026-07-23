use anyhow::Result;
use serde_json::Value;
use std::io::Read;

/// Handle a Claude Code hook event, invoked from the task cwd with the hook's
/// JSON payload on stdin. Maps the event to a task state and mirrors it into the
/// `.tenx-status` file the overlay reads. Hooks fire on every turn, so this must
/// stay cheap and never fail the hook — any parse/IO problem exits 0.
///
/// Event → state mapping (phase 1 hook set):
///   UserPromptSubmit, PreToolUse, PostToolUse → working
///   Notification (permission/idle/needs-input) → blocked
///   Stop                                       → done
///   StopFailure                                → failed
///   SessionStart                               → idle
///   SessionEnd                                 → clear the status file
/// Any other event / notification leaves the current state untouched.
pub fn event() -> Result<()> {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let json: Value = serde_json::from_str(&buf).unwrap_or(Value::Null);

    let event = json
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");

    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => set_state("working"),
        "Notification" => {
            // Only the "needs you" notification types map to blocked; others
            // (auth_success, elicitation_complete, …) leave the state alone.
            let kind = json
                .get("notification_type")
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches!(kind, "permission_prompt" | "idle_prompt" | "agent_needs_input") {
                set_state("blocked");
            }
        }
        "Stop" => set_state("done"),
        "StopFailure" => set_state("failed"),
        "SessionStart" => set_state("idle"),
        "SessionEnd" => clear_state(),
        _ => {}
    }

    Ok(())
}

/// Write a state token to `.tenx-status` in the current (task) directory.
fn set_state(state: &str) {
    if let Ok(dir) = std::env::current_dir() {
        let _ = std::fs::write(dir.join(".tenx-status"), state);
    }
}

/// Remove `.tenx-status` (session ended / no signal).
fn clear_state() {
    if let Ok(dir) = std::env::current_dir() {
        let _ = std::fs::remove_file(dir.join(".tenx-status"));
    }
}
