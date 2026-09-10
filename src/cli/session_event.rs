//! `tenx internal session-event --agent <kind>`: the sink every agent's hook
//! calls to report a lifecycle event. Reads the hook's JSON payload from stdin,
//! finds the agent process's pid, maps the event through
//! `tenx_core::session_event`, and writes/deletes the session record
//! (`workspace::sessions`). It is the one writer of that registry.
//!
//! Invariants: it prints **nothing** to stdout (Claude's `PermissionRequest`
//! reads a hook's stdout as a decision, and Codex the same), and it exits 0 no
//! matter what — a hook that errors or hangs must never disturb the agent.

use crate::agent::AgentKind;
use crate::workspace::sessions;
use serde_json::Value;
use tenx_core::session_event::{claude_action, codex_action, pi_action, SessionAction};

/// Max levels to climb from the hook's parent looking for the agent process.
const MAX_HOPS: u32 = 8;

pub fn run(agent_token: &str, pid_override: Option<u32>) {
    // Best-effort throughout: any early return is a silent success.
    let _ = try_run(agent_token, pid_override);
}

fn try_run(agent_token: &str, pid_override: Option<u32>) -> Option<()> {
    let agent = AgentKind::from_token(agent_token);
    let mut input = String::new();
    use std::io::Read;
    std::io::stdin().read_to_string(&mut input).ok()?;
    let payload: Value = serde_json::from_str(input.trim()).ok()?;

    let event = payload.get("hook_event_name").and_then(|v| v.as_str())?;
    let tool_name = payload.get("tool_name").and_then(|v| v.as_str());
    let notification_type = payload.get("notification_type").and_then(|v| v.as_str());
    let message = payload.get("message").and_then(|v| v.as_str());
    let command = payload.get("tool_input").and_then(|t| t.get("command")).and_then(|v| v.as_str());
    // Subagent events carry an agent_id; they must not move the session record.
    let is_subagent = payload.get("agent_id").is_some();

    let action = match agent {
        AgentKind::Claude => claude_action(event, tool_name, notification_type, message, is_subagent),
        AgentKind::Codex => codex_action(event, command, is_subagent),
        AgentKind::Pi => pi_action(event, message),
    };
    if matches!(action, SessionAction::Ignore) {
        return Some(());
    }

    // pi knows its own pid and passes it; a hook (Claude, Codex) doesn't, so it
    // climbs from its parent to the agent process.
    let pid = pid_override.unwrap_or_else(|| resolve_agent_pid(agent));

    match action {
        SessionAction::Delete => sessions::delete_record(pid),
        SessionAction::Set { status, waiting_for } => {
            let mut record = sessions::read_record(pid).unwrap_or_default();
            record.pid = Some(pid);
            record.agent = Some(agent.as_str().to_string());
            record.status = Some(status.token().to_string());
            record.waiting_for = waiting_for;
            record.status_updated_at = Some(sessions::now_millis());
            record.kind = Some(kind_for(pid)).filter(|k| !k.is_empty()).unwrap_or_else(|| "interactive".to_string());
            if let Some(cwd) = payload.get("cwd").and_then(|v| v.as_str()) {
                record.cwd = Some(cwd.to_string());
            }
            if let Some(id) = payload.get("session_id").and_then(|v| v.as_str()) {
                record.session_id = Some(id.to_string());
            }
            if let Some(tp) = payload.get("transcript_path").and_then(|v| v.as_str()) {
                record.transcript_path = Some(tp.to_string());
            }
            if let Some(mode) = payload.get("permission_mode").and_then(|v| v.as_str()) {
                record.permission_mode = Some(mode.to_string());
            }
            if let Ok(pane) = std::env::var("TMUX_PANE") {
                // The hook runs in the agent's pane; keep it for the overlay's
                // approve-in-place preview. Stored as a bare pane id.
                if !pane.is_empty() {
                    record.tmux = Some(pane);
                }
            }
            let _ = sessions::write_record(pid, &record);
        }
        SessionAction::Ignore => {}
    }
    Some(())
}

