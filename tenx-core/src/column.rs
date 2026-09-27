//! Rules for the column — the task list `tenx` draws on the left of the
//! embedded session (`tui::client`) and the web front end renders as HTML:
//! its width, how its rows are sectioned, ordered and filtered, and where
//! "next task that needs you" lands.

use std::cmp::Ordering;
use std::time::SystemTime;

use crate::status::{TaskGroup, TaskStatus};

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

/// The section a task is listed under: its status's group, except that a
/// pending secrets request (to unlock, or to supply a value) files it under
/// [`TaskGroup::SecretsPending`] whatever its agent is doing — it needs a
/// specific action from you even when the task is otherwise idle.
pub fn section(status: TaskStatus, secrets_pending: bool) -> TaskGroup {
    if secrets_pending { TaskGroup::SecretsPending } else { status.group() }
}

/// What a row is ordered by: its section, then the status it was filed with
/// (`TaskStatus::rank`, so blocked before done within "waiting"), then its
/// last activity, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortKey {
    pub section: TaskGroup,
    pub status: TaskStatus,
    pub activity: SystemTime,
}

/// The list order of two rows — [`SortKey`]'s fields in turn.
pub fn compare(a: &SortKey, b: &SortKey) -> Ordering {
    a.section
        .rank()
        .cmp(&b.section.rank())
        .then(a.status.rank().cmp(&b.status.rank()))
        .then(b.activity.cmp(&a.activity))
}

/// Whether a row wants something from you right now: filed under a pending
/// secrets request, or an agent that is blocked or rang the bell. `status`
/// is the live one, not the one the row was filed with, so a task that got
/// stuck since the list was built still counts.
pub fn needs_you(section: TaskGroup, status: TaskStatus) -> bool {
    section == TaskGroup::SecretsPending || status.needs_you()
}

/// The filter: a case-insensitive subsequence match (fuzzy) — are all the
/// characters of `needle` found in `haystack`, in order? Spaces in the
/// needle are ignored, so "api fix" finds "api: fix the login".
pub fn filter_matches(needle: &str, haystack: &str) -> bool {
    let needle = needle.to_lowercase();
    let haystack = haystack.to_lowercase();
    let mut hay = haystack.chars();
    for nc in needle.chars() {
        if nc == ' ' {
            continue;
        }
        loop {
            match hay.next() {
                Some(hc) if hc == nc => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(section: TaskGroup, status: TaskStatus, secs: u64) -> SortKey {
        SortKey { section, status, activity: SystemTime::UNIX_EPOCH + Duration::from_secs(secs) }
    }

    #[test]
    fn secrets_pending_overrides_the_status_group() {
        assert_eq!(section(TaskStatus::Idle, true), TaskGroup::SecretsPending);
        assert_eq!(section(TaskStatus::Working, false), TaskGroup::Working);
        assert_eq!(section(TaskStatus::Done, false), TaskGroup::Waiting);
    }

    #[test]
    fn rows_order_by_section_then_status_then_newest() {
        let mut rows = [
            key(TaskGroup::Inactive, TaskStatus::Idle, 50),
            key(TaskGroup::Waiting, TaskStatus::Done, 40),
            key(TaskGroup::Waiting, TaskStatus::Blocked, 10),
            key(TaskGroup::Waiting, TaskStatus::Blocked, 30),
            key(TaskGroup::SecretsPending, TaskStatus::Idle, 1),
        ];
        rows.sort_by(compare);
        let order: Vec<(TaskStatus, u64)> = rows
            .iter()
            .map(|k| (k.status, k.activity.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()))
            .collect();
        assert_eq!(
            order,
            vec![
                (TaskStatus::Idle, 1),
                (TaskStatus::Blocked, 30),
                (TaskStatus::Blocked, 10),
                (TaskStatus::Done, 40),
                (TaskStatus::Idle, 50),
            ]
        );
    }

    #[test]
    fn needs_you_is_secrets_or_a_needy_status() {
        assert!(needs_you(TaskGroup::SecretsPending, TaskStatus::Idle));
        assert!(needs_you(TaskGroup::Working, TaskStatus::Blocked));
        assert!(needs_you(TaskGroup::Waiting, TaskStatus::Signaled));
        assert!(!needs_you(TaskGroup::Waiting, TaskStatus::Done));
        assert!(!needs_you(TaskGroup::Working, TaskStatus::Working));
    }

    #[test]
    fn filter_is_a_case_insensitive_subsequence() {
        assert!(filter_matches("", "anything"));
        assert!(filter_matches("lgn", "ledger Login timeout"));
        assert!(filter_matches("API fix", "api: fix the login"));
        assert!(!filter_matches("xyz", "ledger login"));
        assert!(!filter_matches("nl", "ledger login")); // order matters
    }

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
