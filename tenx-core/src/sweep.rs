//! Which task windows are safe to close. A task's window (a live `claude`
//! process plus whatever else the layout spawned) stays resident forever once
//! opened; `sweep` reclaims the ones nobody is waiting on. Reopening is
//! unaffected — the window is recreated on demand and `claude --continue`
//! picks the conversation back up.

use crate::status::TaskStatus;
use crate::time::format_duration;
use std::time::{Duration, SystemTime};

/// Default idle threshold for a `Done` task (finished a turn, waiting on you)
/// before its window is swept. Long enough that answering tomorrow morning
/// still finds it resident; short enough that months of an unanswered "waiting
/// on you" don't sit there costing a live claude process forever. `Idle`
/// tasks (no live session at all) are swept immediately regardless.
pub const DEFAULT_SWEEP_AFTER: Duration = Duration::from_secs(8 * 3600);

/// Everything the decision needs, gathered by the caller from live sources
/// (Claude's registry for `status`/`changed`, the multiplexer for `active`,
/// the task dir for `pinned`).
#[derive(Debug, Clone)]
pub struct SweepInput {
    pub status: TaskStatus,
    /// When the status last changed (only consulted for `Done`).
    pub changed: Option<SystemTime>,
    /// The window the user is currently in — never swept.
    pub active: bool,
    /// Explicit opt-out.
    pub pinned: bool,
}

/// `Some(reason)` if this window should be closed now, `None` to leave it.
/// Never closes: the active window, a pinned task, or a `Blocked`/`Working`
/// task — those are exactly the windows a prompt or an agent is waiting on.
pub fn sweep_reason(input: &SweepInput, after: Duration, now: SystemTime) -> Option<String> {
    if input.active || input.pinned {
        return None;
    }
    match input.status {
        TaskStatus::Blocked | TaskStatus::Signaled | TaskStatus::Working => None,
        TaskStatus::Idle => Some("idle, no live session".to_string()),
        TaskStatus::Done => {
            let changed = input.changed?;
            let elapsed = now.duration_since(changed).ok()?;
            if elapsed < after {
                return None;
            }
            Some(format!("done, waiting {} unanswered", format_duration(elapsed)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn input(status: TaskStatus, changed: Option<u64>) -> SweepInput {
        SweepInput { status, changed: changed.map(at), active: false, pinned: false }
    }

    const NOW: u64 = 100_000;

    #[test]
    fn active_and_pinned_are_never_swept() {
        let mut i = input(TaskStatus::Idle, None);
        i.active = true;
        assert!(sweep_reason(&i, DEFAULT_SWEEP_AFTER, at(NOW)).is_none());
        let mut i = input(TaskStatus::Idle, None);
        i.pinned = true;
        assert!(sweep_reason(&i, DEFAULT_SWEEP_AFTER, at(NOW)).is_none());
    }

    #[test]
    fn blocked_and_working_are_never_swept() {
        assert!(sweep_reason(&input(TaskStatus::Blocked, Some(1)), Duration::ZERO, at(NOW)).is_none());
        assert!(sweep_reason(&input(TaskStatus::Signaled, None), Duration::ZERO, at(NOW)).is_none());
        assert!(sweep_reason(&input(TaskStatus::Working, Some(1)), Duration::ZERO, at(NOW)).is_none());
    }

    #[test]
    fn idle_is_swept_immediately() {
        assert_eq!(
            sweep_reason(&input(TaskStatus::Idle, None), DEFAULT_SWEEP_AFTER, at(NOW)).as_deref(),
            Some("idle, no live session")
        );
    }

    #[test]
    fn done_respects_threshold() {
        let after = Duration::from_secs(3600);
        let recent = input(TaskStatus::Done, Some(NOW - 600));
        assert!(sweep_reason(&recent, after, at(NOW)).is_none());
        let old = input(TaskStatus::Done, Some(NOW - 5 * 3600));
        assert_eq!(sweep_reason(&old, after, at(NOW)).as_deref(), Some("done, waiting 5h unanswered"));
    }

    #[test]
    fn done_without_timestamp_is_left_alone() {
        assert!(sweep_reason(&input(TaskStatus::Done, None), Duration::ZERO, at(NOW)).is_none());
    }
}
