//! `tenx internal open-agent <session-pid> [label]`: show a subagent in Claude
//! Code's own agent view in its session's pane, or with no label the session's
//! main view — what landing on a subagent or task line in the column does
//! (`tui::client`), as a command for scripts and for testing against a tmux
//! server of one's own (`TENX_TMUX_SOCKET`).
//!
//! Claude Code offers no way to name an agent from outside, so this presses
//! keys in the pane the way a finger would, through the agent panel under the
//! prompt; `tenx_core::agent_panel` has the panel's shape and decides each key.

/// Put `target` — a subagent, by the label its row shows, or the session's
/// main view — on screen in the pane of session `session_pid`, by walking
/// Claude Code's agent panel to its row, up or down from wherever its
/// selection is (`tenx_core::agent_panel` decides each key from a fresh
/// capture). Presses nothing when the pane already shows it.
/// Returns the pane. Refuses while a permission dialog is up — `↓` would move
/// its choice — and leaves the panel as it found it when the row isn't listed
/// (Claude drops a finished subagent after about 30 s).
pub use tenx_core::agent_panel::{AgentRef, Target};

pub fn open_in_claude(session_pid: u32, target: Target) -> Result<String, String> {
    use tenx_core::agent_panel::{next_step, selected_row, showing, view_open, PanelStep, MAX_PRESSES};
    let name = match target {
        Target::Main => "main".to_string(),
        Target::Agent(a) => a.label.to_string(),
    };
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
    if showing(&now, target) {
        return Ok(pane);
    }
    if tenx_core::dialog::permission_dialog_visible(&now) {
        return Err("a dialog is open in its pane".into());
    }
    let mut previous: Option<String> = None;
    for presses in 0..=MAX_PRESSES {
        match next_step(&now, target, previous.as_deref(), presses) {
            step @ (PanelStep::Down | PanelStep::Up) => {
                previous = selected_row(&now);
                key(if step == PanelStep::Down { "Down" } else { "Up" })?;
                now = settle(&now)?;
            }
            PanelStep::Open => {
                key("Enter")?;
                now = settle(&now)?;
                let opened = showing(&now, target) || matches!(target, Target::Agent(a) if view_open(&now, a.label));
                return if opened { Ok(pane) } else { Err(format!("'{name}' didn't open in Claude")) };
            }
            PanelStep::GiveUp { clear } => {
                if clear {
                    key("Escape")?;
                }
                break;
            }
        }
    }
    Err(format!("Claude no longer lists '{name}'"))
}

/// The command: prints the pane on success, the reason on failure (exit 1).
/// No label means the session's main view; `agent_type`/`nth`/`peers` find a
/// row that no longer shows the label (`AgentRef`).
pub fn run(
    session_pid: u32,
    label: Option<&str>,
    agent_type: Option<&str>,
    nth: Option<usize>,
    peers: Option<usize>,
) -> anyhow::Result<()> {
    let target = match label {
        None => Target::Main,
        Some(label) => {
            Target::Agent(AgentRef { label, agent_type: agent_type.unwrap_or(""), nth, peers: peers.unwrap_or(0) })
        }
    };
    match open_in_claude(session_pid, target) {
        Ok(pane) => {
            println!("{pane}");
            Ok(())
        }
        Err(e) => anyhow::bail!(e),
    }
}
