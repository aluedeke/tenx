//! `tenx tmux restart`: stop tenx's tmux server and build it again as it was —
//! for what `tenx tmux reload` can't reach (a new tmux, a setting that only
//! applies at server start). The rules per pane are `tenx_core::restart`
//! (`pane_role`, `stopped_program`); this is the tmux work.
//!
//! 1. **Wait** until no task is working or waiting on a prompt (`--now` skips),
//!    exactly as `tenx agent restart` does — every agent restarts here too.
//! 2. **Snapshot** the session: every window in order with its name, task,
//!    layout and zoom; every pane's directory and what it runs; each agent's
//!    resume plan (`cli::restart::plan_for`); the server's environment, so
//!    the new server's panes inherit what the old one's did rather than the
//!    environment of whoever ran this.
//! 3. **Rebuild**, in a detached process (`tenx internal tmux-rebuild`), since
//!    killing the server kills whatever pane this command runs in: stop the
//!    watcher (one that saw every window vanish would delete their released
//!    secrets), kill the server, start a new one, recreate each window —
//!    agents resumed, layout programs (`nvim TASK.md`) run again, shells fresh
//!    in the same folder — then start the watcher and follow each agent up.
//!
//! While it runs, `tmux-restart.pid` marks it, and a `tenx` client whose
//! attach ended waits for it and attaches again (`tui::client`).

