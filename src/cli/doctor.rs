//! `tenx doctor`: a quick health check of the agent integrations, so a task
//! reading the wrong state has one place to explain itself. Reports, per agent,
//! whether its binary is on PATH and its version, and whether tenx's session
//! integration is installed; then the tmux options the non-Claude TUIs need;
//! then any pane running an agent that isn't reporting to tenx's registry
//! (usually a Codex hook awaiting its one-time `/hooks` trust).

use crate::agent::AgentKind;
use anyhow::Result;

pub fn run(reset_skills: bool) -> Result<()> {
    println!("tenx doctor\n");

    println!("Agents:");
    for kind in AgentKind::all() {
        let on_path = crate::cli::session_event::agent_on_path(kind);
        let version = if on_path { agent_version(kind) } else { None };
        let installed = crate::cli::session_event::is_installed(kind).unwrap_or(false);
        let bin = kind.default_bin();
        if !on_path {
            println!("  {:<7} not on PATH", kind.as_str());
            continue;
        }
        let ver = version.unwrap_or_else(|| "?".to_string());
        let integ = if installed {
            "integration installed".to_string()
        } else {
            format!("integration NOT installed — run: tenx agent setup {}", kind.as_str())
        };
        println!("  {:<7} {bin} {ver} · {integ}", kind.as_str());
        if kind == AgentKind::Codex && installed {
            println!("          (Codex needs a one-time trust: start codex, run /hooks, trust the tenx hook)");
        }
    }

    println!("\nSession registry: {}", registry_summary());

    println!("\ntmux server:");
    if crate::tmux::server_running() {
        report_tmux_option("extended-keys", "on");
        report_tmux_option("allow-passthrough", "on");
    } else {
        println!("  not running (start it with: tenx)");
    }

    println!("\nAgent panes:");
    report_agent_panes();

    println!("\nWorkspace skills:");
    report_skills(reset_skills)?;

    Ok(())
}

/// Refresh every registered workspace's installed skills (the same pass a
/// launch runs) and say where each stands; with `reset`, first replace the
/// edited ones (`cli::init::reset_skills`).
fn report_skills(reset: bool) -> Result<()> {
    use tenx_core::skills::SkillState;
    let workspaces = crate::workspace::registered_workspaces();
    let mut edited_any = false;
    for ws in &workspaces {
        if reset {
            for path in crate::cli::init::reset_skills(&ws.dir)? {
                println!("  {:<16} replaced {} (yours kept as .orig)", ws.config.name, rel(&ws.dir, &path));
            }
        }
        let found = crate::cli::init::refresh_skills(&ws.dir);
        if found.is_empty() {
            println!("  {:<16} none installed", ws.config.name);
            continue;
        }
        let updated: Vec<String> = found.iter().filter(|(_, _, u)| *u).map(|(p, _, _)| rel(&ws.dir, p)).collect();
        let edited: Vec<String> =
            found.iter().filter(|(_, s, _)| *s == SkillState::Edited).map(|(p, _, _)| rel(&ws.dir, p)).collect();
        let failed = found.iter().any(|(_, s, u)| *s == SkillState::Stale && !u);
        let mut line = if updated.is_empty() { "current ✓".to_string() } else { format!("updated {}", updated.join(", ")) };
        if !edited.is_empty() {
            edited_any = true;
            line.push_str(&format!(" · edited, left alone: {}", edited.join(", ")));
        }
        if failed {
            line.push_str(" · couldn't rewrite a stale file (permissions?)");
        }
        println!("  {:<16} {line}", ws.config.name);
    }
    if edited_any {
        println!("  (to take tenx's current version of an edited file: tenx doctor --reset-skills)");
    }
    Ok(())
}

fn rel(base: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

/// The agent's `--version`, first line, trimmed.
fn agent_version(kind: AgentKind) -> Option<String> {
    let out = crate::live::run_capture(kind.default_bin(), &["--version"]);
    out.lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

fn registry_summary() -> String {
    match crate::workspace::sessions::registry_dir() {
        Some(dir) => {
            let n = std::fs::read_dir(&dir)
                .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
                .unwrap_or(0);
            format!("{n} record(s) in {}", dir.display())
        }
        None => "no $HOME".to_string(),
    }
}

fn report_tmux_option(name: &str, want: &str) {
    let got = crate::tmux::show_global_option(name);
    match got.as_deref() {
        Some(v) if v == want => println!("  {name} = {v} ✓"),
        Some(v) => println!("  {name} = {v} (expected {want}; restart: tmux -L tenx kill-server then tenx)"),
        None => println!("  {name} unset (expected {want}; restart: tmux -L tenx kill-server then tenx)"),
    }
}

/// Panes whose foreground process is a coding agent, and whether a session
/// record exists for that pane's pid. A running agent with no record is one
/// that isn't reporting — the state you see as a stuck `Idle`.
fn report_agent_panes() {
    let Ok(panes) = crate::tmux::list_pane_pids() else {
        println!("  (tmux not queryable)");
        return;
    };
    let mut any = false;
    for (_, pid) in panes {
        let Some(comm) = comm_of(pid) else { continue };
        let Some(kind) = AgentKind::all().into_iter().find(|k| k.process_names().iter().any(|n| comm.contains(n))) else {
            continue;
        };
        any = true;
        let reporting = crate::workspace::sessions::read_record(pid).is_some();
        let note = if reporting { "reporting ✓" } else { "NOT reporting (setup/trust its hooks)" };
        println!("  pid {pid} · {} ({comm}) · {note}", kind.as_str());
    }
    if !any {
        println!("  (no agent panes)");
    }
}

fn comm_of(pid: u32) -> Option<String> {
    let out = crate::live::run_capture("ps", &["-o", "comm=", "-p", &pid.to_string()]);
    let s = out.trim();
    (!s.is_empty()).then(|| {
        // ps prints the full path; the basename is enough to match a name.
        std::path::Path::new(s).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.to_string())
    })
}
