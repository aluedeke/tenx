//! Decisions behind `tenx secrets` — what a task's queues and sealed files
//! *mean*, given only facts the binary reads from disk.
//!
//! An agent's `need` call has no terminal, so all it can do is enqueue a
//! request and then wait for a human to act on it from a real shell or the
//! column. Three different things make a name leave the queues: the human
//! granted it (an output was written, then the name was cleared), the human
//! denied it (a denial was recorded, then the name was cleared), or someone
//! withdrew it (`tenx secrets cancel`). Granting and withdrawing are not
//! recorded at all, because the disk already knows: fulfilment rewrites an
//! output file, withdrawal doesn't. So the waiter compares the outputs'
//! modification times against the instant it made its request. A denial is
//! the one outcome that carries information the disk otherwise wouldn't
//! have — the human's note — so it is written down.
//!
//! The routing half — is a name already released, sealed and only waiting
//! for a passphrase, or missing so that someone must type a value — rests on
//! one property of `sops`: it encrypts a document's *values* only, so the
//! key names of a sealed file are readable without the identity
//! ([`sealed_keys`]).

use std::time::SystemTime;

/// What a waiting request should do after one look at the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The name is still queued — keep waiting.
    Pending,
    /// The name left the queue and an output was written since the request
    /// was made — the human acted on it.
    Fulfilled,
    /// The name left the queue with a denial recorded — the human said no.
    Denied,
    /// The name left the queue, nothing was written and nothing denied —
    /// withdrawn.
    Cancelled,
}

/// Decide the outcome from `still_pending` (is the name still in a queue?),
/// `denied` (is a denial recorded for it? — enqueueing clears any earlier
/// one, so a present denial always postdates the request) and `outputs`
/// (modification times of every file a fulfilment would have written;
/// missing files simply aren't in the slice) relative to `requested_at`. An
/// output touched *at or after* the request counts: fulfilment always
/// happens after the request, and callers pad `requested_at` backwards for
/// coarse filesystem timestamps rather than this function guessing at a
/// tolerance.
pub fn wait_outcome(still_pending: bool, denied: bool, outputs: &[SystemTime], requested_at: SystemTime) -> WaitOutcome {
    if still_pending {
        return WaitOutcome::Pending;
    }
    if denied {
        return WaitOutcome::Denied;
    }
    if outputs.iter().any(|t| *t >= requested_at) {
        WaitOutcome::Fulfilled
    } else {
        WaitOutcome::Cancelled
    }
}

/// Exit code for a finished wait over several names, so an agent can branch
/// without parsing prose: 0 when everything was granted, otherwise the
/// outcome that most changes what it should do next — a denial (don't ask
/// again without a reason) over a withdrawal over a timeout (just re-run).
pub fn exit_code(outcomes: &[WaitOutcome]) -> i32 {
    if outcomes.contains(&WaitOutcome::Denied) {
        EXIT_DENIED
    } else if outcomes.contains(&WaitOutcome::Cancelled) {
        EXIT_WITHDRAWN
    } else if outcomes.contains(&WaitOutcome::Pending) {
        EXIT_PENDING
    } else {
        0
    }
}

/// Still queued when the wait ran out; re-running resumes it.
pub const EXIT_PENDING: i32 = 3;
/// A human denied at least one name.
pub const EXIT_DENIED: i32 = 4;
/// At least one name was withdrawn (`cancel`) before anyone answered.
pub const EXIT_WITHDRAWN: i32 = 5;

/// Whether `name` can be a secret name: a dotenv key or a filename fragment,
/// and a line in the queue files — so no whitespace (the side files are
/// tab-separated), no path separators, no quotes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '"' | '='))
}

/// Key names of a dotenv document — a `sops`-sealed one (values are
/// `ENC[...]`, keys are plain) or a released one. Skips comments, blank
/// lines and `sops`'s own metadata keys (`sops_version`, `sops_mac`, ...).
pub fn dotenv_keys(text: &str) -> Vec<String> {
    text.lines().filter_map(dotenv_key).filter(|k| !k.starts_with("sops_")).map(str::to_string).collect()
}