/// The pid to key this session's record by: climb from the hook's parent to the
/// first process whose command names the agent. Codex execs the hook directly,
/// so its parent is already `codex`; Claude runs it via a shell, so the walk
/// steps through that. Falls back to the immediate parent if no agent process
/// is found within the hop budget.
fn resolve_agent_pid(agent: AgentKind) -> u32 {
    let parent = parent_pid();
    let comm_of = |pid: u32| comm_of(pid);
    let ppid_of = |pid: u32| ppid_of(pid);
    sessions::find_agent_pid(parent, agent.process_names(), MAX_HOPS, &comm_of, &ppid_of).unwrap_or(parent)
}

fn parent_pid() -> u32 {
    unsafe { libc::getppid() as u32 }
}

/// `interactive` when the pid is one of the tenx server's own pane processes
/// (the visible agent), else `bg` (a `-p`/`exec` agent under a task subdir) —
/// the distinction the watcher's agent-pane rule and `TaskState::agents` use.
fn kind_for(pid: u32) -> String {
    let pane_pids: Vec<u32> = crate::tmux::list_pane_pids().unwrap_or_default().into_iter().map(|(_, p)| p).collect();
    if pane_pids.contains(&pid) { "interactive".to_string() } else { "bg".to_string() }
}

fn comm_of(pid: u32) -> Option<String> {
    let out = crate::live::run_capture("ps", &["-o", "comm=", "-p", &pid.to_string()]);
    let s = out.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn ppid_of(pid: u32) -> Option<u32> {
    let out = crate::live::run_capture("ps", &["-o", "ppid=", "-p", &pid.to_string()]);
    out.trim().parse().ok()
}

// ── `tenx agent setup <kind>` and auto-setup ──────────────────────────────────

const HOOK_TIMEOUT_SECS: u64 = 5;

/// Claude Code hook events tenx subscribes to — chosen so every status
/// transition and every dialog outcome is reported (`session_event::claude_action`).
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse",
    "PostToolUseFailure", "PermissionDenied", "Notification", "Elicitation", "ElicitationResult",
    "Stop", "StopFailure", "SessionEnd",
];

/// Codex CLI hook events tenx subscribes to (`session_event::codex_action`).
const CODEX_EVENTS: &[&str] =
    &["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse", "Stop", "Interrupt", "SessionEnd"];

/// The pi extension source, embedded so `tenx` is self-contained.
const PI_EXTENSION: &str = include_str!("../agent/pi/tenx.ts");

fn claude_command() -> String {
    "tenx internal session-event --agent claude".to_string()
}
fn codex_command() -> String {
    "tenx internal session-event --agent codex".to_string()
}

fn home() -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context;
    Ok(std::path::PathBuf::from(std::env::var("HOME").context("$HOME not set")?))
}

/// `tenx agent setup <kind>` (or `--check`): install the integration that feeds
/// tenx's session registry. `kind` is `claude`/`codex`/`pi`, or `all`.
pub fn setup(token: &str, check: bool) -> anyhow::Result<()> {
    if token.trim() == "all" {
        for kind in AgentKind::all() {
            setup_one(kind, check)?;
        }
        return Ok(());
    }
    setup_one(AgentKind::from_token(token), check)
}

fn setup_one(kind: AgentKind, check: bool) -> anyhow::Result<()> {
    if check {
        let installed = is_installed(kind)?;
        println!("{}: integration {}", kind.as_str(), if installed { "installed" } else { "NOT installed" });
        return Ok(());
    }
    let changed = match kind {
        AgentKind::Claude => install_claude()?,
        AgentKind::Codex => install_codex()?,
        AgentKind::Pi => install_pi()?,
    };
    match kind {
        AgentKind::Claude => {
            eprintln!("✓ claude: session hooks {} ~/.claude/settings.json", if changed { "installed into" } else { "already in" });
            eprintln!("  Picked up on Claude Code's next start. User-level hooks need no trust step.");
        }
        AgentKind::Codex => {
            eprintln!("✓ codex: session hooks {} ~/.codex/hooks.json", if changed { "installed into" } else { "already in" });
            eprintln!("  One-time trust step: start codex, run /hooks, and trust the tenx hook.");
        }
        AgentKind::Pi => {
            eprintln!("✓ pi: extension {} ~/.pi/agent/extensions/tenx.ts", if changed { "installed at" } else { "already at" });
        }
    }
    Ok(())
}

