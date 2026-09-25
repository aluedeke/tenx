//! A coding agent's own subagents — the agents a session spawns (Claude Code's
//! `Agent` tool: foreground, background, forks; Codex's `spawn_agent`; a pi
//! extension's child `pi` process) — as the child items of a task. Pure logic,
//! no I/O.
//!
//! Claude Code and Codex run subagents in-process and report them through
//! their hooks, as below. pi has no subagents of its own: extensions spawn a
//! child `pi` process per subagent, which loads tenx's extension like any pi
//! and so reports as a session of its own; `crate::status::nest_child_sessions`
//! turns such a session into a subagent of the pi session it descends from.
//!
//! The session registry keeps one record per agent *process*; a subagent is not
//! a process, it is a conversation the session runs beside its own. Claude Code
//! reports it through the same hooks as the session: `SubagentStart` and
//! `SubagentStop` bracket it, and every tool event it fires carries its
//! `agent_id` and `agent_type`. The binary (`cli::session_event`) keeps one
//! small record per subagent beside the session's (`workspace::sessions`); this
//! module decides what an event does to it, which of a session's subagents are
//! still worth listing, and when one the hooks never closed is over anyway.
//!
//! Codex (measured on 0.153.2) fires the same `SubagentStart`/`SubagentStop`
//! hooks with the same `agent_id`/`agent_type` fields, but a subagent's events
//! carry *its own* rollout as `transcript_path`, whose first line
//! (`session_meta`) names it (`agent_path`, `agent_nickname`); and its `Stop`
//! lists no background tasks, so a Codex subagent is closed by its own
//! `SubagentStop` or its session's end, never by the session's turn ending.
//!
//! Measured on Claude Code 2.1.281:
//! - `SubagentStart` carries `agent_id` and `agent_type`, no transcript path;
//!   `SubagentStop` adds `agent_transcript_path` and `last_assistant_message`.
//! - The transcript is `<session transcript minus .jsonl>/subagents/agent-<id>.jsonl`,
//!   with an `agent-<id>.meta.json` beside it holding the spawn's
//!   `description` and `requestShape` (`"background"` for `run_in_background`).
//! - The session's own `Stop` carries `background_tasks`: the background
//!   subagents still running as the turn ends. A foreground subagent cannot
//!   outlive its parent's turn, so at `Stop` any subagent not listed there is
//!   over — the one signal that closes a subagent whose turn was interrupted
//!   (Escape fires no `SubagentStop`).

use crate::status::TaskStatus;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Where a subagent is in its life. Coarser than a session's status on purpose:
/// a subagent has no "idle between turns" — it runs once and finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    /// A dialog of its own is open (a permission prompt for one of its tools,
    /// a question) — it shows in the session's pane, and blocks the task.
    Waiting,
    Finished,
}

impl SubagentStatus {
    pub fn token(self) -> &'static str {
        match self {
            SubagentStatus::Running => "running",
            SubagentStatus::Waiting => "waiting",
            SubagentStatus::Finished => "finished",
        }
    }

    /// Unknown tokens read as finished: a record from a newer tenx must not pin
    /// its task to `Working`.
    pub fn from_token(token: &str) -> SubagentStatus {
        match token {
            "running" => SubagentStatus::Running,
            "waiting" => SubagentStatus::Waiting,
            _ => SubagentStatus::Finished,
        }
    }

    /// The task status that draws the same way — so a subagent's glyph and
    /// colour come from the one glyph table (`TaskStatus::glyph`).
    pub fn as_task_status(self) -> TaskStatus {
        match self {
            SubagentStatus::Running => TaskStatus::Working,
            SubagentStatus::Waiting => TaskStatus::Blocked,
            SubagentStatus::Finished => TaskStatus::Done,
        }
    }
}

