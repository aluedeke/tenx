//! Rules for the column — the task list `tenx` draws on the left of the
//! embedded session (`tui::client`): its width, and where "next task that
//! needs you" lands.

/// The column's share of the terminal when no width is configured.
pub const DEFAULT_PERCENT: u16 = 20;
/// Narrower than this and titles truncate to nothing useful; wider and it
/// takes room from the task on a big screen for no gain.
pub const MIN_COLS: u16 = 30;
pub const MAX_COLS: u16 = 48;

/// The column width for a terminal `window_cols` wide. `configured` is the
/// user's `column_width` (0 = automatic: [`DEFAULT_PERCENT`] of the window,
/// clamped to [`MIN_COLS`]..=[`MAX_COLS`]). A configured width is honoured
/// as given, but never so wide that the task gets less than half the window.
pub fn width(window_cols: u16, configured: u16) -> u16 {
    let half = (window_cols / 2).max(1);
    if configured > 0 {
        return configured.min(half);
    }
    (window_cols * DEFAULT_PERCENT / 100).clamp(MIN_COLS, MAX_COLS).min(half)
}

/// The next row that needs you, cycling: `needs[i]` says whether row `i`
/// wants attention, `from` is the row the cursor is on (`None` when it is
/// in the search field, so the search starts at the top). Wraps around the
/// end and never returns `from` itself unless it is the only row that
/// qualifies; `None` when nothing does. There is no "previous": the list
/// is ordered by urgency and the cycle is short, so one direction is enough.
pub fn next_needing(from: Option<usize>, needs: &[bool]) -> Option<usize> {
    let n = needs.len();
    if n == 0 {
        return None;
    }
    // Start one past the cursor, or at row 0 from the search field.
    let start = from.map_or(0, |i| (i + 1) % n);
    (0..n).map(|k| (start + k) % n).find(|&i| needs[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_needing_cycles() {
        let needs = [false, true, false, true, false];
        assert_eq!(next_needing(None, &needs), Some(1));
        assert_eq!(next_needing(Some(1), &needs), Some(3));
        assert_eq!(next_needing(Some(3), &needs), Some(1)); // wraps
        assert_eq!(next_needing(Some(0), &needs), Some(1));
    }

    #[test]
    fn next_needing_handles_the_edges() {
        assert_eq!(next_needing(None, &[]), None);
        assert_eq!(next_needing(Some(0), &[false, false]), None);
        // The only qualifying row is the one the cursor is on: stay there.
        assert_eq!(next_needing(Some(2), &[false, false, true]), Some(2));
    }

    #[test]
    fn automatic_width_is_a_clamped_share() {
        assert_eq!(width(200, 0), 40);
        assert_eq!(width(120, 0), MIN_COLS); // 24 would be too narrow
        assert_eq!(width(400, 0), MAX_COLS); // 80 would be too wide
    }

    #[test]
    fn configured_width_wins_but_leaves_half_the_window() {
        assert_eq!(width(200, 36), 36);
        assert_eq!(width(60, 45), 30);
        assert_eq!(width(50, 0), 25);
    }
}
