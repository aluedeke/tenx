//! The watcher's picture of every task, as one serialisable document — what
//! `tenx watch` writes each tick and the sidebar panes render.
//!
//! The sidebar is a pane in every task window, so there can be a dozen of
//! them; if each resolved every task itself, the cost of the session would
//! grow with the number of open windows. Instead the watcher — which resolves
//! everything anyway — publishes this snapshot, and a sidebar only ever parses
//! a small JSON file. A sidebar that finds the snapshot missing or stale
//! (no watcher) falls back to resolving on its own, so the picture is never
//! blank, just more expensive.
//!
//! Timestamps are absolute (unix seconds), never ages: the document then
//! only changes when something happened, and a reader computes ages itself.

use serde::{Deserialize, Serialize};

use crate::live::PrInfo;

/// After this many seconds without a write the snapshot is treated as stale —
/// the watcher is gone (it writes, or touches, the file on every 2 s poll).
pub const STALE_AFTER_SECS: u64 = 10;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Slug of the session's current window, if it is a task window.
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub tasks: Vec<TaskSnap>,
}

/// One task, with everything a list row needs and nothing that has to be
/// re-derived from disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnap {
    pub ws: String,
    pub ws_dir: String,
    pub slug: String,
    pub title: String,
    pub path: String,
    /// `TaskStatus::token`.
    pub status: String,
    #[serde(default)]
    pub waiting_for: Option<String>,
    /// When the status last changed, unix seconds.
    #[serde(default)]
    pub changed_at: Option<u64>,
    /// The task directory's mtime, unix seconds — the activity fallback for
    /// a task Claude has never touched.
    #[serde(default)]
    pub created_at: u64,
    /// The task's tmux window id when its window is open.
    #[serde(default)]
    pub window_id: Option<String>,
    /// The pane its Claude session runs in.
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub prs: Vec<PrInfo>,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub secrets_pending: Vec<String>,
    #[serde(default)]
    pub secrets_pending_set: Vec<String>,
}

impl Snapshot {
    /// Whether a snapshot last written at `written_at` is still current at
    /// `now` (both unix seconds).
    pub fn is_fresh(written_at: u64, now: u64) -> bool {
        now.saturating_sub(written_at) <= STALE_AFTER_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_fields() {
        let snap = Snapshot {
            current: Some("a".into()),
            tasks: vec![TaskSnap { slug: "a".into(), status: "working".into(), ports: vec![3000], ..Default::default() }],
        };
        let text = serde_json::to_string(&snap).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&text).unwrap(), snap);
        let minimal: Snapshot = serde_json::from_str(r#"{"tasks":[{"ws":"","ws_dir":"","slug":"x","title":"","path":"","status":"idle"}]}"#).unwrap();
        assert_eq!(minimal.tasks[0].slug, "x");
        assert!(minimal.current.is_none());
    }

    #[test]
    fn freshness_window() {
        assert!(Snapshot::is_fresh(100, 100 + STALE_AFTER_SECS));
        assert!(!Snapshot::is_fresh(100, 101 + STALE_AFTER_SECS));
        assert!(Snapshot::is_fresh(100, 90)); // a clock step backwards is not staleness
    }
}
