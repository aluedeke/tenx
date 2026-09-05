//! `tenx internal sidebar` — the tmux side of the sidebar pane: what Ctrl+w
//! runs (`cycle`: show and focus the column, or hide it from inside), and the
//! on/off switch the overlay's `:sidebar` command uses (`toggle`). The pane
//! itself is `tenx overlay --sidebar` (`tui::overlay`); `tmux::open_sidebar`
//! creates it.
//!
//! Both act on the window that holds `pane` — the pane the key was pressed
//! in, as tmux expands `#{pane_id}` in the binding — so they work for
//! whichever client pressed the key, never the session's current window
//! from some other client's point of view.

use anyhow::Result;

/// What `sidebar_width` to hand `tmux::open_sidebar`, per the global config.
pub fn configured_width() -> u16 {
    crate::workspace::load_global().map(|g| g.sidebar_width).unwrap_or(0)
}

/// `Some(width)` when the global config wants a sidebar in new windows.
pub fn wanted() -> Option<u16> {
    let global = crate::workspace::load_global().unwrap_or_default();
    global.sidebar.then_some(global.sidebar_width)
}

/// Ctrl+w: from the task, bring up the list — focus the window's sidebar,
/// opening one first if the window has none (hidden, opened before the
/// sidebar existed, or the feature off). From inside the sidebar, hide it;
/// tmux gives the space and the focus back to the task. So one press shows
/// the column and a second press from there puts it away, and a column
/// left showing is a keystroke away while you work.
pub fn cycle(pane: &str) -> Result<()> {
    let window = crate::tmux::window_of_pane(pane)?;
    let sidebar = crate::tmux::list_panes(&window)?.into_iter().find(|p| p.sidebar).map(|p| p.id);
    match sidebar {
        Some(id) if id == pane => crate::tmux::close_sidebar(&window)?,
        Some(id) => crate::tmux::select_pane(&id)?,
        None => {
            let id = open(&window)?;
            crate::tmux::select_pane(&id)?;
        }
    }
    Ok(())
}

/// Put the keyboard in `window`'s sidebar, opening one if it has none —
/// how a sidebar hands over to the next window's sidebar when its selection
/// switches windows (`tui::overlay`).
pub fn ensure_focused(window: &str) -> Result<()> {
    let id = match crate::tmux::sidebar_pane(window) {
        Some(id) => id,
        None => open(window)?,
    };
    crate::tmux::select_pane(&id)
}

/// Add a sidebar to `window`, or remove the one it has.
pub fn toggle(window: &str) -> Result<()> {
    match crate::tmux::sidebar_pane(window) {
        Some(_) => crate::tmux::close_sidebar(window),
        None => open(window).map(drop),
    }
}

fn open(window: &str) -> Result<String> {
    let bin = std::env::current_exe()?;
    let cwd = crate::tmux::pane_path(window).unwrap_or_else(|_| std::env::var("HOME").unwrap_or_default());
    crate::tmux::open_sidebar(window, &bin.to_string_lossy(), &cwd, configured_width())
}
