//! Helpers shared by the TUI event loops for turning `Event::Mouse` into
//! actions. Every TUI enables `EnableMouseCapture`, so crossterm reports clicks
//! and wheel events with absolute terminal coordinates; these map those
//! coordinates onto rendered widgets.
//!
//! Note there is deliberately no click-to-activate helper: activating a task
//! runs `zellij action go-to-tab`, which zellij applies to the last client
//! that pressed a *key* (mouse input doesn't update that), so mouse-triggered
//! jumps switch the wrong client's tab when several clients are attached.
//! Clicks only ever select; activation stays on ⏎.

use ratatui::layout::Rect;

/// Whether a click at terminal (`col`, `row`) lands inside `area`.
pub fn hit(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x
        && col < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
}

/// Which list item sits under a click at terminal (`col`, `row`).
///
/// `border` is how many rows/cols the widget's block border steals from the
/// top/left edges (0 for a borderless list, 1 for a bordered one). `offset` is
/// the list's current scroll offset in items and `item_height` how many terminal
/// rows each item occupies (1 for a normal list; the repos list uses several).
/// Returns `None` when the point is outside the item region.
pub fn item_at(
    area: Rect,
    border: u16,
    offset: usize,
    item_height: u16,
    col: u16,
    row: u16,
) -> Option<usize> {
    let item_height = item_height.max(1);
    let x0 = area.x + border;
    let y0 = area.y + border;
    let x1 = area.x + area.width.saturating_sub(border);
    let y1 = area.y + area.height.saturating_sub(border);
    if col < x0 || col >= x1 || row < y0 || row >= y1 {
        return None;
    }
    Some(offset + ((row - y0) / item_height) as usize)
}
