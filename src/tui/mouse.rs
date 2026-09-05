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

/// Which list item sits under a click when items have differing heights
/// (`heights`, one per item, in render order): walk the visible rows from
/// the scroll `offset` (in items) until the click's row is reached. `None`
/// outside the item region or past the last item.
pub fn item_at_heights(area: Rect, border: u16, offset: usize, heights: &[u16], col: u16, row: u16) -> Option<usize> {
    let x0 = area.x + border;
    let y0 = area.y + border;
    let x1 = area.x + area.width.saturating_sub(border);
    let y1 = area.y + area.height.saturating_sub(border);
    if col < x0 || col >= x1 || row < y0 || row >= y1 {
        return None;
    }
    let mut y = y0;
    for (i, h) in heights.iter().enumerate().skip(offset) {
        let next = y + (*h).max(1);
        if row < next {
            return Some(i);
        }
        y = next;
    }
    None
}
