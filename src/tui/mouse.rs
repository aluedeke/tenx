//! Helpers shared by the TUI event loops for turning `Event::Mouse` into
//! actions. Every TUI enables `EnableMouseCapture`, so crossterm reports clicks
//! and wheel events with absolute terminal coordinates; these map those
//! coordinates onto rendered widgets and distinguish single from double clicks.

use ratatui::layout::Rect;
use std::time::{Duration, Instant};

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

/// Tracks left-clicks to tell a single click from a double-click on the same
/// item, so the event loops can select on the first click and activate on the
/// second without any per-frame timing state of their own.
pub struct ClickTracker {
    last: Option<(usize, Instant)>,
}

impl ClickTracker {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Record a click on item `idx`; returns true when it completes a
    /// double-click (same item within the threshold). A double-click resets the
    /// tracker so a third click starts a fresh pair.
    pub fn click(&mut self, idx: usize) -> bool {
        let now = Instant::now();
        let double = matches!(self.last, Some((i, t)) if i == idx && now.duration_since(t) < THRESHOLD);
        self.last = if double { None } else { Some((idx, now)) };
        double
    }
}

const THRESHOLD: Duration = Duration::from_millis(400);
