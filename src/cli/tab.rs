use anyhow::Result;

pub fn notify() -> Result<()> {
    rename(true)
}

pub fn notify_clear() -> Result<()> {
    rename(false)
}

fn rename(add_indicator: bool) -> Result<()> {
    if !crate::zellij::is_inside_session() {
        return Ok(());
    }

    let task_dir = std::env::current_dir()?;
    let title = crate::workspace::read_task_display_name(&task_dir);
    let new_name = if add_indicator {
        format!("\u{1F4AC} {title}")
    } else {
        title.clone()
    };

    // Try the stored tab ID first (fast path, valid within the same session).
    let id_str = std::fs::read_to_string(task_dir.join(".tenx-tab-id")).unwrap_or_default();
    if let Ok(tab_id) = id_str.trim().parse::<u32>() {
        if crate::zellij::rename_tab_by_id(tab_id, &new_name).is_ok() {
            return Ok(());
        }
    }

    // Stored ID is stale (new session). Fall back to searching by tab name and
    // update the file so future calls use the ID directly.
    let bare_title = title.trim_start_matches('\u{1F4AC}').trim().to_string();
    if let Ok(tabs) = crate::zellij::list_tabs() {
        let tab = tabs.iter().find(|t| {
            let n = t.name.trim_start_matches('\u{1F4AC}').trim();
            n == bare_title
        });
        if let Some(tab) = tab {
            let _ = crate::zellij::rename_tab_by_id(tab.tab_id, &new_name);
            let _ = std::fs::write(task_dir.join(".tenx-tab-id"), tab.tab_id.to_string());
        }
    }

    Ok(())
}
