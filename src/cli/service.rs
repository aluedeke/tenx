//! `tenx service -- <command>`: run a long-lived process for the task — a
//! dev server, `tilt up`, a watch build — so that tenx knows what it is
//! (`tenx_core::taskservice`). An agent starts it with its background shell;
//! the turn that started it still ends, the sidebar lists it, and closing the
//! task's window or deleting the task stops it.
//!
//! The command runs as the leader of a process group of its own, so stopping
//! it stops everything it started. The wrapper stays in front of it: it
//! registers in `~/.config/tenx/services/<pid>.json`, passes a stop signal on
//! to the group (SIGKILL after [`STOP_GRACE`]), stops it too when the agent
//! that started it is gone (its parent changes), and exits with the command's
//! status, so the agent hears when its service died. A wrapper killed outright
//! leaves its record behind; [`services`] stops the orphaned group and prunes
//! the record.

use anyhow::{bail, Context, Result};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
use tenx_core::taskservice::{self, Service, ServiceRecord};

/// How long a stopped service gets to exit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// `~/.config/tenx/services/`: one `<pid>.json` per running service.
fn services_dir() -> Option<PathBuf> {
    Some(crate::workspace::home_dir().ok()?.join(".config").join("tenx").join("services"))
}

static SIGNALLED: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNALLED.store(sig, Ordering::SeqCst);
}