/// One subagent of a live session.
#[derive(Debug, Clone, PartialEq)]
pub struct Subagent {
    /// Claude Code's `agent_id`.
    pub id: String,
    /// The pid of the session that spawned it — its record's key.
    pub session_pid: u32,
    /// The harness running it (`claude`, `codex`, `pi`) — how its transcript
    /// reads.
    pub agent: String,
    /// `agent_type`: `Explore`, `general-purpose`, a custom agent's name.
    pub agent_type: String,
    /// The spawn's short description (from the meta file, or the parent's
    /// `background_tasks`), when known.
    pub description: Option<String>,
    pub status: SubagentStatus,
    pub waiting_for: Option<String>,
    pub started_at: Option<SystemTime>,
    pub updated_at: Option<SystemTime>,
    pub transcript_path: Option<PathBuf>,
    /// Spawned with `run_in_background`: it may outlive its parent's turn.
    pub background: bool,
}

impl Subagent {
    /// The label a list shows: the description when there is one, else the type.
    pub fn label(&self) -> &str {
        self.description.as_deref().filter(|d| !d.is_empty()).unwrap_or(&self.agent_type)
    }
}

/// What a hook event from inside a subagent does to its record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentAction {
    Set { status: SubagentStatus, waiting_for: Option<String> },
    Ignore,
}

/// Whether an event may *create* a subagent's record, for an id tenx has no
/// record of yet. `SubagentStart` does; a tool event does when it names its
/// `agent_type` (the subagent started before tenx's hooks were installed).
/// Nothing else: Claude Code also fires `SubagentStop` — with an `agent_id`,
/// no `agent_type`, no transcript and no `SubagentStart` before it — for its
/// own internal helper runs, and those are not agents anyone spawned.
pub fn opens_record(event: &str, agent_type: Option<&str>) -> bool {
    event == "SubagentStart" || (event != "SubagentStop" && agent_type.is_some_and(|t| !t.is_empty()))
}

/// Map a Claude Code hook event that carries an `agent_id` to what it means for
/// that subagent. The same reading as `session_event::claude_action`, minus the
/// session-only events, plus the subagent's own start and stop.
pub fn claude_subagent_action(event: &str, tool_name: Option<&str>) -> SubagentAction {
    let set = |status, waiting_for: Option<String>| SubagentAction::Set { status, waiting_for };
    match event {
        "SubagentStart" | "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" | "ElicitationResult" => {
            set(SubagentStatus::Running, None)
        }
        "PreToolUse" => {
            if tool_name.is_some_and(crate::session_event::is_claude_dialog_tool) {
                set(SubagentStatus::Waiting, Some("input needed".into()))
            } else {
                set(SubagentStatus::Running, None)
            }
        }
        "PermissionRequest" => set(SubagentStatus::Waiting, Some(format!("permission: {}", tool_name.unwrap_or("tool")))),
        "Elicitation" => set(SubagentStatus::Waiting, Some("input needed".into())),
        "SubagentStop" => set(SubagentStatus::Finished, None),
        _ => SubagentAction::Ignore,
    }
}

/// [`claude_subagent_action`] for Codex: the tool events bracket the
/// subagent's own tool calls, and an approval it asks for waits on you, with
/// the command as the reason (as `session_event::codex_action` does).
pub fn codex_subagent_action(event: &str, command: Option<&str>) -> SubagentAction {
    let set = |status, waiting_for: Option<String>| SubagentAction::Set { status, waiting_for };
    match event {
        "SubagentStart" | "PreToolUse" | "PostToolUse" => set(SubagentStatus::Running, None),
        "PermissionRequest" => set(
            SubagentStatus::Waiting,
            Some(match command {
                Some(cmd) if !cmd.is_empty() => format!("approval: {cmd}"),
                _ => "approval".to_string(),
            }),
        ),
        "SubagentStop" => set(SubagentStatus::Finished, None),
        _ => SubagentAction::Ignore,
    }
}

