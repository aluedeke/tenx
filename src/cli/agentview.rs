//! `tenx internal open-agent <session-pid> <label>`: open a subagent in Claude
//! Code's own agent view, in its session's pane — what ⏎ on a subagent line in
//! the column does (`tui::client`), as a command for scripts and for testing
//! against a tmux server of one's own (`TENX_TMUX_SOCKET`).
//!
//! Claude Code offers no way to name an agent from outside, so this presses
//! keys in the pane the way a finger would; `tenx_core::agent_panel` has the
//! panel's shape and decides each key.

/// Walk Claude Code's agent panel in the pane of session `session_pid` to the
/// row showing `label`, and open it (`tenx_core::agent_panel` decides each
/// key from a fresh capture). Returns the pane on success. Refuses while a
/// permission dialog is up — `↓` would move its choice — and leaves the panel
/// as it found it when the row isn't there.
pub fn open_in_claude(session_pid: u32, label: &str) -> Result<String, String> {
    use tenx_core::agent_panel::{next_step, selected_row, view_open, PanelStep, MAX_PRESSES};
    let pane = crate::workspace::sessions::sessions()
        .into_iter()
        .find(|s| s.pid == session_pid)
        .and_then(|s| s.pane)
        .ok_or_else(|| "no pane known for its session".to_string())?;
    let capture = |p: &str| crate::tmux::capture_pane(p).map_err(|e| e.to_string());
    let key = |k: &str| crate::tmux::send_keys(&pane, k).map_err(|e| e.to_string());
    // Claude redraws within a frame or two of a key; wait for that, not longer.
    let settle = |before: &str| -> Result<String, String> {
        for _ in 0..16 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let now = capture(&pane)?;
            if now != before {
                std::thread::sleep(std::time::Duration::from_millis(50));
                return capture(&pane);
            }
        }
        capture(&pane)
    };
    let mut now = capture(&pane)?;
    if tenx_core::dialog::permission_dialog_visible(&now) {
        return Err("a dialog is open in its pane".into());
    }
    let mut previous: Option<String> = None;
    for presses in 0..=MAX_PRESSES {
        match next_step(&now, label, previous.as_deref(), presses) {
            PanelStep::Down => {
                previous = selected_row(&now);
                key("Down")?;
                now = settle(&now)?;
            }
            PanelStep::Open => {
                key("Enter")?;
                now = settle(&now)?;
                return if view_open(&now, label) { Ok(pane) } else { Err(format!("'{label}' didn't open in Claude")) };
            }
            PanelStep::GiveUp { clear } => {
                if clear {
                    key("Escape")?;
                }
                return Err(format!("'{label}' isn't in Claude's agent panel"));
            }
        }
    }
    Err(format!("'{label}' isn't in Claude's agent panel"))
}

/// The command: prints the pane on success, the reason on failure (exit 1).
pub fn run(session_pid: u32, label: &str) -> anyhow::Result<()> {
    match open_in_claude(session_pid, label) {
        Ok(pane) => {
            println!("{pane}");
            Ok(())
        }
        Err(e) => anyhow::bail!(e),
    }
}