/// Whether an agent's integration is already in place — the `--check`,
/// auto-setup, and `doctor` query, side-effect free.
pub fn is_installed(kind: AgentKind) -> anyhow::Result<bool> {
    let home = home()?;
    Ok(match kind {
        AgentKind::Claude => read_json(&home.join(".claude/settings.json"))
            .is_some_and(|root| tenx_core::session_event::has_command_hook(&root, &claude_command())),
        AgentKind::Codex => read_json(&home.join(".codex/hooks.json"))
            .is_some_and(|root| tenx_core::session_event::has_command_hook(&root, &codex_command())),
        AgentKind::Pi => {
            let path = home.join(".pi/agent/extensions/tenx.ts");
            std::fs::read_to_string(&path).ok().and_then(|c| integration_version(&c)) == integration_version(PI_EXTENSION)
                && integration_version(PI_EXTENSION).is_some()
        }
    })
}

fn read_json(path: &std::path::Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn install_claude() -> anyhow::Result<bool> {
    merge_hook_file(&home()?.join(".claude/settings.json"), CLAUDE_EVENTS, &claude_command())
}

fn install_codex() -> anyhow::Result<bool> {
    // Codex reads a top-level `hooks` map from hooks.json, the same shape
    // ensure_command_hooks writes.
    merge_hook_file(&home()?.join(".codex/hooks.json"), CODEX_EVENTS, &codex_command())
}

/// Merge tenx's command hooks into a JSON hooks file (Claude settings or Codex
/// hooks.json), creating it if needed. Idempotent; returns whether it changed.
fn merge_hook_file(path: &std::path::Path, events: &[&str], command: &str) -> anyhow::Result<bool> {
    use anyhow::Context;
    let mut root: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?,
        Err(_) => serde_json::json!({}),
    };
    let changed = tenx_core::session_event::ensure_command_hooks(&mut root, events, command, HOOK_TIMEOUT_SECS);
    if changed {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, format!("{root:#}\n")).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(changed)
}

fn install_pi() -> anyhow::Result<bool> {
    use anyhow::Context;
    let path = home()?.join(".pi/agent/extensions/tenx.ts");
    let current = std::fs::read_to_string(&path).ok().and_then(|c| integration_version(&c));
    let ours = integration_version(PI_EXTENSION);
    if current == ours && ours.is_some() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, PI_EXTENSION).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// Parse `TENX_INTEGRATION_VERSION=<n>` from an extension's header, so a
/// reinstall only overwrites when the shipped version differs.
fn integration_version(source: &str) -> Option<u32> {
    source.lines().find_map(|l| l.split("TENX_INTEGRATION_VERSION=").nth(1)?.trim().parse().ok())
}

/// Install every not-yet-installed integration for the agents on PATH, once.
///
/// Guarded by a sentinel so it runs a single time per machine, not on every
/// `tenx` launch, and so a user who later removes an integration on purpose
/// isn't fought. `tenx agent setup` (manual) always acts regardless. Best-effort
/// and quiet: a failure here must not disturb launching the session.
pub fn auto_setup() {
    let Ok(home) = home() else { return };
    let sentinel = home.join(".config/tenx/.agent-setup-done");
    if sentinel.exists() {
        return;
    }
    for kind in AgentKind::all() {
        if on_path(kind.process_names()[0]) && !is_installed(kind).unwrap_or(true) {
            let _ = match kind {
                AgentKind::Claude => install_claude(),
                AgentKind::Codex => install_codex(),
                AgentKind::Pi => install_pi(),
            };
        }
    }
    if let Some(parent) = sentinel.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&sentinel, "");
}

/// Whether an agent's binary is on `$PATH` — for `tenx init`'s setup prompt.
pub fn agent_on_path(kind: AgentKind) -> bool {
    on_path(kind.process_names()[0])
}

/// Whether `name` is an executable on `$PATH`.
fn on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(name);
        std::fs::metadata(&p).map(|m| m.is_file()).unwrap_or(false)
    })
}
