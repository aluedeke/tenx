//! `tenx service -- <command>`: a long-running process an agent starts for a
//! task — a dev server, `tilt up`, a watch build — that runs until someone
//! stops it. The agent starts it with its usual background shell, wrapped in
//! `tenx service`, which says what it is:
//!
//! - it doesn't keep the task working: a `Stop` whose background tasks are
//!   only services ends the turn ([`is_service_command`],
//!   `crate::session_event::claude_stop_background`), where any other
//!   background script — a build, a test run, a `gh run watch` — is work the
//!   agent waits for;
//! - it is listed in the sidebar, with its ports ([`Service`], `Live::services`);
//! - it is stopped when its task's window closes or the task is deleted.
//!
//! The registry is one `~/.config/tenx/services/<pid>.json` per running
//! wrapper ([`ServiceRecord`]), as `tenx pr wait`'s is.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One running `tenx service`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRecord {
    /// The `tenx service` process.
    pub pid: u32,
    /// The command it runs, leader of a process group of its own: stopping
    /// the service signals that whole group.
    pub child: u32,
    pub name: String,
    pub command: String,
    /// Where it was started — the agent's directory, inside its task.
    pub cwd: PathBuf,
    /// Unix seconds.
    pub started: u64,
}

/// A service as the sidebar shows it (`Live::services`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub command: String,
    /// Unix seconds.
    pub started: u64,
    /// Listening TCP ports of its processes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
}

/// What a Claude Code session inside tenx is told when it starts (the
/// `SessionStart` hook's `additionalContext`): which background tool means
/// what, so that its task shows as working exactly while it waits for
/// something.
pub const CLAUDE_CONTEXT: &str = "\
This session runs in a tenx task. tenx shows the task as working while you wait for background work, and as done when your turn is over. Pick the tool by what you start:
- Work whose result you wait for (tests, a build, a script that processes data, `gh run watch --exit-status`): Bash with run_in_background.
- A stream of events you react to (CI checks one by one, new lines in a log): the Monitor tool.
- A process that must stay up until someone stops it (a dev server, `tilt up`, `npm run dev`, a watch build): Bash with run_in_background, wrapped as `tenx service [--name NAME] -- <command>`. Your turn then ends normally, the task's sidebar lists the service with its ports, and tenx stops it when the task's window closes. `tenx service list` and `tenx service stop [NAME]` manage them.";

/// The `SessionStart` hook output that hands Claude Code [`CLAUDE_CONTEXT`].
pub fn claude_session_start_output() -> String {
    serde_json::json!({
        "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": CLAUDE_CONTEXT }
    })
    .to_string()
}

/// Whether a background command line is a `tenx service` — what a Claude
/// `Stop`'s `background_tasks` entry names. Its own subcommands (`list`,
/// `stop`) end at once and never get that far.
pub fn is_service_command(command: &str) -> bool {
    command.split_whitespace().collect::<Vec<_>>().windows(2).any(|w| w[0].ends_with("tenx") && w[1] == "service")
}

/// The services started from inside `task_dir`, oldest first.
pub fn in_task<'a>(records: &'a [ServiceRecord], task_dir: &Path) -> Vec<&'a ServiceRecord> {
    let mut out: Vec<&ServiceRecord> = records.iter().filter(|r| r.cwd.starts_with(task_dir)).collect();
    out.sort_by_key(|r| r.started);
    out
}

/// A service's name when none is given: the program's file name (`tilt`,
/// `npm`), else `service`.
pub fn default_name(command: &[String]) -> String {
    let first = command.first().map(|c| c.split_whitespace().next().unwrap_or("")).unwrap_or("");
    let name = Path::new(first).file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.is_empty() { "service".to_string() } else { name.to_string() }
}

/// The record whose name or pid is `which`, among a task's services.
pub fn find<'a>(services: &[&'a ServiceRecord], which: &str) -> Option<&'a ServiceRecord> {
    services.iter().copied().find(|r| r.name == which || r.pid.to_string() == which || r.child.to_string() == which)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: u32, name: &str, cwd: &str, started: u64) -> ServiceRecord {
        ServiceRecord { pid, child: pid + 1, name: name.into(), command: name.into(), cwd: cwd.into(), started }
    }

    #[test]
    fn a_service_is_told_from_other_background_commands() {
        assert!(is_service_command("tenx service -- tilt up"));
        assert!(is_service_command("cd monorepo && tenx service --name web -- npm run dev > /tmp/x.log 2>&1"));
        assert!(is_service_command("/opt/homebrew/bin/tenx service -- tilt up"));
        assert!(!is_service_command("tilt up"));
        assert!(!is_service_command("tenx pr wait 12"));
        assert!(!is_service_command("echo service"));
    }

    #[test]
    fn a_tasks_services_are_the_ones_started_inside_it() {
        let recs = vec![rec(3, "web", "/ws/tasks/a/app", 20), rec(1, "tilt", "/ws/tasks/a", 10), rec(5, "x", "/ws/tasks/ab", 5)];
        let names: Vec<&str> = in_task(&recs, Path::new("/ws/tasks/a")).iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["tilt", "web"]);
        assert!(in_task(&recs, Path::new("/ws/tasks/b")).is_empty());
        let mine = in_task(&recs, Path::new("/ws/tasks/a"));
        assert_eq!(find(&mine, "web").map(|r| r.pid), Some(3));
        assert_eq!(find(&mine, "2").map(|r| r.name.as_str()), Some("tilt"));
        assert!(find(&mine, "x").is_none());
    }

    #[test]
    fn claude_is_told_which_tool_to_use_when_it_starts() {
        let out: serde_json::Value = serde_json::from_str(&claude_session_start_output()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "SessionStart");
        let text = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(text.contains("tenx service") && text.contains("Monitor") && text.contains("run_in_background"));
    }

    #[test]
    fn a_service_is_named_after_its_program() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(default_name(&s(&["tilt", "up"])), "tilt");
        assert_eq!(default_name(&s(&["/usr/local/bin/npm run dev"])), "npm");
        assert_eq!(default_name(&[]), "service");
    }
}