/// Run `command` (one shell command line, or a program and its arguments) as
/// the service `name` until it exits or is stopped.
pub fn run(name: Option<&str>, command: &[String]) -> Result<()> {
    if command.is_empty() {
        bail!("usage: tenx service [--name NAME] -- <command>");
    }
    let mut cmd = if command.len() == 1 {
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg(&command[0]);
        c
    } else {
        let mut c = std::process::Command::new(&command[0]);
        c.args(&command[1..]);
        c
    };
    cmd.process_group(0);
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores into an atomic.
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
    let parent = std::os::unix::process::parent_id();
    let mut child = cmd.spawn().with_context(|| format!("start {}", command.join(" ")))?;
    let cwd = std::env::current_dir()?;
    let rec = ServiceRecord {
        pid: std::process::id(),
        child: child.id(),
        name: name.map(str::to_string).unwrap_or_else(|| taskservice::default_name(command)),
        command: command.join(" "),
        cwd: cwd.canonicalize().unwrap_or(cwd),
        started: crate::live::now_secs(),
    };
    let _registered = register(&rec);

    let mut stopping: Option<Instant> = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let orphaned = std::os::unix::process::parent_id() != parent;
        if stopping.is_none() && (SIGNALLED.load(Ordering::SeqCst) != 0 || orphaned) {
            signal_group(rec.child, libc::SIGTERM);
            stopping = Some(Instant::now());
        }
        if stopping.is_some_and(|at| at.elapsed() > STOP_GRACE) {
            signal_group(rec.child, libc::SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Whatever the command left running in its group goes with it.
    signal_group(rec.child, libc::SIGTERM);
    drop(_registered);
    use std::os::unix::process::ExitStatusExt;
    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    std::process::exit(code);
}

fn signal_group(leader: u32, sig: libc::c_int) {
    // SAFETY: plain syscall; a group that is gone just fails.
    unsafe { libc::kill(-(leader as libc::pid_t), sig) };
}

/// A service's registration; removing the file when it drops covers every
/// ordinary exit.
struct Registered(PathBuf);

impl Drop for Registered {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn register(rec: &ServiceRecord) -> Option<Registered> {
    let dir = services_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.json", rec.pid));
    std::fs::write(&path, serde_json::to_vec(rec).ok()?).ok()?;
    Some(Registered(path))
}

/// Every service running now. A record whose wrapper is gone — or is no
/// longer a `tenx service`, its pid reused — is deleted on the way, and the
/// group it left behind is stopped.
pub fn services() -> Vec<ServiceRecord> {
    let Some(entries) = services_dir().and_then(|d| std::fs::read_dir(d).ok()) else { return Vec::new() };
    let mut out = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        let rec: Option<ServiceRecord> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        match rec {
            Some(rec) if is_wrapper(rec.pid) => out.push(rec),
            found => {
                if let Some(rec) = found
                    && leads_group(rec.child)
                {
                    signal_group(rec.child, libc::SIGTERM);
                }
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out.sort_by_key(|r| r.started);
    out
}

fn is_wrapper(pid: u32) -> bool {
    crate::workspace::sessions::pid_alive(pid)
        && crate::live::run_capture("ps", &["-o", "command=", "-p", &pid.to_string()]).contains(" service")
}

/// Whether `pid` still runs and leads its own process group — the command a
/// dead wrapper left, not a stranger that got its pid.
fn leads_group(pid: u32) -> bool {
    crate::workspace::sessions::pid_alive(pid)
        && crate::live::run_capture("ps", &["-o", "pgid=", "-p", &pid.to_string()]).trim() == pid.to_string()
}

/// Ask a service to stop: its wrapper passes it on and kills it after
/// [`STOP_GRACE`]; the group is signalled directly too, for a wrapper that no
/// longer listens. Returns at once.
fn stop(rec: &ServiceRecord) {
    // SAFETY: plain syscalls.
    unsafe { libc::kill(rec.pid as libc::pid_t, libc::SIGTERM) };
    signal_group(rec.child, libc::SIGTERM);
}

/// Stop every service started in `task_dir` — its window closed, or it is
/// being deleted. Returns how many were asked to stop.
pub fn stop_in(task_dir: &Path) -> usize {
    let all = services();
    if all.is_empty() {
        return 0;
    }
    let dir = task_dir.canonicalize().unwrap_or_else(|_| task_dir.to_path_buf());
    let mine = taskservice::in_task(&all, &dir);
    mine.iter().for_each(|r| stop(r));
    mine.len()
}

/// The services of `task_dir` as the sidebar shows them, with their ports
/// (`listeners` and `tree` from the watcher's port scan).
pub fn shown_in(records: &[ServiceRecord], task_dir: &Path, listeners: &[(u32, u16)], tree: &[(u32, u32)]) -> Vec<Service> {
    if records.is_empty() {
        return Vec::new();
    }
    let dir = task_dir.canonicalize().unwrap_or_else(|_| task_dir.to_path_buf());
    taskservice::in_task(records, &dir)
        .into_iter()
        .map(|r| Service {
            name: r.name.clone(),
            command: r.command.clone(),
            started: r.started,
            ports: tenx_core::live::ports_for(&[r.child], listeners, tree),
        })
        .collect()
}

/// The task directory the current directory is in, if any.
fn current_task() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    crate::workspace::registered_workspaces()
        .iter()
        .flat_map(|ws| ws.tasks().unwrap_or_default())
        .map(|t| t.path.canonicalize().unwrap_or(t.path))
        .find(|p| cwd.starts_with(p))
}

/// `tenx service stop [NAME] [--all]`: stop a service of the current task, or
/// all of them (`--all` outside a task: every service).
pub fn stop_cmd(which: Option<&str>, all: bool) -> Result<()> {
    let records = services();
    let task = current_task();
    let scope: Vec<&ServiceRecord> = match &task {
        Some(dir) => taskservice::in_task(&records, dir),
        None if all => records.iter().collect(),
        None => bail!("not inside a task: name the task's directory with `cd`, or pass --all to stop every service"),
    };
    let chosen: Vec<&ServiceRecord> = match which {
        _ if all => scope,
        Some(w) => match taskservice::find(&scope, w) {
            Some(r) => vec![r],
            None => bail!("no service '{w}' here (tenx service list)"),
        },
        None if scope.len() == 1 => scope,
        None if scope.is_empty() => bail!("no service runs here"),
        None => bail!("{} services run here: name one, or pass --all", scope.len()),
    };
    for r in chosen {
        stop(r);
        println!("stopped {} (pid {})", r.name, r.child);
    }
    Ok(())
}

/// `tenx service list`: the services running now, with the task each runs in.
pub fn list(json: bool) -> Result<()> {
    let records = services();
    let tasks: Vec<(String, PathBuf)> = crate::workspace::registered_workspaces()
        .iter()
        .flat_map(|ws| {
            let name = ws.config.name.clone();
            ws.tasks().unwrap_or_default().into_iter().map(move |t| {
                let path = t.path.canonicalize().unwrap_or(t.path);
                (format!("{name}/{}", t.name), path)
            })
        })
        .collect();
    let task_of = |r: &ServiceRecord| tasks.iter().find(|(_, p)| r.cwd.starts_with(p)).map(|(n, _)| n.clone());
    if json {
        let rows: Vec<serde_json::Value> = records
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name, "command": r.command, "pid": r.child, "task": task_of(r),
                    "cwd": r.cwd, "started": r.started,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if records.is_empty() {
        println!("no services running");
        return Ok(());
    }
    let now = crate::live::now_secs();
    for r in &records {
        let age = tenx_core::time::format_duration(Duration::from_secs(now.saturating_sub(r.started)));
        let task = task_of(r).unwrap_or_else(|| r.cwd.display().to_string());
        println!("{}  {task}  up {age}  pid {}  {}", r.name, r.child, r.command);
    }
    Ok(())
}
