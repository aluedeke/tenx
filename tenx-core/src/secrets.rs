//! Decisions behind `tenx secrets`' agent-side wait — what a change in a
//! task's pending queue *means*, given only facts the binary reads from disk.
//!
//! An agent's `decrypt`/`set` call has no terminal, so all it can do is
//! enqueue a request and then wait for a human to act on it from a real
//! shell or the overlay. Two different things make its name leave the queue:
//! the human fulfilled it (the plaintext was written, then the name was
//! cleared), or someone withdrew it (`tenx secrets cancel`). Nothing is
//! recorded to tell those apart — no tombstone, no receipt — because the
//! disk already knows: fulfilment rewrites an output file, cancellation
//! doesn't. So the waiter compares the outputs' modification times against
//! the instant it made its request.

use std::time::SystemTime;

/// What a waiting `decrypt`/`set` should do after one look at the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The name is still queued — keep waiting.
    Pending,
    /// The name left the queue and an output was written since the request
    /// was made — the human acted on it.
    Fulfilled,
    /// The name left the queue but nothing was written — withdrawn.
    Cancelled,
}

/// Decide the outcome from `still_pending` (is the name still in its queue?)
/// and `outputs` (modification times of every file a fulfilment would have
/// written — the released plaintext for a decrypt, the re-encrypted bundle
/// for a set; missing files simply aren't in the slice) relative to
/// `requested_at`. An output touched *at or after* the request counts:
/// fulfilment always happens after the request, and callers pad
/// `requested_at` backwards for coarse filesystem timestamps rather than
/// this function guessing at a tolerance.
pub fn wait_outcome(still_pending: bool, outputs: &[SystemTime], requested_at: SystemTime) -> WaitOutcome {
    if still_pending {
        return WaitOutcome::Pending;
    }
    if outputs.iter().any(|t| *t >= requested_at) {
        WaitOutcome::Fulfilled
    } else {
        WaitOutcome::Cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn still_queued_is_pending_whatever_the_outputs_say() {
        assert_eq!(wait_outcome(true, &[t(200)], t(100)), WaitOutcome::Pending);
        assert_eq!(wait_outcome(true, &[], t(100)), WaitOutcome::Pending);
    }

    #[test]
    fn dequeued_with_a_fresh_output_is_fulfilled() {
        assert_eq!(wait_outcome(false, &[t(150)], t(100)), WaitOutcome::Fulfilled);
        // Written in the same instant as the request still counts.
        assert_eq!(wait_outcome(false, &[t(100)], t(100)), WaitOutcome::Fulfilled);
        // One fresh output among stale ones is enough.
        assert_eq!(wait_outcome(false, &[t(10), t(150)], t(100)), WaitOutcome::Fulfilled);
    }

    #[test]
    fn dequeued_with_only_stale_outputs_is_cancelled() {
        // A plaintext left over from an earlier unlock doesn't satisfy a
        // request made after it.
        assert_eq!(wait_outcome(false, &[t(50)], t(100)), WaitOutcome::Cancelled);
        assert_eq!(wait_outcome(false, &[], t(100)), WaitOutcome::Cancelled);
    }
}
