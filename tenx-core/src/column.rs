//! Sizing rules for the column — the task list `tenx` draws on the left of
//! the embedded session (`tui::client`) — and for the task windows behind
//! it, which are shared by every client but have one size each.

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

/// A client narrower than this gets no column beside the task: the list is
/// the whole screen or hidden (`tui::client`).
pub const SMALL_CLIENT_COLS: u16 = 100;

/// The size of a window shown on a `cols`×`rows` tmux client: tmux keeps
/// the last row for its status line. tenx's own tmux clients run in a pty
/// already cut to the task's share of the terminal (beside the column, or
/// the whole screen on a narrow client), so nothing else comes off.
pub fn window_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(1), rows.saturating_sub(1).max(1))
}

/// The one size a task window should have. With clients on it (`on_it`,
/// their tty sizes): the smallest of them, so every one of them sees the
/// whole window — a phone opening a task the desktop is in shrinks it, and
/// the desktop gets it back the moment the phone leaves. With nobody on it:
/// the widest attached client's, where its output will be read next and
/// where an agent's history should wrap. `None` with no client attached at
/// all: nothing to size for, so nothing is touched.
pub fn expected_size(on_it: &[(u16, u16)], attached: &[(u16, u16)]) -> Option<(u16, u16)> {
    let pick = if on_it.is_empty() {
        attached.iter().copied().max_by_key(|&(c, r)| (c, r))
    } else {
        on_it.iter().copied().min_by_key(|&(c, r)| (c, r))
    };
    pick.map(|(c, r)| window_size(c, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_size_keeps_tmux_its_status_row() {
        assert_eq!(window_size(144, 45), (144, 44));
        assert_eq!(window_size(70, 30), (70, 29));
        assert_eq!(window_size(1, 1), (1, 1));
    }

    #[test]
    fn shared_windows_take_the_smallest_viewer() {
        assert_eq!(expected_size(&[(144, 45), (70, 30)], &[(144, 45), (70, 30)]), Some((70, 29)));
        assert_eq!(expected_size(&[(144, 45)], &[(144, 45), (70, 30)]), Some((144, 44)));
    }

    #[test]
    fn unviewed_windows_take_the_widest_client_or_nothing() {
        assert_eq!(expected_size(&[], &[(70, 30), (144, 45)]), Some((144, 44)));
        assert_eq!(expected_size(&[], &[(70, 30)]), Some((70, 29)));
        assert_eq!(expected_size(&[], &[]), None);
    }
}
