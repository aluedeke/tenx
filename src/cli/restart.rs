//! `tenx agent restart`: restart the coding agent in every open task window,
//! so a new agent version or a changed agent config takes effect, without
//! losing what tenx shows. The rules are `tenx_core::restart`; this is the
//! tmux and registry work around them.
//!
//! - **Which agents**: every interactive session in a pane of the tenx server
//!   whose window belongs to a task (`@tenx_task_dir`), any agent. Closed
//!   tasks need nothing: they start the new version when next opened.
//! - **When**: each task as soon as it is quiet — a running turn or an open
//!   prompt is waited out (`--now` skips that). The agent that runs this
//!   command (an agent asked to restart everything) goes last, from a
//!   detached `tmux run-shell`, since respawning its pane ends this process.
//! - **How**: `tmux respawn-pane -k` on the agent's pane with the agent's
//!   resume command for its session id (`agent::launch_resume`). The window,
//!   its other panes, its layout and its bell flag are untouched.
//! - **After**: the new session's record takes over the old status time for a
//!   task that was done, and an agent cut off mid-turn (or whose `tenx pr
//!   wait` died with it) is told to carry on.

use crate::agent::AgentKind;
use crate::workspace::{self, Workspace, sessions};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};
use tenx_core::status::TaskStatus;

/// Default for `--timeout`: long enough for a long turn to finish.
pub const DEFAULT_TIMEOUT: &str = "30m";

/// How long a respawned agent gets to report in before we give up on it.
const REPORT_LIMIT: Duration = Duration::from_secs(30);

/// One agent to restart.
struct Target {
    /// Index into the workspace list [`targets`] returns.
    ws: usize,
    slug: String,
    title: String,
    task_dir: PathBuf,
    pane: String,
    pid: u32,
    agent: AgentKind,
    session_id: Option<String>,
    transcript: Option<String>,
    /// This process runs inside that agent (it asked for the restart).
    is_self: bool,
}

/// Everything needed to respawn one pane and follow it up — handed to
/// `tenx internal respawn-agent` as JSON when this process can't do it
/// itself (its own agent's pane).
#[derive(Debug, Serialize, Deserialize)]
pub struct Plan {
    pub pane: String,
    pub cwd: String,
    pub command: String,
    pub old_pid: u32,
    /// Typed into the new agent once it has reported in.
    pub message: Option<String>,
    /// The old session's `statusUpdatedAt`, kept while the new one is idle.
    pub carry_ms: Option<u64>,
}

