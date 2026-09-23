//! Decisions behind `tenx secrets`' agent-side wait — what a change in a
//! task's pending queue *means*, given only facts the binary reads from disk.
//!
//! An agent's `decrypt`/`set` call has no terminal, so all it can do is
//! enqueue a request and then wait for a human to act on it from a real
//! shell or the column. Two different things make its name leave the queue:
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

/// Bracketed-paste markers (`ESC[200~` … `ESC[201~`): a terminal wraps a
/// paste in them whenever the program in front asked for bracketed paste —
/// the tenx client does, for its embedded terminal — and a prompt reading
/// raw bytes gets them as part of the value.
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// Strip bracketed-paste markers from terminal input.
pub fn strip_paste_markers(raw: &str) -> String {
    raw.replace(PASTE_START, "").replace(PASTE_END, "")
}

/// Clean a secret value typed or pasted at tenx's masked prompt: drop
/// bracketed-paste markers and the line ending, then refuse anything that
/// still carries control characters (an escape sequence from the terminal,
/// a stray Ctrl key) — sealing those silently stores a value that differs
/// from the one the human meant, and nobody can see it: the prompt is masked.
/// A tab is allowed; everything else below space and DEL is not.
pub fn clean_typed_value(raw: &str) -> Result<String, String> {
    let value = strip_paste_markers(raw).trim_end_matches(['\n', '\r']).to_string();
    if let Some(c) = value.chars().find(|c| c.is_control() && *c != '\t') {
        return Err(format!(
            "the value contains a control character ({:?}) — probably terminal escape codes from the paste; nothing was set",
            c
        ));
    }
    Ok(value)
}

/// What the human is shown after a masked entry, so a wrong paste is caught
/// before it's sealed: the length, and the last four characters of anything
/// long enough that four don't give it away.
pub fn describe_value(value: &str) -> String {
    let n = value.chars().count();
    if n >= 16 {
        let tail: String = value.chars().skip(n - 4).collect();
        format!("{n} characters, ending …{tail}")
    } else {
        format!("{n} characters")
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

    #[test]
    fn pasted_values_lose_their_bracketed_paste_markers() {
        assert_eq!(clean_typed_value("\x1b[200~sntrys_abc123\x1b[201~\n"), Ok("sntrys_abc123".to_string()));
        assert_eq!(clean_typed_value("typed\r\n"), Ok("typed".to_string()));
        assert_eq!(clean_typed_value("with\ttab"), Ok("with\ttab".to_string()));
        assert_eq!(clean_typed_value(""), Ok(String::new()));
    }

    #[test]
    fn other_control_characters_are_refused_not_sealed() {
        assert!(clean_typed_value("abc\x1b[Adef").is_err());
        assert!(clean_typed_value("abc\x7f").is_err());
        assert!(clean_typed_value("a\nb").is_err());
    }

    #[test]
    fn describing_a_value_shows_a_tail_only_when_long() {
        assert_eq!(describe_value("short"), "5 characters");
        assert_eq!(describe_value("sntrys_0123456789abcdef"), "23 characters, ending …cdef");
    }
}