fn dotenv_key(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

/// Key names of a `sops`-sealed document in any of the formats `sops`
/// writes — dotenv (`KEY=ENC[...]`), YAML (`key: ENC[...]`) or JSON
/// (`"key": "ENC[...]"`) — found by their encrypted values, which is what
/// lets a request be routed without the identity. Nested keys count as
/// well; the scan stops at `sops`'s own metadata block (`sops:` / `"sops":`,
/// always written last), whose `mac` is also an `ENC[...]` value.
pub fn sealed_keys(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim_start().trim_start_matches("- ");
        if l.starts_with("sops:") || l.starts_with("\"sops\":") {
            break;
        }
        let l = l.strip_prefix("export ").unwrap_or(l);
        let (key, rest) = if let Some(r) = l.strip_prefix('"') {
            let Some((k, rest)) = r.split_once('"') else { continue };
            let Some(rest) = rest.trim_start().strip_prefix(':') else { continue };
            (k, rest)
        } else if let Some(i) = l.find(['=', ':']) {
            (l[..i].trim(), &l[i + 1..])
        } else {
            continue;
        };
        let value = rest.trim_start().trim_start_matches(['"', '\'']);
        if value.starts_with("ENC[") && !key.is_empty() && !key.starts_with("sops_") && !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    keys
}

/// Merge the lines of `incoming` (a freshly decrypted dotenv document) into
/// `existing` (what was released before), keeping only keys in `only` when
/// given. A key already present is replaced in place, a new one appended —
/// so releasing one more name never drops what an earlier release granted.
pub fn merge_dotenv(existing: &str, incoming: &str, only: Option<&[String]>) -> String {
    let mut lines: Vec<String> = existing.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect();
    for line in incoming.lines() {
        let Some(key) = dotenv_key(line) else { continue };
        if key.starts_with("sops_") || only.is_some_and(|o| !o.iter().any(|n| n == key)) {
            continue;
        }
        match lines.iter_mut().find(|l| dotenv_key(l) == Some(key)) {
            Some(slot) => *slot = line.to_string(),
            None => lines.push(line.to_string()),
        }
    }
    if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" }
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

/// Parse a `NAME<TAB>text` side file (the request reasons, the denials).
/// A line without a tab is a name with empty text.
pub fn parse_notes(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match l.split_once('\t') {
            Some((n, t)) => (n.trim().to_string(), t.trim().to_string()),
            None => (l.trim().to_string(), String::new()),
        })
        .collect()
}

/// Inverse of [`parse_notes`]. Tabs and newlines inside the text are folded
/// to spaces so one note stays one line.
pub fn render_notes(notes: &[(String, String)]) -> String {
    notes
        .iter()
        .map(|(n, t)| format!("{n}\t{}\n", t.replace(['\t', '\n', '\r'], " ")))
        .collect()
}

/// `notes` with `name` set to `text`, replacing any earlier entry for it.
pub fn upsert_note(mut notes: Vec<(String, String)>, name: &str, text: &str) -> Vec<(String, String)> {
    notes.retain(|(n, _)| n != name);
    notes.push((name.to_string(), text.to_string()));
    notes
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
        assert_eq!(wait_outcome(true, false, &[t(200)], t(100)), WaitOutcome::Pending);
        assert_eq!(wait_outcome(true, true, &[], t(100)), WaitOutcome::Pending);
    }

    #[test]
    fn dequeued_with_a_fresh_output_is_fulfilled() {
        assert_eq!(wait_outcome(false, false, &[t(150)], t(100)), WaitOutcome::Fulfilled);
        // Written in the same instant as the request still counts.
        assert_eq!(wait_outcome(false, false, &[t(100)], t(100)), WaitOutcome::Fulfilled);
        // One fresh output among stale ones is enough.
        assert_eq!(wait_outcome(false, false, &[t(10), t(150)], t(100)), WaitOutcome::Fulfilled);
    }

    #[test]
    fn dequeued_with_only_stale_outputs_is_cancelled() {
        // A plaintext left over from an earlier unlock doesn't satisfy a
        // request made after it.
        assert_eq!(wait_outcome(false, false, &[t(50)], t(100)), WaitOutcome::Cancelled);
        assert_eq!(wait_outcome(false, false, &[], t(100)), WaitOutcome::Cancelled);
    }

    #[test]
    fn a_denial_wins_over_an_output_written_for_another_name() {
        assert_eq!(wait_outcome(false, true, &[t(150)], t(100)), WaitOutcome::Denied);
    }

    #[test]
    fn exit_code_prefers_the_outcome_that_changes_the_next_step() {
        use WaitOutcome::*;
        assert_eq!(exit_code(&[Fulfilled, Fulfilled]), 0);
        assert_eq!(exit_code(&[]), 0);
        assert_eq!(exit_code(&[Fulfilled, Pending]), EXIT_PENDING);
        assert_eq!(exit_code(&[Pending, Cancelled]), EXIT_WITHDRAWN);
        assert_eq!(exit_code(&[Cancelled, Denied, Pending]), EXIT_DENIED);
    }

    #[test]
    fn names_are_single_tokens() {
        assert!(valid_name("STRIPE_KEY"));
        assert!(valid_name("staging"));
        assert!(valid_name("secrets.prod.enc.env"));
        for bad in ["", ".", "..", "a b", "a\tb", "a/b", "a\"b", "A=1"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn keys_of_a_sealed_document_are_readable() {
        let sealed = "A=ENC[AES256_GCM,data:RQ==]\nB=ENC[AES256_GCM,data:DC==]\n\
                      sops_age__list_0__map_recipient=age1xyz\nsops_mac=ENC[...]\nsops_version=3.13.2\n";
        assert_eq!(dotenv_keys(sealed), ["A", "B"]);
        assert_eq!(dotenv_keys("# c\n\nexport X=1\n Y = 2\nnoequals\n"), ["X", "Y"]);
    }

    #[test]
    fn sealed_keys_in_every_sops_format() {
        let env = "A=ENC[AES256_GCM,data:RQ==]\nPLAIN_unencrypted=x\nsops_mac=ENC[AES256_GCM,data:x]\n";
        assert_eq!(sealed_keys(env), ["A"]);
        let yaml = "db:\n    password: ENC[AES256_GCM,data:x]\ntoken: ENC[AES256_GCM,data:y]\nsops:\n    mac: ENC[AES256_GCM,data:z]\n";
        assert_eq!(sealed_keys(yaml), ["password", "token"]);
        let json = "{\n\t\"API_KEY\": \"ENC[AES256_GCM,data:x]\",\n\t\"sops\": {\n\t\t\"mac\": \"ENC[AES256_GCM,data:z]\"\n\t}\n}\n";
        assert_eq!(sealed_keys(json), ["API_KEY"]);
    }

    #[test]
    fn merge_keeps_earlier_releases_and_filters_new_ones() {
        let existing = "A=old\nC=3\n";
        let incoming = "A=new\nB=2\nD=4\n";
        let only = ["A".to_string(), "B".to_string()];
        assert_eq!(merge_dotenv(existing, incoming, Some(&only)), "A=new\nC=3\nB=2\n");
        assert_eq!(merge_dotenv("", incoming, None), "A=new\nB=2\nD=4\n");
        assert_eq!(merge_dotenv("", incoming, Some(&[])), "");
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

    #[test]
    fn notes_round_trip_one_line_each() {
        let notes = upsert_note(vec![("A".into(), "x".into())], "B", "run the\ttests\nnow");
        let notes = upsert_note(notes, "A", "y");
        let text = render_notes(&notes);
        assert_eq!(text, "B\trun the tests now\nA\ty\n");
        assert_eq!(parse_notes(&text), notes.iter().map(|(n, t)| (n.clone(), t.replace(['\t', '\n'], " "))).collect::<Vec<_>>());
        assert_eq!(parse_notes("LONE\n"), [("LONE".to_string(), String::new())]);
    }
}