/// `tenx agent restart [TASK]... [--now] [--timeout] [--dry-run]`.
pub fn run(only: &[String], now: bool, timeout: Duration, dry_run: bool) -> Result<()> {
    if !crate::tmux::server_running() {
        bail!("the '{}' session isn't running — nothing to restart", crate::tmux::SESSION);
    }
    let (workspaces, mut pending) = targets(only);
    if pending.is_empty() {
        println!("no agent is running in an open task window");
        return Ok(());
    }
    let waits = crate::cli::prwatch::waits();

    if dry_run {
        let sessions = sessions::sessions();
        let signals = crate::tmux::signals();
        for t in &pending {
            let st = workspace::resolve_task_state(&t.task_dir, &sessions, &signals);
            let when = tenx_core::restart::hold_reason(st.status, st.waiting_for.as_deref(), t.is_self, now)
                .map_or_else(|| "now".to_string(), |why| format!("once it stops {why}"));
            println!("would restart {} ({}) {when}: {}", t.title, t.agent.as_str(), command(&workspaces, t));
        }
        return Ok(());
    }

    let start = Instant::now();
    let mut shown: Option<String> = None;
    loop {
        let sessions = sessions::sessions();
        let signals = crate::tmux::signals();
        let mut held = Vec::new();
        let mut still = Vec::new();
        for t in pending {
            if t.is_self {
                still.push(t);
                continue;
            }
            let st = workspace::resolve_task_state(&t.task_dir, &sessions, &signals);
            match tenx_core::restart::hold_reason(st.status, st.waiting_for.as_deref(), false, now) {
                Some(why) => {
                    held.push(format!("{} ({why})", t.title));
                    still.push(t);
                }
                None => restart_now(&workspaces, &t, st.status, st.changed, &waits),
            }
        }
        pending = still;
        if held.is_empty() {
            break;
        }
        let line = format!("waiting for {}", held.join(", "));
        if shown.as_deref() != Some(&line) {
            println!("{line}");
            shown = Some(line);
        }
        if start.elapsed() >= timeout {
            let message = format!("gave up waiting; not restarted: {}. Re-run, or use --now", held.join(", "));
            return Err(crate::cli::secrets::Exit { code: 3, message }.into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    // Last: the agent this command runs in. Respawning its pane kills this
    // process, so the respawn and its follow-up run from the server.
    for t in pending {
        let sessions = sessions::sessions();
        let signals = crate::tmux::signals();
        let st = workspace::resolve_task_state(&t.task_dir, &sessions, &signals);
        let plan = plan_for(&workspaces, &t, st.status, st.changed, &waits);
        let json = serde_json::to_string(&plan)?;
        let bin = crate::tmux::self_bin()?;
        let log = log_path();
        let shell = format!(
            "{} internal respawn-agent {} >/dev/null 2>>{}",
            crate::tmux::shell_quote(&bin.to_string_lossy()),
            crate::tmux::shell_quote(&json),
            crate::tmux::shell_quote(&log.to_string_lossy()),
        );
        println!("restarting {} (this agent) — it continues on its own", t.title);
        crate::tmux::run_shell_background(&shell)?;
    }
    Ok(())
}

/// Respawn one task's agent from this process and report how it went.
fn restart_now(workspaces: &[Workspace], t: &Target, status: TaskStatus, changed: Option<std::time::SystemTime>, waits: &[tenx_core::prwatch::WaitRecord]) {
    let plan = plan_for(workspaces, t, status, changed, waits);
    match respawn(&plan) {
        Ok(true) => println!("restarted {} ({})", t.title, t.agent.as_str()),
        Ok(false) => println!("restarted {} ({}) — it hasn't reported back yet; check its window", t.title, t.agent.as_str()),
        Err(e) => eprintln!("! {}: {e:#}", t.title),
    }
}

fn plan_for(workspaces: &[Workspace], t: &Target, status: TaskStatus, changed: Option<std::time::SystemTime>, waits: &[tenx_core::prwatch::WaitRecord]) -> Plan {
    let watching = crate::cli::prwatch::watched_in(waits, &t.task_dir);
    let changed_ms = changed.and_then(|c| c.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
    Plan {
        pane: t.pane.clone(),
        cwd: crate::tmux::pane_start_path(&t.pane).unwrap_or_else(|| t.task_dir.to_string_lossy().into_owned()),
        command: command(workspaces, t),
        old_pid: t.pid,
        message: tenx_core::restart::resume_message(status, t.is_self, &watching),
        carry_ms: if status == TaskStatus::Done { changed_ms } else { None },
    }
}

/// The agent's resume command, built like a fresh launch would be (so a
/// changed `[agents.<kind>]` config lands), resuming the running session — if
/// its transcript is still on disk; a resume of a missing one would exit, and
/// in an agent-only window take the whole window with it.
fn command(workspaces: &[Workspace], t: &Target) -> String {
    let ws = &workspaces[t.ws];
    let transcript = t.transcript.as_deref().filter(|p| Path::new(p).is_file());
    let session_id = t.session_id.as_deref().filter(|_| transcript.is_some());
    let add_dirs = ws.find_task(&t.slug).map(|task| crate::cli::task::readable_dirs(ws, &task)).unwrap_or_default();
    crate::agent::launch_resume(ws, t.agent, &t.slug, &t.task_dir, &add_dirs, session_id, transcript)
}

/// Respawn `plan.pane` and follow up once the new agent reports in. Returns
/// whether it did report in within [`REPORT_LIMIT`].
pub fn respawn(plan: &Plan) -> Result<bool> {
    crate::tmux::respawn_pane(&plan.pane, &plan.cwd, &plan.command)?;
    let start = Instant::now();
    let (pid, mut record) = loop {
        if let Some(found) = sessions::record_for_pane(&plan.pane, plan.old_pid) {
            break found;
        }
        if start.elapsed() >= REPORT_LIMIT {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    sessions::delete_record(plan.old_pid);
    let status = record.status.clone().unwrap_or_default();
    if let Some(ms) = tenx_core::restart::carried_status_time(TaskStatus::Done, plan.carry_ms, &status) {
        record.status_updated_at = Some(ms);
        let _ = sessions::write_record(pid, &record);
    }
    if let Some(text) = &plan.message {
        // The session-start hook fires before the agent's input is ready.
        std::thread::sleep(Duration::from_secs(2));
        crate::tmux::paste_text(&plan.pane, text)?;
        std::thread::sleep(Duration::from_millis(200));
        crate::tmux::send_keys(&plan.pane, "Enter")?;
    }
    Ok(true)
}

/// `tenx internal respawn-agent <plan>`: [`respawn`] for a plan made by
/// another process.
pub fn respawn_from_json(json: &str) -> Result<()> {
    let plan: Plan = serde_json::from_str(json)?;
    if !respawn(&plan)? {
        bail!("{}: the restarted agent didn't report back", plan.pane);
    }
    Ok(())
}

fn log_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config/tenx/restart.log")
}

/// Every interactive agent in a task window, one per pane, filtered to the
/// named tasks (slug or title) when any are given.
fn targets(only: &[String]) -> (Vec<Workspace>, Vec<Target>) {
    let mine = ancestors(std::process::id());
    let workspaces = workspace::registered_workspaces();
    let tasks: Vec<(usize, workspace::Task)> = workspaces
        .iter()
        .enumerate()
        .flat_map(|(i, ws)| ws.tasks().unwrap_or_default().into_iter().map(move |t| (i, t)))
        .collect();
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in sessions::sessions() {
        if s.kind != "interactive" {
            continue;
        }
        let (Some(pane), Some(dir)) = (s.pane.clone(), s.task_dir.clone()) else { continue };
        if !seen.insert(pane.clone()) {
            continue;
        }
        let dir = canonical(&dir);
        let Some((ws, task)) = tasks.iter().find(|(_, t)| canonical(&t.path) == dir) else { continue };
        if !only.is_empty() && !only.iter().any(|n| *n == task.name || *n == task.display_name) {
            continue;
        }
        let record = sessions::read_record(s.pid);
        out.push(Target {
            ws: *ws,
            slug: task.name.clone(),
            title: task.display_name.clone(),
            task_dir: task.path.clone(),
            pane,
            pid: s.pid,
            agent: AgentKind::from_token(&s.agent),
            session_id: s.session_id.clone(),
            transcript: record.and_then(|r| r.transcript_path),
            is_self: mine.contains(&s.pid),
        });
    }
    (workspaces, out)
}

/// `pid` and every process above it.
fn ancestors(pid: u32) -> HashSet<u32> {
    let tree = tenx_core::live::parse_ps(&crate::live::run_capture("ps", &["-axo", "pid=,ppid="]));
    let mut out = HashSet::new();
    let mut at = pid;
    while out.insert(at) {
        match tree.iter().find(|(p, _)| *p == at) {
            Some((_, parent)) if *parent > 1 => at = *parent,
            _ => break,
        }
    }
    out
}
