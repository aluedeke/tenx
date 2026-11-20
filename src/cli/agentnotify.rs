//! `tenx notify`: a notification an agent sends because you asked it to —
//! "ping me when CI is green", "tell me when you're done". It goes to the
//! Mac (`cli::notify`), and to every browser and iOS device that turned
//! notifications on (`crate::push`) — unless you are at the Mac looking at
//! the task already (`tenx_core::notify::at_mac`): then the Mac alone.
//!
//! `--when-done` arms it instead (`.tenx-notify` in the task directory,
//! `tenx_core::notify::Armed`); the watcher sends it when the agent's turn
//! is over, with the agent's last reply as its text (`fire_armed`).

use crate::workspace::{self, Task, Workspace};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tenx_core::notify::{self as rules, Armed};

/// The task the current directory is in.
fn current() -> Option<(Workspace, Task)> {
    let cwd = std::env::current_dir().ok()?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    for ws in workspace::registered_workspaces() {
        for task in ws.tasks().unwrap_or_default() {
            let dir = task.path.canonicalize().unwrap_or_else(|_| task.path.clone());
            if cwd.starts_with(&dir) {
                return Some((ws, task));
            }
        }
    }
    None
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// `tenx notify [MESSAGE] [--title T] [--when-done] [--cancel]`.
pub fn run(message: Option<&str>, title: Option<&str>, when_done: bool, cancel: bool) -> Result<()> {
    let here = current();
    if cancel || when_done {
        let (_, task) = here.as_ref().context("not inside a task directory — `--when-done` belongs to a task")?;
        if cancel {
            disarm(&task.path);
            println!("No notification when this task is done.");
        } else {
            arm(&task.path, title, message)?;
            println!("Armed: tenx notifies you when this turn is over{}.", if message.is_some() { "" } else { ", with your last reply" });
        }
        return Ok(());
    }
    let Some(message) = message.map(str::trim).filter(|m| !m.is_empty()) else {
        bail!("usage: tenx notify \"<message>\" (or --when-done)");
    };
    let report = match &here {
        Some((ws, task)) => deliver(Some((ws, task)), title, message),
        None => deliver(None, title, message),
    };
    println!("{report}");
    Ok(())
}

/// Arm `--when-done` for the task at `dir`.
pub(crate) fn arm(dir: &Path, title: Option<&str>, message: Option<&str>) -> Result<()> {
    let armed = Armed {
        armed_at_ms: now_ms(),
        title: title.map(str::to_string),
        message: message.map(str::trim).filter(|m| !m.is_empty()).map(str::to_string),
    };
    let path = dir.join(rules::ARMED_FILE);
    std::fs::write(&path, serde_json::to_string(&armed)?).with_context(|| format!("write {}", path.display()))
}

pub(crate) fn disarm(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(rules::ARMED_FILE));
}

/// One watcher pass: send every armed notification whose task finished a
/// turn since it was armed. The file goes first, so it fires once; the
/// reply is read from the transcript on a thread of its own.
pub(crate) fn fire_armed(turns: &[crate::cli::watch::TurnState]) {
    for turn in turns {
        let path = turn.dir.join(rules::ARMED_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(armed) = serde_json::from_str::<Armed>(&text) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let changed = turn.changed.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
        if !rules::fires(&armed, turn.status == tenx_core::status::TaskStatus::Done, changed) {
            continue;
        }
        if std::fs::remove_file(&path).is_err() {
            continue;
        }
        let (workspace, slug) = (turn.workspace.clone(), turn.slug.clone());
        std::thread::spawn(move || {
            let Some((ws, task)) = find(&workspace, &slug) else { return };
            let body = match armed.message {
                Some(m) => m,
                None => match crate::cli::drive::last_turn(&ws, &task) {
                    Ok(t) => t.replies.last().map(|r| rules::summary(r, rules::BODY_MAX)).unwrap_or_else(|| "Done.".to_string()),
                    Err(_) => "Done.".to_string(),
                },
            };
            deliver(Some((&ws, &task)), armed.title.as_deref(), &body);
        });
    }
}

fn find(workspace: &str, slug: &str) -> Option<(Workspace, Task)> {
    let ws = workspace::registered_workspaces().into_iter().find(|w| w.config.name == workspace)?;
    let task = ws.find_task(slug).ok()?;
    Some((ws, task))
}

/// Send it: the Mac always, the devices unless you are at the Mac looking
/// at the task. What happened, as a line to print.
fn deliver(target: Option<(&Workspace, &Task)>, title: Option<&str>, body: &str) -> String {
    let (id, task_title, workspace) = match target {
        Some((ws, task)) => (format!("{}/{}", ws.config.name, task.name), task.display_name.clone(), ws.config.name.clone()),
        None => ("tenx".to_string(), "tenx".to_string(), String::new()),
    };
    let subtitle = if workspace.is_empty() { task_title.clone() } else { format!("{task_title} · {workspace}") };
    crate::cli::notify::platform().notify(title.unwrap_or("tenx — message"), &subtitle, body);

    if let Some((_, task)) = target {
        if looking_at(&task.path) {
            return "Shown on the Mac only: you are there, looking at this task.".to_string();
        }
    }
    let msg = crate::push::Message {
        title: title.map(str::to_string).unwrap_or(task_title),
        body: body.to_string(),
        tag: id.clone(),
        url: if target.is_some() { format!("/?task={id}") } else { "/".to_string() },
    };
    match crate::push::Push::load() {
        Ok(push) => {
            let (sent, total) = push.send_all(&msg);
            match total {
                0 => "Shown on the Mac. No phone or browser has notifications on (tenx web → ⋯ → notifications).".to_string(),
                _ => format!("Shown on the Mac and sent to {sent} of {total} device(s)."),
            }
        }
        Err(e) => format!("Shown on the Mac; devices: {e:#}"),
    }
}

/// You are at the Mac and the terminal attached to the tenx session shows
/// the task at `dir` (`tenx_core::notify::at_mac`).
fn looking_at(dir: &Path) -> bool {
    let activity = crate::tmux::client_activity_in(crate::tmux::SESSION);
    if activity.is_empty() {
        return false;
    }
    let shown = crate::tmux::window_option_in(crate::tmux::SESSION, crate::tmux::TASK_DIR_OPTION)
        .filter(|d| !d.is_empty())
        .map(|d| canonical(Path::new(&d)));
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    let idle = mac_idle_secs().or_else(|| activity.iter().max().map(|a| now.saturating_sub(*a)));
    rules::at_mac(idle, shown.as_deref(), &canonical(dir))
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Seconds since the Mac's last keyboard or mouse input (`HIDIdleTime`),
/// whichever app it went to.
#[cfg(target_os = "macos")]
fn mac_idle_secs() -> Option<u64> {
    let out = std::process::Command::new("ioreg").args(["-c", "IOHIDSystem", "-d", "4", "-r", "-k", "HIDIdleTime"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("\"HIDIdleTime\""))?;
    let ns: u64 = line.rsplit('=').next()?.trim().parse().ok()?;
    Some(ns / 1_000_000_000)
}

#[cfg(not(target_os = "macos"))]
fn mac_idle_secs() -> Option<u64> {
    None
}