use crate::cli::restart::{self, Plan};
use crate::workspace::{self, sessions};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tenx_core::restart::{PaneRole, SHELLS, pane_role, stopped_program, unquote_start_command};

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    env: Vec<(String, String)>,
    size: (u32, u32),
    windows: Vec<WindowSnap>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WindowSnap {
    name: String,
    task_dir: Option<String>,
    layout: String,
    zoomed: bool,
    active: bool,
    /// A pane was left out (an agent-log follower), so the saved layout no
    /// longer fits the panes.
    dropped: bool,
    panes: Vec<PaneSnap>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PaneSnap {
    path: String,
    /// The shell string to run; `None` is a plain shell.
    command: Option<String>,
    active: bool,
    /// An agent's follow-up once it is back (message, done-since time).
    plan: Option<Plan>,
}

/// `tenx tmux restart [--now] [--timeout] [--dry-run]`.
pub fn run(now: bool, timeout: Duration, dry_run: bool) -> Result<()> {
    if !crate::tmux::server_running() {
        bail!("the '{}' session isn't running — start it with: tenx", crate::tmux::SESSION);
    }
    if in_progress() {
        bail!("a tmux restart is already running");
    }
    if !now && !dry_run {
        wait_until_quiet(timeout)?;
    }
    let (snap, stopped) = snapshot()?;
    let agents = snap.windows.iter().flat_map(|w| &w.panes).filter(|p| p.plan.is_some()).count();
    if dry_run {
        for w in &snap.windows {
            println!("{}", w.name);
            for p in &w.panes {
                let what = p.command.as_deref().unwrap_or("shell");
                println!("  {} — {what}", p.path);
            }
        }
    }
    println!("{} window(s), {agents} agent(s) to resume", snap.windows.len());
    for s in &stopped {
        println!("  stops {s} — start it again yourself");
    }
    if dry_run {
        return Ok(());
    }

    let path = snapshot_path()?;
    std::fs::write(&path, serde_json::to_string(&snap)?).with_context(|| format!("write {}", path.display()))?;
    spawn_rebuild(&path)?;
    println!("restarting tmux — a running `tenx` and `tenx web` tabs reattach by themselves");
    Ok(())
}

/// Block until no open task is working or waiting on a prompt — except the
/// one whose agent runs this command, which is busy doing so.
fn wait_until_quiet(timeout: Duration) -> Result<()> {
    let (_, targets) = restart::targets(&[]);
    let mine: Vec<PathBuf> = targets.iter().filter(|t| t.is_self).map(|t| t.task_dir.clone()).collect();
    let dirs: Vec<(String, PathBuf)> =
        targets.iter().filter(|t| !t.is_self).map(|t| (t.title.clone(), t.task_dir.clone())).collect();
    let start = Instant::now();
    let mut shown: Option<String> = None;
    loop {
        let sessions = sessions::sessions();
        let signals = crate::tmux::signals();
        let held: Vec<String> = dirs
            .iter()
            .filter(|(_, d)| !mine.contains(d))
            .filter_map(|(title, d)| {
                let st = workspace::resolve_task_state(d, &sessions, &signals);
                tenx_core::restart::hold_reason(st.status, st.waiting_for.as_deref(), false, false).map(|why| format!("{title} ({why})"))
            })
            .collect();
        if held.is_empty() {
            return Ok(());
        }
        let line = format!("waiting for {}", held.join(", "));
        if shown.as_deref() != Some(&line) {
            println!("{line}");
            shown = Some(line);
        }
        if start.elapsed() >= timeout {
            let message = format!("gave up waiting; nothing restarted: {}. Re-run, or use --now", held.join(", "));
            return Err(crate::cli::secrets::Exit { code: 3, message }.into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The session as it is now, plus the programs the restart will stop.
fn snapshot() -> Result<(Snapshot, Vec<String>)> {
    let (workspaces, targets) = restart::targets(&[]);
    let waits = crate::cli::prwatch::waits();
    let sessions = sessions::sessions();
    let signals = crate::tmux::signals();
    let plans: HashMap<String, Plan> = targets
        .iter()
        .map(|t| {
            let st = workspace::resolve_task_state(&t.task_dir, &sessions, &signals);
            (t.pane.clone(), restart::plan_for(&workspaces, t, st.status, st.changed, &waits))
        })
        .collect();
    let tasks: Vec<(usize, workspace::Task)> = workspaces
        .iter()
        .enumerate()
        .flat_map(|(i, ws)| ws.tasks().unwrap_or_default().into_iter().map(move |t| (i, t)))
        .collect();
    let canonical = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));

    let wfmt = "#{window_id}\t#{window_name}\t#{window_active}\t#{window_zoomed_flag}\t#{window_width}\t#{window_height}\t#{@tenx_task_dir}\t#{window_layout}";
    let pfmt = "#{window_id}\t#{pane_active}\t#{pane_current_path}\t#{pane_start_path}\t#{pane_current_command}\t#{pane_id}\t#{pane_start_command}";
    let session = crate::tmux::SESSION;
    let mut panes: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    for line in crate::tmux::run(&["list-panes", "-s", "-t", session, "-F", pfmt])?.lines() {
        let f: Vec<String> = line.splitn(7, '\t').map(str::to_string).collect();
        if f.len() == 7 {
            panes.entry(f[0].clone()).or_default().push(f);
        }
    }

    let mut windows = Vec::new();
    let mut stopped = Vec::new();
    let mut size = (0, 0);
    for line in crate::tmux::run(&["list-windows", "-t", session, "-F", wfmt])?.lines() {
        let f: Vec<&str> = line.splitn(8, '\t').collect();
        let [id, name, active, zoomed, w, h, task_dir, layout] = f[..] else { continue };
        // The home window comes back with the server; a transcript viewer
        // (`↳ …`) is opened again on demand.
        if name == crate::tmux::HOME_WINDOW || name.starts_with('↳') {
            continue;
        }
        size = (size.0.max(w.parse().unwrap_or(0)), size.1.max(h.parse().unwrap_or(0)));
        let task = (!task_dir.is_empty()).then(|| canonical(task_dir));
        let owner = task.as_ref().and_then(|d| tasks.iter().find(|(_, t)| canonical(&t.path.to_string_lossy()) == *d));
        let agent = owner.map(|(i, t)| crate::agent::agent_for(&workspaces[*i], &t.path));
        let agent_bin = match (owner, agent) {
            (Some((i, _)), Some(kind)) => crate::agent::configured(&workspaces[*i], kind).0,
            _ => String::new(),
        };
        let mut snap = WindowSnap {
            name: name.to_string(),
            task_dir: (!task_dir.is_empty()).then(|| task_dir.to_string()),
            layout: layout.to_string(),
            zoomed: zoomed == "1",
            active: active == "1",
            dropped: false,
            panes: Vec::new(),
        };
        for p in panes.get(id).map(Vec::as_slice).unwrap_or_default() {
            let (pane_active, cur_path, start_path, cur_cmd, pane_id, start_cmd) = (&p[1], &p[2], &p[3], &p[4], &p[5], &p[6]);
            let plan = plans.get(pane_id.as_str());
            let start = unquote_start_command(start_cmd);
            let role = pane_role(&start, plan.is_some(), &agent_bin);
            if let Some(prog) = stopped_program(&role, cur_cmd, SHELLS) {
                stopped.push(format!("{prog} in {name} ({cur_path})"));
            }
            let (path, command, plan) = match role {
                PaneRole::AgentLog => {
                    snap.dropped = true;
                    continue;
                }
                PaneRole::Agent => match (plan, owner, agent) {
                    (Some(plan), _, _) => (plan.cwd.clone(), Some(plan.command.clone()), Some(plan.clone())),
                    // Never reported in: the ordinary launch, with no first prompt.
                    (None, Some((i, t)), Some(kind)) => {
                        let ws = &workspaces[*i];
                        let dirs = crate::cli::task::readable_dirs(ws, t);
                        (start_path.clone(), Some(crate::agent::launch(ws, kind, &t.name, &t.path, &dirs, None)), None)
                    }
                    _ => (start_path.clone(), Some(start.clone()), None),
                },
                PaneRole::Program(cmd) => (start_path.clone(), Some(cmd), None),
                PaneRole::Shell => (cur_path.clone(), None, None),
            };
            let path = if path.is_empty() { cur_path.clone() } else { path };
            snap.panes.push(PaneSnap { path, command, active: pane_active == "1", plan });
        }
        if !snap.panes.is_empty() {
            windows.push(snap);
        }
    }
    Ok((Snapshot { env: crate::tmux::server_environment(), size, windows }, stopped))
}


/// Start `tenx internal tmux-rebuild <snapshot>` in a process of its own
/// session, outliving the server and the pane this command runs in; its
/// output goes to the restart log.
fn spawn_rebuild(snapshot: &std::path::Path) -> Result<()> {
    let log = std::fs::OpenOptions::new().create(true).append(true).open(restart::log_path())?;
    let mut cmd = Command::new(crate::tmux::self_bin()?);
    cmd.args(["internal", "tmux-rebuild"])
        .arg(snapshot)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Double fork + setsid, as the watcher is started: reparented to init, and
    // no SIGHUP when the terminal or pane that ran us goes away.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::fork() > 0 {
                libc::_exit(0);
            }
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?.wait()?;
    Ok(())
}

/// `tenx internal tmux-rebuild <snapshot>`: the detached half of the restart.
pub fn rebuild(snapshot: &str) -> Result<()> {
    let snap: Snapshot = serde_json::from_str(&std::fs::read_to_string(snapshot)?)?;
    let marker = marker_path()?;
    std::fs::write(&marker, std::process::id().to_string())?;
    let result = rebuild_with(&snap);
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(snapshot);
    let follow = result?;
    // The client reattaches once the marker is gone; agents report in
    // meanwhile.
    let handles: Vec<_> = follow
        .into_iter()
        .map(|(plan, pane)| {
            std::thread::spawn(move || match restart::follow_up(&plan, &pane) {
                Ok(true) => {}
                Ok(false) => eprintln!("tmux restart: the agent in {pane} didn't report back"),
                Err(e) => eprintln!("tmux restart: {pane}: {e:#}"),
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

/// Stop everything and build it again; returns each agent's plan with its new pane.
fn rebuild_with(snap: &Snapshot) -> Result<Vec<(Plan, String)>> {
    crate::cli::watch::stop();
    crate::tmux::kill_server()?;
    let start = Instant::now();
    while crate::tmux::server_running() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(100));
    }
    crate::tmux::start_session_with(&snap.env, snap.size)?;

    let mut follow = Vec::new();
    let mut active = None;
    for w in &snap.windows {
        match build_window(w) {
            Ok((id, panes)) => {
                if w.active {
                    active = Some(id);
                }
                for (snap_pane, new_pane) in w.panes.iter().zip(panes) {
                    if let Some(plan) = &snap_pane.plan {
                        follow.push((plan.clone(), new_pane));
                    }
                }
            }
            Err(e) => eprintln!("tmux restart: window {}: {e:#}", w.name),
        }
    }
    if let Some(id) = active {
        let _ = crate::tmux::select_window(&id);
    }
    crate::cli::watch::ensure_running(&crate::tmux::self_bin()?);
    Ok(follow)
}

/// One window with all its panes; returns its id and the new pane ids, in
/// the snapshot's order.
fn build_window(w: &WindowSnap) -> Result<(String, Vec<String>)> {
    let target = format!("{}:", crate::tmux::SESSION);
    let first = &w.panes[0];
    let mut args = vec!["new-window", "-d", "-t", &target, "-n", &w.name, "-c", &first.path, "-P", "-F", "#{window_id} #{pane_id}"];
    if let Some(c) = &first.command {
        args.push(c);
    }
    let out = crate::tmux::run(&args)?;
    let (id, pane) = out.trim().split_once(' ').context("new-window printed no ids")?;
    let (id, mut panes) = (id.to_string(), vec![pane.to_string()]);
    if let Some(dir) = &w.task_dir {
        crate::tmux::set_window_option(&id, crate::tmux::TASK_DIR_OPTION, dir)?;
        let _ = std::fs::write(PathBuf::from(dir).join(crate::tmux::WINDOW_ID_FILE), &id);
    }
    for p in &w.panes[1..] {
        // Split the pane made last: tmux puts a new pane right after the one
        // it splits, so splitting the first pane each time would reverse the
        // order, and the saved layout would put every pane in another's place.
        let last = panes.last().cloned().unwrap_or_default();
        let mut args = vec!["split-window", "-d", "-t", &last, "-c", &p.path, "-P", "-F", "#{pane_id}"];
        if let Some(c) = &p.command {
            args.push(c);
        }
        panes.push(crate::tmux::run(&args)?.trim().to_string());
        // Keep room for the next split; the saved layout is applied below.
        let _ = crate::tmux::run(&["select-layout", "-t", &id, "tiled"]);
    }
    if !w.dropped {
        let _ = crate::tmux::run(&["select-layout", "-t", &id, &w.layout]);
    }
    if let Some(i) = w.panes.iter().position(|p| p.active) {
        let _ = crate::tmux::run(&["select-pane", "-t", &panes[i]]);
        if w.zoomed {
            let _ = crate::tmux::run(&["resize-pane", "-Z", "-t", &panes[i]]);
        }
    }
    Ok((id, panes))
}

/// Whether a rebuild is running right now (its marker names a live process).
pub fn in_progress() -> bool {
    marker_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| t.trim().parse::<u32>().ok())
        .is_some_and(sessions::pid_alive)
}

/// Per server, like the watcher's pidfile.
fn marker_path() -> Result<PathBuf> {
    config_file("tmux-restart", "pid")
}

fn snapshot_path() -> Result<PathBuf> {
    config_file("tmux-restart", "json")
}

fn config_file(stem: &str, ext: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").context("$HOME not set")?;
    let sock = crate::tmux::socket();
    let name = if sock == crate::tmux::SOCKET { format!("{stem}.{ext}") } else { format!("{stem}-{sock}.{ext}") };
    let dir = PathBuf::from(home).join(".config/tenx");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(name))
}
