//! The conversation between `tenx web` and `tenx-voice`, the voice assistant
//! (a Pipecat pipeline in Python) — and what it is told about tasks.
//!
//! The browser talks to the assistant over WebRTC: its microphone and its
//! speaker, with the browser's echo cancellation. Only the setup goes
//! through `tenx web`, behind its cookie: the page posts its offer to
//! `/voice/offer` (and trickles ICE candidates with `PATCH` on the same
//! path), and `tenx web` hands both to the program as lines on its stdin.
//! The audio itself then flows between the browser and the program.
//!
//! `tenx web` → program, one JSON object per line (`ToVoice`): `offer`,
//! `ice`, `context` (the task the page shows), `pause` (the page paused the
//! conversation, or went on) and `event` (something to
//! tell the user — `Turns` and the watcher's attention edges decide what).
//! Program → `tenx web` (`FromVoice`): `ready` once it runs, an `answer`
//! or `ok` or `error` for each request id, and `sessions` whenever the
//! number of connected pages changes (`tenx web` stops an unused program).
//! EOF on stdin ends the program.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// What `tenx web` writes to the program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToVoice {
    /// A page's SDP offer: a new connection, or a renegotiation of `pc_id`.
    Offer {
        id: u64,
        sdp: String,
        sdp_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pc_id: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        restart_pc: bool,
        /// The task the page shows (a column row's id), if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        /// The page's `tenx web` session (its `hello`), sent back to `/mcp`
        /// as `mcp::SESSION_HEADER` so `tenx_focus_task` shows a task on
        /// that page.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// ICE candidates the page found after its offer.
    Ice { id: u64, pc_id: String, candidates: Vec<Candidate> },
    /// The page now shows another task (`None`: none).
    Context {
        pc_id: String,
        #[serde(default)]
        task: Option<String>,
    },
    /// The page paused the conversation (its microphone is off, so is its
    /// speaker): stop talking, and keep news until it goes on.
    Pause { pc_id: String, paused: bool },
    /// Something to tell every connected user, in a sentence.
    Event { text: String },
}

/// One ICE candidate as Pipecat's request handler takes it. An empty
/// `candidate` is the end-of-candidates marker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub candidate: String,
    pub sdp_mid: String,
    pub sdp_mline_index: u32,
}

/// What the program writes back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromVoice {
    Ready,
    Answer { id: u64, sdp: String, sdp_type: String, pc_id: String },
    Ok { id: u64 },
    Error {
        #[serde(default)]
        id: Option<u64>,
        error: String,
    },
    /// How many pages are connected now.
    Sessions { count: usize },
}

impl FromVoice {
    /// The request this answers, if it answers one.
    pub fn id(&self) -> Option<u64> {
        match self {
            FromVoice::Answer { id, .. } | FromVoice::Ok { id } => Some(*id),
            FromVoice::Error { id, .. } => *id,
            FromVoice::Ready | FromVoice::Sessions { .. } => None,
        }
    }
}

/// A line, newline included.
pub fn line<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).expect("a voice message serializes");
    s.push('\n');
    s
}

/// The page's `POST /voice/offer` body — the shape Pipecat's own clients
/// send, so the program's request handler takes it as it is.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PageOffer {
    pub sdp: String,
    #[serde(rename = "type")]
    pub sdp_type: String,
    #[serde(default)]
    pub pc_id: Option<String>,
    #[serde(default)]
    pub restart_pc: bool,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}

/// The page's `PATCH /voice/offer` body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PageIce {
    pub pc_id: String,
    pub candidates: Vec<Candidate>,
}

/// An SDP offer is text, and not a large one: what `tenx web` takes.
pub const MAX_BODY: usize = 64 * 1024;

/// Which tasks just finished a turn: a task seen `working` that is now
/// `done`. Blocked and bell edges are the watcher's own (`edge`); this is
/// what only a voice adds — "your task is done", as a person beside you
/// would say it. A task first seen done, or that went working → blocked →
/// done, is not news here: the first is a backlog, the second was already
/// announced as needing you.
#[derive(Debug, Default)]
pub struct Turns {
    last: HashMap<String, String>,
}

impl Turns {
    /// One poll: `(key, status token)` of every task with a live session
    /// (a task missing is idle). The keys that finished since the last poll.
    pub fn step<'a>(&mut self, now: &[(&'a str, &str)]) -> Vec<&'a str> {
        let mut done = Vec::new();
        let mut next = HashMap::with_capacity(now.len());
        for &(key, status) in now {
            if status == "done" && self.last.get(key).is_some_and(|s| s == "working") {
                done.push(key);
            }
            next.insert(key.to_string(), status.to_string());
        }
        self.last = next;
        done
    }
}

