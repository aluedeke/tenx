use anyhow::Result;

/// Long-lived renderer for the 1-line header pane at the top of a task tab:
/// shows the task's display name and live Claude status, refreshing every
/// second (cheap: a handful of small file reads per tick). Runs until zellij
/// closes the pane with the tab. Name and status are re-read each tick so
/// TASK.md renames and state changes show up without any signaling — resolved
/// the same way the overlay resolves them, so the header and the list can't
/// disagree about a task.
pub fn header() -> Result<()> {
    use crate::workspace::TaskStatus as S;
    use std::io::Write as _;

    let dir = std::env::current_dir()?;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[?25l"); // hide the cursor in the sliver pane
    loop {
        let name = crate::workspace::read_task_display_name(&dir);
        let sessions = crate::workspace::claude::sessions();
        let state = crate::workspace::resolve_task_state(&dir, &sessions);
        let (status, changed) = (state.status, state.changed);
        let (badge, color) = match status {
            S::Working => ("▷ working", "\x1b[36m"),
            S::Blocked => ("\u{1F4AC} needs input", "\x1b[1;33m"),
            S::Done => ("\u{2705} waiting for you", "\x1b[32m"),
            S::Idle => ("· inactive", "\x1b[90m"),
        };
        // Age only for resting states — mirrors the overlay.
        let age = changed
            .filter(|_| matches!(status, S::Blocked | S::Done))
            .map(|t| format!(" {}", crate::workspace::format_age(t)))
            .unwrap_or_default();
        // What Claude is waiting on, in its own words — the header pane is right
        // above the session it describes, so the reason lands where you look.
        let reason = state
            .waiting_for
            .as_deref()
            .map(|r| format!(" · {r}"))
            .unwrap_or_default();
        let _ = write!(out, "\r\x1b[2K \x1b[1m{name}\x1b[0m  {color}{badge}{age}{reason}\x1b[0m");
        let _ = out.flush();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
