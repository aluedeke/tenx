use anyhow::Result;

pub fn notify() -> Result<()> {
    set_status(true)
}

pub fn notify_clear() -> Result<()> {
    set_status(false)
}

/// Mirror the Claude-activity state into a status file the overlay reads. Tab
/// names are stable (no 💬 renames) — the overlay is the only place activity is
/// shown, so this works identically inside and outside zellij.
fn set_status(attention: bool) -> Result<()> {
    let task_dir = std::env::current_dir()?;
    let status_file = task_dir.join(".tenx-status");
    if attention {
        let _ = std::fs::write(&status_file, "attention");
    } else {
        let _ = std::fs::remove_file(&status_file);
    }
    Ok(())
}