/// What a Codex subagent's rollout says about it, from its first line
/// (`session_meta`): a description from the last segment of its `agent_path`
/// (`/root/list_files` → `list files`), and its nickname (`Averroes`).
pub fn codex_subagent_meta(first_line: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(first_line) else { return (None, None) };
    let Some(p) = v.get("payload") else { return (None, None) };
    let description = p
        .get("agent_path")
        .and_then(|a| a.as_str())
        .and_then(|a| a.rsplit('/').next())
        .map(|seg| seg.replace(['_', '-'], " ").trim().to_string())
        .filter(|d| !d.is_empty() && d != "root");
    let nickname = p.get("agent_nickname").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).map(str::to_string);
    (description, nickname)
}

/// A background subagent still running as its session's turn ended, from the
/// `Stop` payload's `background_tasks` (entries of `type` `"subagent"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTask {
    pub id: String,
    pub description: Option<String>,
}

/// Parse the `background_tasks` array of a `Stop` payload, keeping subagents
/// only (the array also lists background shells and the like).
pub fn background_tasks(payload: &serde_json::Value) -> Vec<BackgroundTask> {
    payload
        .get("background_tasks")
        .and_then(|v| v.as_array())
        .map(|tasks| {
            tasks
                .iter()
                .filter(|t| t.get("type").and_then(|v| v.as_str()).is_none_or(|ty| ty == "subagent"))
                .filter_map(|t| {
                    Some(BackgroundTask {
                        id: t.get("id")?.as_str()?.to_string(),
                        description: t.get("description").and_then(|v| v.as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a subagent's `SubagentStop` is only a pause: Claude Code fires it
/// when the subagent ends its turn to wait on background work of its own (a
/// `run_in_background` shell), and the payload's `background_tasks` then lists
/// that work as running — anything but the subagent itself, which the list
/// carries until it has reported back. Measured on Claude Code 2.1.282.
pub fn waits_on_background(payload: &serde_json::Value, agent_id: &str) -> bool {
    payload
        .get("background_tasks")
        .and_then(|v| v.as_array())
        .is_some_and(|tasks| {
            tasks.iter().any(|t| {
                t.get("id").and_then(|v| v.as_str()) != Some(agent_id)
                    && t.get("status").and_then(|v| v.as_str()) == Some("running")
            })
        })
}

/// What a session's `Stop` changes about its subagents: every one not finished
/// and not among `still_running` is over; those that are get their description
/// filled in if it was missing. Returns the records to rewrite.
pub fn settle_on_stop(subagents: &[Subagent], still_running: &[BackgroundTask]) -> Vec<Subagent> {
    let mut out = Vec::new();
    for s in subagents {
        match still_running.iter().find(|t| t.id == s.id) {
            Some(t) => {
                if s.description.is_none() && t.description.is_some() {
                    let mut s = s.clone();
                    s.description = t.description.clone();
                    s.background = true;
                    out.push(s);
                }
            }
            None if s.status != SubagentStatus::Finished => {
                let mut s = s.clone();
                s.status = SubagentStatus::Finished;
                s.waiting_for = None;
                out.push(s);
            }
            None => {}
        }
    }
    out
}

/// How long a finished subagent stays listed under its task: long enough to
/// see that it finished and to open what it did, short enough that a session
/// which fanned out a dozen agents an hour ago isn't still a dozen lines tall.
pub const FINISHED_LINGER: Duration = Duration::from_secs(10 * 60);

/// At most this many finished subagents per task are listed; running and
/// waiting ones are always shown.
pub const MAX_FINISHED: usize = 3;

/// The subagents a task lists, in display order: waiting first, then running,
/// then the most recently finished (at most [`MAX_FINISHED`], none older than
/// [`FINISHED_LINGER`]); within each, newest first.
pub fn visible(subagents: &[Subagent], now: SystemTime) -> Vec<Subagent> {
    let recent = |s: &Subagent| {
        s.updated_at
            .and_then(|t| now.duration_since(t).ok())
            .is_none_or(|age| age <= FINISHED_LINGER)
    };
    let rank = |s: &Subagent| match s.status {
        SubagentStatus::Waiting => 0,
        SubagentStatus::Running => 1,
        SubagentStatus::Finished => 2,
    };
    let mut live: Vec<Subagent> =
        subagents.iter().filter(|s| s.status != SubagentStatus::Finished || recent(s)).cloned().collect();
    live.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then(b.started_at.or(b.updated_at).cmp(&a.started_at.or(a.updated_at)))
            .then(a.id.cmp(&b.id))
    });
    let mut finished = 0;
    live.retain(|s| {
        if s.status == SubagentStatus::Finished {
            finished += 1;
            finished <= MAX_FINISHED
        } else {
            true
        }
    });
    live
}

/// Whether a finished subagent's record can be deleted: past its linger with
/// room to spare, so a reader never sees it vanish while still listable.
pub fn prunable(s: &Subagent, now: SystemTime) -> bool {
    s.status == SubagentStatus::Finished
        && s.updated_at.and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > FINISHED_LINGER * 6)
}

/// A prompt as a one-line label: its first non-empty line, without the `Task:`
/// prefix pi's subagent extension puts on the task it hands a child, cut to
/// `max` characters.
pub fn prompt_label(prompt: &str, max: usize) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.strip_prefix("Task:").map(str::trim_start).unwrap_or(line);
    let flat: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(if flat.chars().count() <= max { flat } else { format!("{}…", flat.chars().take(max - 1).collect::<String>()) })
}

/// Where Claude Code writes a subagent's transcript, from its session's
/// `transcript_path`: `<dir>/<session>.jsonl` → `<dir>/<session>/subagents/agent-<id>.jsonl`.
/// `SubagentStart` doesn't carry the path; `SubagentStop` does and wins.
pub fn claude_transcript_path(session_transcript: &Path, agent_id: &str) -> Option<PathBuf> {
    let stem = session_transcript.file_stem()?;
    Some(session_transcript.with_file_name(stem).join("subagents").join(format!("agent-{agent_id}.jsonl")))
}

/// The `agent-<id>.meta.json` beside a subagent transcript.
pub fn claude_meta_path(transcript: &Path) -> Option<PathBuf> {
    let stem = transcript.file_stem()?.to_str()?;
    Some(transcript.with_file_name(format!("{stem}.meta.json")))
}

/// What a subagent's meta file says: its description, and whether it runs in
/// the background. Unknown keys are ignored; a missing one is `None`/`false`.
pub fn parse_meta(text: &str) -> (Option<String>, bool) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return (None, false) };
    let description = v.get("description").and_then(|d| d.as_str()).filter(|d| !d.is_empty()).map(str::to_string);
    let background = v.get("requestShape").and_then(|r| r.as_str()) == Some("background");
    (description, background)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn sub(id: &str, status: SubagentStatus, started: u64, updated: u64) -> Subagent {
        Subagent {
            id: id.into(),
            session_pid: 1,
            agent: "claude".into(),
            agent_type: "Explore".into(),
            description: None,
            status,
            waiting_for: None,
            started_at: Some(at(started)),
            updated_at: Some(at(updated)),
            transcript_path: None,
            background: false,
        }
    }

    fn status(a: &SubagentAction) -> Option<SubagentStatus> {
        match a {
            SubagentAction::Set { status, .. } => Some(*status),
            SubagentAction::Ignore => None,
        }
    }

    #[test]
    fn events_map_to_a_subagents_life() {
        assert_eq!(status(&claude_subagent_action("SubagentStart", None)), Some(SubagentStatus::Running));
        assert_eq!(status(&claude_subagent_action("PreToolUse", Some("Bash"))), Some(SubagentStatus::Running));
        assert_eq!(
            claude_subagent_action("PermissionRequest", Some("Bash")),
            SubagentAction::Set { status: SubagentStatus::Waiting, waiting_for: Some("permission: Bash".into()) }
        );
        assert_eq!(status(&claude_subagent_action("PreToolUse", Some("AskUserQuestion"))), Some(SubagentStatus::Waiting));
        assert_eq!(status(&claude_subagent_action("PostToolUse", Some("Bash"))), Some(SubagentStatus::Running));
        assert_eq!(status(&claude_subagent_action("PermissionDenied", Some("Bash"))), Some(SubagentStatus::Running));
        assert_eq!(status(&claude_subagent_action("SubagentStop", None)), Some(SubagentStatus::Finished));
        // Session-only events say nothing about a subagent.
        assert_eq!(claude_subagent_action("Notification", None), SubagentAction::Ignore);
        assert_eq!(claude_subagent_action("SessionEnd", None), SubagentAction::Ignore);
    }

    #[test]
    fn only_a_real_start_opens_a_record() {
        assert!(opens_record("SubagentStart", None));
        assert!(opens_record("PreToolUse", Some("Explore")));
        // Claude Code's internal helpers: a bare stop, no type.
        assert!(!opens_record("SubagentStop", None));
        assert!(!opens_record("SubagentStop", Some("Explore")));
        assert!(!opens_record("PostToolUse", None));
        assert!(!opens_record("PostToolUse", Some("")));
    }

    #[test]
    fn codex_events_map_to_a_subagents_life() {
        assert_eq!(status(&codex_subagent_action("SubagentStart", None)), Some(SubagentStatus::Running));
        assert_eq!(status(&codex_subagent_action("PreToolUse", None)), Some(SubagentStatus::Running));
        assert_eq!(
            codex_subagent_action("PermissionRequest", Some("rm -rf build")),
            SubagentAction::Set { status: SubagentStatus::Waiting, waiting_for: Some("approval: rm -rf build".into()) }
        );
        assert_eq!(status(&codex_subagent_action("SubagentStop", None)), Some(SubagentStatus::Finished));
        assert_eq!(codex_subagent_action("Stop", None), SubagentAction::Ignore);
    }

    #[test]
    fn codex_meta_names_the_subagent() {
        let line = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"x","agent_nickname":"Averroes","agent_path":"/root/list_files","base_instructions":"…"}}"#;
        assert_eq!(codex_subagent_meta(line), (Some("list files".into()), Some("Averroes".into())));
        let root = r#"{"type":"session_meta","payload":{"agent_path":"/root"}}"#;
        assert_eq!(codex_subagent_meta(root), (None, None));
        assert_eq!(codex_subagent_meta("{"), (None, None));
    }

    #[test]
    fn a_stop_while_its_own_shell_runs_is_a_pause() {
        // Paused on its background `sleep`: still at work.
        let pause = json!({ "hook_event_name": "SubagentStop", "background_tasks": [
            { "id": "b5bi0cfm3", "type": "shell", "status": "running", "command": "sleep 20; echo ok" }
        ]});
        assert!(waits_on_background(&pause, "a82"));
        // Lists only itself (what it carries until it reports): done.
        let done = json!({ "background_tasks": [{ "id": "a82", "type": "subagent", "status": "running" }] });
        assert!(!waits_on_background(&done, "a82"));
        assert!(!waits_on_background(&json!({ "background_tasks": [] }), "a82"));
        assert!(!waits_on_background(&json!({}), "a82"));
    }

    #[test]
    fn background_tasks_keeps_subagents_only() {
        let payload = json!({ "background_tasks": [
            { "id": "a1", "type": "subagent", "status": "running", "description": "Map the hooks" },
            { "id": "b1", "type": "shell", "status": "running" },
            { "type": "subagent" },
        ]});
        assert_eq!(background_tasks(&payload), vec![BackgroundTask { id: "a1".into(), description: Some("Map the hooks".into()) }]);
        assert!(background_tasks(&json!({})).is_empty());
    }

    #[test]
    fn stop_closes_what_it_does_not_list() {
        let fg = sub("fg", SubagentStatus::Running, 1, 2); // interrupted: no SubagentStop
        let bg = sub("bg", SubagentStatus::Running, 1, 2); // still running in the background
        let done = sub("done", SubagentStatus::Finished, 1, 2); // already closed: untouched
        let still = [BackgroundTask { id: "bg".into(), description: Some("Run the suite".into()) }];
        let changed = settle_on_stop(&[fg, bg, done], &still);
        assert_eq!(changed.len(), 2);
        assert_eq!((changed[0].id.as_str(), changed[0].status), ("fg", SubagentStatus::Finished));
        assert_eq!(changed[1].id, "bg");
        assert_eq!(changed[1].status, SubagentStatus::Running);
        assert_eq!(changed[1].description.as_deref(), Some("Run the suite"));
        assert!(changed[1].background);
    }

    #[test]
    fn visible_orders_by_attention_and_drops_old_finished() {
        let now = at(10_000);
        let waiting = sub("w", SubagentStatus::Waiting, 100, 9_000);
        let old_run = sub("r1", SubagentStatus::Running, 100, 100); // running never expires
        let new_run = sub("r2", SubagentStatus::Running, 200, 200);
        let recent = sub("f1", SubagentStatus::Finished, 300, 9_900);
        let stale = sub("f2", SubagentStatus::Finished, 50, 1_000);
        let ids: Vec<String> =
            visible(&[stale, recent, old_run, waiting, new_run], now).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["w", "r2", "r1", "f1"]);
    }

    #[test]
    fn visible_caps_finished() {
        let now = at(10_000);
        let subs: Vec<Subagent> = (0..6).map(|i| sub(&format!("f{i}"), SubagentStatus::Finished, 9_000 + i, 9_500)).collect();
        let ids: Vec<String> = visible(&subs, now).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["f5", "f4", "f3"]);
    }

    #[test]
    fn prunes_only_long_finished() {
        let now = at(100_000);
        assert!(prunable(&sub("a", SubagentStatus::Finished, 1, 1), now));
        assert!(!prunable(&sub("b", SubagentStatus::Finished, 1, 99_000), now));
        assert!(!prunable(&sub("c", SubagentStatus::Running, 1, 1), now));
    }

    #[test]
    fn transcript_and_meta_paths_follow_the_session() {
        let t = Path::new("/h/.claude/projects/-w/abc.jsonl");
        let p = claude_transcript_path(t, "a67").unwrap();
        assert_eq!(p, Path::new("/h/.claude/projects/-w/abc/subagents/agent-a67.jsonl"));
        assert_eq!(claude_meta_path(&p).unwrap(), Path::new("/h/.claude/projects/-w/abc/subagents/agent-a67.meta.json"));
    }

    #[test]
    fn meta_gives_description_and_shape() {
        let text = r#"{"agentType":"Explore","description":"Map hooks","toolUseId":"t","spawnDepth":1,"requestShape":"background"}"#;
        assert_eq!(parse_meta(text), (Some("Map hooks".into()), true));
        assert_eq!(parse_meta(r#"{"agentType":"Explore"}"#), (None, false));
        assert_eq!(parse_meta("nope"), (None, false));
    }

    #[test]
    fn prompt_labels_are_one_short_line() {
        assert_eq!(prompt_label("Task: run the tests\nand report", 40).as_deref(), Some("run the tests"));
        assert_eq!(prompt_label("\n  fix   the build  ", 40).as_deref(), Some("fix the build"));
        assert_eq!(prompt_label("abcdefghij", 5).as_deref(), Some("abcd…"));
        assert_eq!(prompt_label("  \n ", 5), None);
    }

    #[test]
    fn label_prefers_description() {
        let mut s = sub("x", SubagentStatus::Running, 1, 1);
        assert_eq!(s.label(), "Explore");
        s.description = Some("Map hooks".into());
        assert_eq!(s.label(), "Map hooks");
    }
}
