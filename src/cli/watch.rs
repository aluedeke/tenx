//! The attention watcher: a detached background process that tells you when an
//! agent starts waiting on you.
//!
//! Everything else in tenx only knows a task's state while you're looking at a
//! list. This is the piece that watches when you aren't — the gap that let eight
//! tasks sit 18-23 hours waiting on an answer nobody knew they wanted.
//!
//! **Why a process and not a zellij plugin.** A plugin was the obvious home —
//! it dies with the session, no supervision — but zellij 0.44 has no paneless
//! plugin: `zellij pipe --plugin <url>` launches one *with a pane* (measured:
//! session pane count 11 → 13, and `plugin location=...` in the layout dump),
//! and `zellij plugin` says so in its own help. A watcher that needs a visible
//! pane to sit in is exactly the leftover-pane problem the tabless design
//! avoids. A process also serves every consumer, where a plugin can only pipe
//! to other plugins.
//!
//! **Lifecycle.** `tenx` is the only way into the session, and `main::open()` is
//! the single funnel for all three entry paths, so [`ensure_running`] hangs off
//! it: start the session, get a watcher. It exits on its own once the zellij
//! session is gone, so it can't outlive what it's watching.
//!
//! **Why polling rather than a filesystem watch.** `~/.claude/sessions` is
//! push-updated by Claude Code, so watching it looks right — but a full resolve
//! costs ~5 ms (measured on 6 workspaces / 43 tasks), which at this interval is
//! a rounding error, and it avoids both a new dependency and zellij's own
//! "somewhat unstable" `watch_filesystem`. If this ever gets expensive, the
//! answer is to watch the directory, not to poll less often.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::workspace::{self, TaskStatus};

/// How often to resolve every task. Cheap enough (~5 ms) that the interval is
/// set by how fast you want to hear, not by cost.
const POLL: Duration = Duration::from_secs(2);

/// A task must still be waiting on the *next* poll before it earns a
/// notification. Approving a permission prompt within a couple of seconds is
/// normal working rhythm, not something to interrupt yourself over.
const DEBOUNCE_POLLS: u32 = 1;

/// Check whether the session is still alive every this many polls.
const SESSION_CHECK_POLLS: u32 = 15;

/// Consecutive failed session checks before exiting. The watcher is started
/// *before* `create_and_attach_session` returns, so the session legitimately
/// doesn't exist yet for the first moments of its life.
const SESSION_MISSES_BEFORE_EXIT: u32 = 3;

/// Run the watch loop until the tenx zellij session goes away. Blocks; this is
/// the body of the detached process, not something to call inline.
pub fn run() -> Result<()> {
    // The guard belongs here, not only in `ensure_running`: `tenx watch` is a
    // real command someone can run twice, and two watchers means two
    // notifications for one prompt.
    if running() {
        eprintln!("tenx watch: already running");
        return Ok(());
    }
    write_pidfile()?;

    // Prime from the current state: anything already waiting when the watcher
    // starts has been waiting since before we existed, and firing a burst of
    // notifications for a backlog is not "attention", it's noise.
    let mut notified: HashSet<String> = blocked_now().into_iter().map(|(k, _)| k).collect();
    let mut pending: HashMap<String, u32> = HashMap::new();
    let mut tick: u32 = 0;
    let mut session_misses: u32 = 0;

    loop {
        std::thread::sleep(POLL);
        tick = tick.wrapping_add(1);

        let blocked = blocked_now();
        let keys: HashSet<String> = blocked.iter().map(|(k, _)| k.clone()).collect();

        for (key, note) in &blocked {
            if notified.contains(key) {
                continue;
            }
            let seen = pending.entry(key.clone()).or_insert(0);
            *seen += 1;
            if *seen > DEBOUNCE_POLLS {
                notify(note);
                notified.insert(key.clone());
                pending.remove(key);
            }
        }

        // Left the waiting state — forget it, so the next prompt notifies again.
        pending.retain(|k, _| keys.contains(k));
        notified.retain(|k| keys.contains(k));

        if tick % SESSION_CHECK_POLLS == 0 {
            let alive = crate::zellij::session_exists(crate::zellij::SESSION).unwrap_or(true);
            session_misses = if alive { 0 } else { session_misses + 1 };
            if session_misses >= SESSION_MISSES_BEFORE_EXIT {
                let _ = std::fs::remove_file(pid_path()?);
                return Ok(());
            }
        }
    }
}

/// What to say about a task that started waiting.
struct Note {
    task: String,
    workspace: String,
    reason: Option<String>,
}

/// Every task currently blocked, keyed by workspace dir + slug.
fn blocked_now() -> Vec<(String, Note)> {
    let sessions = workspace::claude::sessions();
    let mut out = Vec::new();
    for ws in workspace::registered_workspaces() {
        for task in ws.tasks().unwrap_or_default() {
            let state = workspace::resolve_task_state(&task.path, &sessions);
            if state.status != TaskStatus::Blocked {
                continue;
            }
            out.push((
                format!("{}/{}", ws.dir.display(), task.name),
                Note {
                    task: task.display_name.clone(),
                    workspace: ws.config.name.clone(),
                    reason: state.waiting_for.clone(),
                },
            ));
        }
    }
    out
}

/// Deliver one notification. `terminal-notifier` when it's installed (it can
/// carry a subtitle and group by task), `osascript` otherwise — always present
/// on macOS, so there's no path where the watcher runs but can't speak.
fn notify(note: &Note) {
    let title = "tenx — needs input";
    let subtitle = format!("{} · {}", note.task, note.workspace);
    let body = note.reason.clone().unwrap_or_else(|| "waiting for you".into());

    if which("terminal-notifier").is_some() {
        let _ = Command::new("terminal-notifier")
            .args(["-title", title, "-subtitle", &subtitle, "-message", &body])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return;
    }
    let script = format!(
        "display notification {} with title {} subtitle {}",
        applescript_str(&body),
        applescript_str(title),
        applescript_str(&subtitle),
    );
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Quote a string for AppleScript. Task titles come from `TASK.md`, i.e. from
/// whatever anyone typed — an unescaped quote would turn a notification into a
/// syntax error at best.
fn applescript_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn which(bin: &str) -> Option<PathBuf> {
    let out = Command::new("command").args(["-v", bin]).output().ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

// ── Single-instance bookkeeping ───────────────────────────────────────────────

fn pid_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("$HOME not set")?;
    Ok(PathBuf::from(home).join(".config/tenx/watch.pid"))
}

fn write_pidfile() -> Result<()> {
    let path = pid_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, std::process::id().to_string())?;
    Ok(())
}

/// Whether a watcher is already running, by the same pid-liveness test the
/// session registry gets read with — a stale pidfile after a crash is expected,
/// not exceptional.
fn running() -> bool {
    let Ok(path) = pid_path() else { return false };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    pid != std::process::id() && workspace::claude::pid_alive(pid)
}

/// Start a watcher unless one is already running. Called from `main::open()`,
/// so every route into the session gets one. Best-effort throughout: a watcher
/// that fails to start must never stop you opening your tasks.
pub fn ensure_running(bin: &Path) {
    if running() {
        return;
    }
    let mut cmd = Command::new(bin);
    cmd.arg("watch")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Own session + process group, so closing the terminal that happened to
    // start it doesn't take the watcher down with a SIGHUP.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}
