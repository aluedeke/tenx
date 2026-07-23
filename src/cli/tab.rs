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

/// Long-lived renderer for the 1-line header pane at the top of a task tab:
/// shows the task's display name and live Claude status, refreshing every
/// second (cheap: two small file reads per tick). Runs until zellij closes the
/// pane with the tab. Name and status are re-read each tick so TASK.md renames
/// and hook-driven state changes show up without any signaling.
pub fn header() -> Result<()> {
    use crate::workspace::TaskStatus as S;
    use std::io::Write as _;

    let dir = std::env::current_dir()?;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[?25l"); // hide the cursor in the sliver pane
    loop {
        let name = crate::workspace::read_task_display_name(&dir);
        let (status, changed) = crate::workspace::read_task_status(&dir);
        let (badge, color) = match status {
            S::Working => ("▷ working", "\x1b[36m"),
            S::Blocked => ("\u{1F4AC} blocked", "\x1b[1;33m"),
            S::Done => ("\u{2705} done", "\x1b[32m"),
            S::Failed => ("\u{26A0}\u{FE0F} failed", "\x1b[1;31m"),
            S::Idle => ("· idle", "\x1b[90m"),
        };
        // Age only for resting states — mirrors the overlay.
        let age = changed
            .filter(|_| matches!(status, S::Blocked | S::Done | S::Failed))
            .map(|t| format!(" {}", crate::workspace::format_age(t)))
            .unwrap_or_default();
        let _ = write!(out, "\r\x1b[2K \x1b[1m{name}\x1b[0m  {color}{badge}{age}\x1b[0m");
        let _ = out.flush();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
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
