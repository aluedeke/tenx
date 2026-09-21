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
/// on you" don't sit there costing a live claude process forever.
pub const DEFAULT_SWEEP_AFTER: Duration = Duration::from_secs(8 * 3600);

/// How long an `Idle` window must have been *quiet* before it's swept.
///
/// `Idle` means "no session running in this task", which is normally true of a
/// window nobody has opened in weeks — cheap to close, instantly reopenable.
/// But it's also what every misread looks like: a status resolved against the
/// wrong directory, a registry that hasn't caught up with a session that just
/// started, an agent between processes. Closing on the spot turns any such
/// misread into a killed window, and the window is where the user's panes and
/// scrollback live, so the mistake is not free.
///
/// A window that genuinely has no session also produces no output, so requiring
/// a stretch of silence costs an abandoned window nothing and gives a misread
/// time to correct itself. Short enough that reclaiming still happens within a
/// coffee break.
pub const DEFAULT_IDLE_GRACE: Duration = Duration::from_secs(15 * 60);

/// Everything the decision needs, gathered by the caller from live sources
/// (Claude's registry for `status`/`changed`, the multiplexer for `active`,
/// the task dir for `pinned`).
#[derive(Debug, Clone)]
pub struct SweepInput {
    pub status: TaskStatus,
    /// When the status last changed (only consulted for `Done`).
    pub changed: Option<SystemTime>,
    /// When the window last produced any output (the multiplexer's own
    /// activity clock, which — unlike `changed` — survives the session that an
    /// `Idle` task no longer has). `None` means "age unknown", which never
    /// sweeps.
    pub quiet_since: Option<SystemTime>,
    /// The window the user is currently in — never swept.
    pub active: bool,
    /// Explicit opt-out.
    pub pinned: bool,
}

/// `Some(reason)` if this window should be closed now, `None` to leave it.
/// Never closes: the active window, a pinned task, or a `Blocked`/`Working`
/// task — those are exactly the windows a prompt or an agent is waiting on.
pub fn sweep_reason(input: &SweepInput, after: Duration, idle_after: Duration, now: SystemTime) -> Option<String> {
    if input.active || input.pinned {
        return None;
    }
    match input.status {
        TaskStatus::Blocked | TaskStatus::Signaled | TaskStatus::Working => None,
        TaskStatus::Idle => {
            // No session left to date the task by, so the window's own
            // activity clock is the only age available. Unknown age is not
            // an old one: leave it.
            let quiet = input.quiet_since?;
            let elapsed = now.duration_since(quiet).ok()?;
            if elapsed < idle_after {
                return None;
            }
            Some(format!("idle, no live session, quiet {}", format_duration(elapsed)))
        }
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
        SweepInput { status, changed: changed.map(at), quiet_since: Some(at(0)), active: false, pinned: false }
    }

    const NOW: u64 = 100_000;

    #[test]
    fn active_and_pinned_are_never_swept() {
        let mut i = input(TaskStatus::Idle, None);
        i.active = true;
        assert!(sweep_reason(&i, DEFAULT_SWEEP_AFTER, DEFAULT_IDLE_GRACE, at(NOW)).is_none());
        let mut i = input(TaskStatus::Idle, None);
        i.pinned = true;
        assert!(sweep_reason(&i, DEFAULT_SWEEP_AFTER, DEFAULT_IDLE_GRACE, at(NOW)).is_none());
    }

    #[test]
    fn blocked_and_working_are_never_swept() {
        assert!(sweep_reason(&input(TaskStatus::Blocked, Some(1)), Duration::ZERO, Duration::ZERO, at(NOW)).is_none());
        assert!(sweep_reason(&input(TaskStatus::Signaled, None), Duration::ZERO, Duration::ZERO, at(NOW)).is_none());
        assert!(sweep_reason(&input(TaskStatus::Working, Some(1)), Duration::ZERO, Duration::ZERO, at(NOW)).is_none());
    }

    #[test]
    fn idle_is_swept_once_the_window_has_been_quiet() {
        let i = input(TaskStatus::Idle, None); // quiet since the epoch
        assert_eq!(
            sweep_reason(&i, DEFAULT_SWEEP_AFTER, DEFAULT_IDLE_GRACE, at(NOW)).as_deref(),
            Some("idle, no live session, quiet 1d")
        );
    }

    #[test]
    fn idle_within_the_grace_period_is_left_alone() {
        let mut i = input(TaskStatus::Idle, None);
        i.quiet_since = Some(at(NOW - DEFAULT_IDLE_GRACE.as_secs() + 1));
        assert!(sweep_reason(&i, DEFAULT_SWEEP_AFTER, DEFAULT_IDLE_GRACE, at(NOW)).is_none());
        i.quiet_since = Some(at(NOW - DEFAULT_IDLE_GRACE.as_secs()));
        assert!(sweep_reason(&i, DEFAULT_SWEEP_AFTER, DEFAULT_IDLE_GRACE, at(NOW)).is_some());
    }

    #[test]
    fn idle_without_a_known_age_is_left_alone() {
        let mut i = input(TaskStatus::Idle, None);
        i.quiet_since = None;
        assert!(sweep_reason(&i, DEFAULT_SWEEP_AFTER, DEFAULT_IDLE_GRACE, at(NOW)).is_none());
    }

    #[test]
    fn done_respects_threshold() {
        let after = Duration::from_secs(3600);
        let recent = input(TaskStatus::Done, Some(NOW - 600));
        assert!(sweep_reason(&recent, after, DEFAULT_IDLE_GRACE, at(NOW)).is_none());
        let old = input(TaskStatus::Done, Some(NOW - 5 * 3600));
        assert_eq!(sweep_reason(&old, after, DEFAULT_IDLE_GRACE, at(NOW)).as_deref(), Some("done, waiting 5h unanswered"));
    }

    #[test]
    fn done_without_timestamp_is_left_alone() {
        assert!(sweep_reason(&input(TaskStatus::Done, None), Duration::ZERO, DEFAULT_IDLE_GRACE, at(NOW)).is_none());
    }
}
