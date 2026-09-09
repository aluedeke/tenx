//! Pure parsing of Codex CLI's on-disk artifacts that tenx reads — currently
//! just the first line of a rollout transcript, used to decide whether a task's
//! directory has a resumable Codex thread.
//!
//! Codex writes each session to `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>
//! .jsonl` (sometimes `.jsonl.zst`); the first line is a `session_meta` record
//! carrying the session's `cwd`. `codex resume --last` resumes the newest thread
//! whose cwd is the current directory, so tenx only passes `resume --last` when
//! such a thread exists — otherwise Codex errors and the pane dies.

use serde::Deserialize;

#[derive(Deserialize)]
struct RolloutHead {
    #[serde(rename = "type")]
    kind: Option<String>,
    // The meta may be nested under `payload` (rollout line wrapper) or flattened
    // at the top level, depending on Codex version — accept either.
    payload: Option<Meta>,
    #[serde(flatten)]
    top: Meta,
}

#[derive(Deserialize, Default)]
struct Meta {
    cwd: Option<String>,
}

/// The `cwd` recorded in a rollout file's first (`session_meta`) line, if this
/// line is that record and carries one.
pub fn session_meta_cwd(first_line: &str) -> Option<String> {
    let head: RolloutHead = serde_json::from_str(first_line.trim()).ok()?;
    // Only trust a line that is actually the session_meta record.
    if head.kind.as_deref() != Some("session_meta") && head.kind.is_some() {
        return None;
    }
    head.payload.and_then(|p| p.cwd).or(head.top.cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cwd_from_payload_wrapper() {
        let line = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"x","cwd":"/ws/tasks/foo","originator":"cli"}}"#;
        assert_eq!(session_meta_cwd(line).as_deref(), Some("/ws/tasks/foo"));
    }

    #[test]
    fn reads_cwd_from_flattened_meta() {
        let line = r#"{"type":"session_meta","id":"x","cwd":"/ws/tasks/bar"}"#;
        assert_eq!(session_meta_cwd(line).as_deref(), Some("/ws/tasks/bar"));
    }

    #[test]
    fn rejects_non_meta_and_garbage() {
        assert_eq!(session_meta_cwd(r#"{"type":"response_item","payload":{}}"#), None);
        assert_eq!(session_meta_cwd("not json"), None);
        assert_eq!(session_meta_cwd(r#"{"type":"session_meta","payload":{}}"#), None);
    }
}