/// What the assistant is told about a task — not the words it says: it
/// turns this into a sentence of its own.
pub fn needs_you(task: &str, workspace: &str, reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("Task \"{task}\" ({workspace}) needs the user: {r}."),
        None => format!("Task \"{task}\" ({workspace}) needs the user."),
    }
}

pub fn secrets_requested(task: &str, workspace: &str, what: &str) -> String {
    format!("Task \"{task}\" ({workspace}) asks the user to release secrets: {what}. That is answered in the tenx column, not by voice.")
}

pub fn finished(task: &str, workspace: &str) -> String {
    format!("Task \"{task}\" ({workspace}) finished its turn and waits for the user.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_go_out_as_tagged_lines() {
        let offer = ToVoice::Offer {
            id: 7,
            sdp: "v=0".into(),
            sdp_type: "offer".into(),
            pc_id: None,
            restart_pc: false,
            task: Some("tenx/pipecat".into()),
            session: None,
        };
        assert_eq!(
            line(&offer),
            "{\"type\":\"offer\",\"id\":7,\"sdp\":\"v=0\",\"sdp_type\":\"offer\",\"task\":\"tenx/pipecat\"}\n"
        );
        let back: ToVoice = serde_json::from_str(line(&offer).trim()).unwrap();
        assert_eq!(back, offer);
    }

    #[test]
    fn replies_carry_their_request() {
        let a: FromVoice =
            serde_json::from_str(r#"{"type":"answer","id":3,"sdp":"v=0","sdp_type":"answer","pc_id":"pc-1"}"#).unwrap();
        assert_eq!(a.id(), Some(3));
        let e: FromVoice = serde_json::from_str(r#"{"type":"error","error":"no key"}"#).unwrap();
        assert_eq!(e.id(), None);
        assert_eq!(
            line(&ToVoice::Pause { pc_id: "pc-1".into(), paused: true }),
            "{\"type\":\"pause\",\"pc_id\":\"pc-1\",\"paused\":true}\n"
        );
        let s: FromVoice = serde_json::from_str(r#"{"type":"sessions","count":2}"#).unwrap();
        assert_eq!(s, FromVoice::Sessions { count: 2 });
        assert_eq!(serde_json::from_str::<FromVoice>(r#"{"type":"ready"}"#).unwrap(), FromVoice::Ready);
    }

    #[test]
    fn the_page_sends_what_pipecat_clients_send() {
        let o: PageOffer = serde_json::from_str(r#"{"sdp":"v=0","type":"offer","pc_id":null,"restart_pc":false}"#).unwrap();
        assert_eq!(o.sdp_type, "offer");
        assert_eq!(o.pc_id, None);
        assert_eq!(o.session, None);
        let o: PageOffer = serde_json::from_str(r#"{"sdp":"v=0","type":"offer","session":"a1b2"}"#).unwrap();
        assert_eq!(o.session.as_deref(), Some("a1b2"));
        let i: PageIce =
            serde_json::from_str(r#"{"pc_id":"pc-1","candidates":[{"candidate":"","sdp_mid":"0","sdp_mline_index":0}]}"#).unwrap();
        assert_eq!(i.candidates[0].candidate, "");
    }

    #[test]
    fn a_turn_finishes_on_working_to_done_only() {
        let mut t = Turns::default();
        // First sight: a backlog, nothing to say.
        assert!(t.step(&[("a", "done"), ("b", "working")]).is_empty());
        assert_eq!(t.step(&[("a", "done"), ("b", "done")]), vec!["b"]);
        // Still done: said once.
        assert!(t.step(&[("a", "done"), ("b", "done")]).is_empty());
        // Through blocked: that was announced as needing the user.
        assert!(t.step(&[("b", "working")]).is_empty());
        assert!(t.step(&[("b", "blocked")]).is_empty());
        assert!(t.step(&[("b", "done")]).is_empty());
        // Gone (idle) and back working, then done: news again.
        assert!(t.step(&[]).is_empty());
        assert!(t.step(&[("b", "working")]).is_empty());
        assert_eq!(t.step(&[("b", "done")]), vec!["b"]);
    }

    #[test]
    fn what_the_assistant_is_told() {
        assert_eq!(needs_you("web", "tenx", Some(" Bash: cargo test ")), "Task \"web\" (tenx) needs the user: Bash: cargo test.");
        assert_eq!(needs_you("web", "tenx", Some("")), "Task \"web\" (tenx) needs the user.");
        assert!(finished("web", "tenx").starts_with("Task \"web\" (tenx) finished"));
    }
}
