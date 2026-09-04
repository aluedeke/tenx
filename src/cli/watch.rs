//! The attention watcher: a detached background process that resolves every
//! task on a timer, and is the session's single source of "what is happening
//! right now" for anything that needs telling rather than asking.
//!
//! Everything else in tenx only knows a task's state while you're looking at a
//! list. This is the piece that watches when you aren't — the gap that let eight
//! tasks sit 18-23 hours waiting on an answer nobody knew they wanted.
//!
//! **Several consumers, one resolve pass** ([`resolve_all`]):
//!
//! - a desktop notification on the edge into a needs-you state (`Blocked`, or
//!   a bell → `Signaled`), debounced, so you hear about a prompt while you're
//!   in another app;
//! - the tmux status line, via per-window `@tenx_status` and the global
//!   `@tenx_right` user options ([`push_status`]), which is how every window
//!   learns that a task you *aren't* looking at changed state;
//! - a transcript pane for each background agent ([`pane_new_agents`]);
//! - the per-task `.tenx-live.json` cache of ports and PRs ([`refresh_live`]).
//!
//! Notification and status line deliberately differ on `Done`: a finished
//! turn fires after every single turn, which is noise as a desktop popup and
//! exactly the right thing to show quietly in a status line.
//!
//! **Lifecycle.** `tenx` is the only way into the session, and `main::open()`
//! is the single funnel for every entry path, so [`ensure_running`] hangs off
//! it: start the session, get a watcher. It exits on its own once the tmux
//! server is gone, so it can't outlive what it's watching.
//!
//! **Why polling rather than a filesystem watch.** `~/.claude/sessions` is
//! push-updated by Claude Code, so watching it looks right — but a full
//! resolve costs ~5 ms (measured on 6 workspaces / 43 tasks), which at this
//! interval is a rounding error, and tmux's bell flags have to be polled
//! anyway. If this ever gets expensive, the answer is to watch the directory,
//! not to poll less often.

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
    // notifications for a backlog is not "attention", it's noise. Same
    // reasoning for secrets-pending, tracked in parallel — an independent
    // condition from `blocked` (a task can be idle and still have a pending
    // secrets request left over from an agent that already finished).
    let primed = resolve_all();
    let mut notified: HashSet<String> = primed.blocked.into_iter().map(|(k, _)| k).collect();
    let mut pending: HashMap<String, u32> = HashMap::new();
    let mut notified_secrets: HashSet<String> =
        primed.secrets_pending.into_iter().map(|(k, _)| k).collect();
    let mut pending_secrets: HashMap<String, u32> = HashMap::new();
    let mut notified_secrets_set: HashSet<String> =
        primed.secrets_pending_set.into_iter().map(|(k, _)| k).collect();
    let mut pending_secrets_set: HashMap<String, u32> = HashMap::new();
    let mut tick: u32 = 0;
    let mut session_misses: u32 = 0;
    // Agents already given a pane. Primed from the current state: agents
    // running before the watcher started were either paned by a previous
    // watcher or predate this feature; either way a pane now would be a
    // surprise, not news.
    let mut paned: HashSet<(PathBuf, u32)> = primed.agents.iter().map(|a| (a.cwd.clone(), a.pid)).collect();
    let mut pr_lookups = PrLookups::new();
    // Deliberately not seeded from `primed`: the session is still being created
    // at this point (see SESSION_MISSES_BEFORE_EXIT), so no status bar exists to
    // receive a push yet. Starting empty means the first tick always publishes,
    // which is what gives a freshly opened session its initial state.
    let mut published = String::new();

    loop {
        std::thread::sleep(POLL);
        tick = tick.wrapping_add(1);

        let snapshot = resolve_all();
        let blocked = snapshot.blocked;
        let keys: HashSet<String> = blocked.iter().map(|(k, _)| k.clone()).collect();

        for (key, note) in &blocked {
            if notified.contains(key) {
                continue;
            }
            let seen = pending.entry(key.clone()).or_insert(0);
            *seen += 1;
            if *seen > DEBOUNCE_POLLS {
                notify(note, "tenx — needs input");
                notified.insert(key.clone());
                pending.remove(key);
            }
        }

        // Left the waiting state — forget it, so the next prompt notifies again.
        pending.retain(|k, _| keys.contains(k));
        notified.retain(|k| keys.contains(k));

        // Same edge-notify shape, independent condition: a secrets request
        // outlives the session that made it, so this can fire (and clear)
        // completely out of step with `blocked` for the same task.
        let secrets_pending = snapshot.secrets_pending;
        let secrets_keys: HashSet<String> = secrets_pending.iter().map(|(k, _)| k.clone()).collect();

        for (key, note) in &secrets_pending {
            if notified_secrets.contains(key) {
                continue;
            }
            let seen = pending_secrets.entry(key.clone()).or_insert(0);
            *seen += 1;
            if *seen > DEBOUNCE_POLLS {
                notify(note, "tenx — secrets pending");
                notified_secrets.insert(key.clone());
                pending_secrets.remove(key);
            }
        }
        // Cleared (unlocked, or the marker removed some other way) — forget
        // it, so a future request for this task notifies again.
        pending_secrets.retain(|k, _| secrets_keys.contains(k));
        notified_secrets.retain(|k| secrets_keys.contains(k));

        // Third independent edge, same shape again: `set`'s value-request
        // queue (see `workspace::SECRETS_PENDING_SET_FILE`) — a human needs
        // to type in a value for something that doesn't exist yet, distinct
        // from `secrets_pending` above (which means "release something
        // already sealed").
        let secrets_pending_set = snapshot.secrets_pending_set;
        let secrets_set_keys: HashSet<String> =
            secrets_pending_set.iter().map(|(k, _)| k.clone()).collect();

        for (key, note) in &secrets_pending_set {
            if notified_secrets_set.contains(key) {
                continue;
            }
            let seen = pending_secrets_set.entry(key.clone()).or_insert(0);
            *seen += 1;
            if *seen > DEBOUNCE_POLLS {
                notify(note, "tenx — secret value needed");
                notified_secrets_set.insert(key.clone());
                pending_secrets_set.remove(key);
            }
        }
        pending_secrets_set.retain(|k, _| secrets_set_keys.contains(k));
        notified_secrets_set.retain(|k| secrets_set_keys.contains(k));

        pane_new_agents(&snapshot.agents, &snapshot.windows, &mut paned);
        let live_changed = refresh_live(&snapshot.live_targets, &mut pr_lookups);

        // Publish last. Only on a real change, so a quiet session sends
        // nothing at all: `digest` covers state, `live_changed` the cached
        // ports/PRs (which `push_status` reads back after the refresh).
        let current = digest(&snapshot.tasks);
        if current != published || live_changed {
            published = current;
            push_status(&snapshot.tasks, &snapshot.windows);
        }

        if tick.is_multiple_of(SESSION_CHECK_POLLS) {
            let alive = crate::tmux::server_running();
            session_misses = if alive { 0 } else { session_misses + 1 };
            if session_misses >= SESSION_MISSES_BEFORE_EXIT {
                let _ = std::fs::remove_file(pid_path()?);
                return Ok(());
            }
        }
    }
}

/// What to say about a task that started waiting — for a `blocked` edge,
/// `reason` is Claude Code's own waiting-for label; for a secrets-pending
/// edge, it's the joined names of what's pending (`cli::secrets::enqueue_pending`,
/// `decrypt`'s non-interactive fallback).
struct Note {
    task: String,
    workspace: String,
    reason: Option<String>,
}

/// One resolve pass over every task, serving both consumers.
///
/// The desktop notification needs the blocked *edge*; the status bar needs every
/// task's current state. Deriving both from a single pass is what keeps them
/// from ever disagreeing about what a task is doing — and the loop already
/// walked every task to find the blocked ones, it just threw the rest away.
struct Snapshot {
    /// Tasks currently blocked, keyed by workspace dir + slug.
    blocked: Vec<(String, Note)>,
    /// Tasks with a pending secrets *release* request, keyed the same way.
    /// Independent of `blocked` — this is `cli::secrets::enqueue_pending`'s
    /// marker (`decrypt`'s non-interactive fallback), which outlives the
    /// session that wrote it.
    secrets_pending: Vec<(String, Note)>,
    /// Tasks with a pending secrets *value* request — `set`'s non-interactive
    /// fallback (`cli::secrets::enqueue_pending_set`), meaning a human needs
    /// to type in a value for something that doesn't exist yet. Tracked
    /// separately from `secrets_pending` above: different marker file,
    /// different fulfillment action.
    secrets_pending_set: Vec<(String, Note)>,
    /// Background agents (`--bg`, any non-interactive kind) running under a
    /// task. The watcher gives each a pane in its task's window the first
    /// time it sees it.
    agents: Vec<Agent>,
    /// Every task, for the live-facts refresh (`refresh_live`): where it is,
    /// which repos it has, and whether it's active (open window or a live
    /// session), which sets how eagerly its PR is re-checked.
    live_targets: Vec<LiveTarget>,
    /// The session's windows as listed for this pass — one `list-windows`
    /// serves signals, agent panes and the status push.
    windows: Vec<crate::tmux::Window>,
    /// Tasks with a live Claude Code session, in the shared wire shape
    /// (`workspace::task_json`).
    ///
    /// `Idle` tasks are omitted rather than sent as idle: `Idle` *is* "no live
    /// session", so absence carries the same information, and a reader that
    /// knows its own slug reads "not in this list" as idle. It matters because
    /// pipe latency scales with payload size — measured on this workspace set,
    /// 41 tasks is 10.3 KB and ~0.5 s to deliver, the 11 live ones are 2.8 KB
    /// and ~0.17 s. Sending everything would make the cost grow with how many
    /// tasks you have ever created; sending the live ones makes it grow with how
    /// many agents are actually running.
    tasks: Vec<serde_json::Value>,
}

struct LiveTarget {
    slug: String,
    path: PathBuf,
    repos: Vec<String>,
    active: bool,
}

struct Agent {
    slug: String,
    cwd: PathBuf,
    pid: u32,
    session_id: Option<String>,
}

fn resolve_all() -> Snapshot {
    let sessions = workspace::claude::sessions();
    let windows = crate::tmux::list_windows().unwrap_or_default();
    let signals = crate::tmux::signals_from(&windows);
    let mut blocked = Vec::new();
    let mut secrets_pending = Vec::new();
    let mut secrets_pending_set = Vec::new();
    let mut agents = Vec::new();
    let mut live_targets = Vec::new();
    let mut tasks: Vec<(Option<std::time::SystemTime>, serde_json::Value)> = Vec::new();
    for ws in workspace::registered_workspaces() {
        for task in ws.tasks().unwrap_or_default() {
            let state = workspace::resolve_task_state(&task.path, &sessions, &signals);
            let key = format!("{}/{}", ws.dir.display(), task.name);
            live_targets.push(LiveTarget {
                slug: task.name.clone(),
                path: task.path.clone(),
                repos: task.repos.clone(),
                active: state.status != TaskStatus::Idle || signals.contains_key(&task.name),
            });
            for s in tenx_core::status::sessions_for(&sessions, &task.path) {
                if s.kind != "interactive" && s.cwd != task.path {
                    agents.push(Agent {
                        slug: task.name.clone(),
                        cwd: s.cwd.clone(),
                        pid: s.pid,
                        session_id: s.session_id.clone(),
                    });
                }
            }
            // `Blocked` and `Signaled` are both "waiting on you" edges — a
            // prompt, or a bell from anything in the window. One list, one
            // debounce, one notification shape.
            if state.status.needs_you() {
                blocked.push((
                    key.clone(),
                    Note {
                        task: task.display_name.clone(),
                        workspace: ws.config.name.clone(),
                        reason: state.waiting_for.clone(),
                    },
                ));
            }
            let pending_names = workspace::secrets_pending(&task.path);
            if !pending_names.is_empty() {
                secrets_pending.push((
                    key.clone(),
                    Note {
                        task: task.display_name.clone(),
                        workspace: ws.config.name.clone(),
                        reason: Some(pending_names.join(", ")),
                    },
                ));
            }
            let pending_set_names = workspace::secrets_pending_set(&task.path);
            if !pending_set_names.is_empty() {
                secrets_pending_set.push((
                    key,
                    Note {
                        task: task.display_name.clone(),
                        workspace: ws.config.name.clone(),
                        reason: Some(format!("{} (needs value)", pending_set_names.join(", "))),
                    },
                ));
            }
            // Idle tasks are normally omitted (see `Snapshot::tasks` docs), but
            // a task with a pending secrets request still needs to reach the
            // status bar even with no live session — that's exactly the case
            // of an agent that requested a secret and then finished or was
            // closed before you unlocked it.
            if state.status != TaskStatus::Idle || !pending_names.is_empty() || !pending_set_names.is_empty() {
                tasks.push((state.changed, workspace::task_json(&ws, &task, &state)));
            }
        }
    }
    // Newest activity first, matching what `tenx overlay --json` promises for
    // the same shape. Same bytes in a different order is still a different wire
    // format to anyone who reads position; cheap to guarantee here, awkward to
    // rediscover in a consumer that assumed it.
    tasks.sort_by_key(|t| std::cmp::Reverse(t.0));
    Snapshot {
        blocked,
        secrets_pending,
        secrets_pending_set,
        agents,
        live_targets,
        windows,
        tasks: tasks.into_iter().map(|(_, v)| v).collect(),
    }
}

/// PR lookups in flight and their results. The helper thread only *fetches*;
/// every write to `.tenx-live.json` happens on the watch thread, so the ports
/// update and the PR update can't race each other's read-modify-write.
struct PrLookups {
    busy: bool,
    tx: std::sync::mpsc::Sender<(PathBuf, Vec<crate::live::PrInfo>)>,
    rx: std::sync::mpsc::Receiver<(PathBuf, Vec<crate::live::PrInfo>)>,
}

impl PrLookups {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        PrLookups { busy: false, tx, rx }
    }
}

/// Refresh every task's `.tenx-live.json`: ports for the open windows every
/// tick (cheap, local), PRs on a staggered schedule (network, off-thread so a
/// slow `gh` can't hold up the notification path; one batch in flight at a
/// time). Returns whether any cache changed, so the caller can republish.
fn refresh_live(targets: &[LiveTarget], prs: &mut PrLookups) -> bool {
    let mut changed = false;

    // Land finished PR lookups first — this is the only writer of `prs`. An
    // empty path is the batch's sentinel: the helper thread is done.
    while let Ok((path, found)) = prs.rx.try_recv() {
        if path.as_os_str().is_empty() {
            prs.busy = false;
            continue;
        }
        let mut live = crate::live::read(&path);
        if live.prs != found {
            changed = true;
        }
        live.prs = found;
        live.pr_checked = crate::live::now_secs();
        let _ = crate::live::write(&path, &live);
    }

    // A task with no window (closed, swept, server gone) has no ports — clear
    // them, don't keep showing the last observed set.
    let ports = crate::live::ports_by_window();
    for t in targets {
        let new_ports = ports.get(&t.slug).cloned().unwrap_or_default();
        let mut live = crate::live::read(&t.path);
        if live.ports != new_ports {
            live.ports = new_ports;
            changed |= crate::live::write(&t.path, &live).is_ok();
        }
    }

    if prs.busy || !crate::live::gh_available() {
        return changed;
    }
    if !targets.iter().any(|t| !t.repos.is_empty()) {
        return changed;
    }
    // Budget counts `gh` calls (one per repo), not tasks: a five-repo task
    // is five network round trips.
    let now = crate::live::now_secs();
    let mut budget = crate::live::PR_LOOKUPS_PER_TICK;
    let mut stale: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for t in targets.iter().filter(|t| !t.repos.is_empty()) {
        if budget == 0 {
            break;
        }
        let ttl = if t.active { crate::live::PR_TTL_ACTIVE } else { crate::live::PR_TTL_PARKED };
        if now.saturating_sub(crate::live::read(&t.path).pr_checked) < ttl.as_secs() {
            continue;
        }
        let repos: Vec<String> = t.repos.iter().take(budget).cloned().collect();
        budget -= repos.len();
        stale.push((t.path.clone(), repos));
    }
    if stale.is_empty() {
        return changed;
    }
    prs.busy = true;
    let tx = prs.tx.clone();
    std::thread::spawn(move || {
        for (path, repos) in stale {
            let found: Vec<_> = repos.iter().filter_map(|r| crate::live::fetch_pr(r, &path.join(r))).collect();
            let _ = tx.send((path, found));
        }
        // Sentinel: the batch is done, the next tick may start another.
        let _ = tx.send((PathBuf::new(), Vec::new()));
    });
    changed
}

/// Give every background agent a pane in its task's window, once. An agent
/// is another process with no terminal of its own, so the pane follows its
/// transcript (`cli::agentlog`) and closes when the agent exits. `paned` is
/// keyed by (cwd, pid) so a restarted agent in the same directory gets a
/// fresh pane; entries for dead agents are pruned so the set can't grow
/// forever. No window open for the task → nothing to do, and the agent is
/// tried again next tick in case the window opens later.
fn pane_new_agents(agents: &[Agent], windows: &[crate::tmux::Window], paned: &mut HashSet<(PathBuf, u32)>) {
    paned.retain(|(cwd, pid)| agents.iter().any(|a| a.cwd == *cwd && a.pid == *pid));
    if agents.is_empty() {
        return;
    }
    let Ok(bin) = std::env::current_exe() else { return };
    for a in agents {
        let key = (a.cwd.clone(), a.pid);
        if paned.contains(&key) {
            continue;
        }
        let Some(w) = windows.iter().find(|w| w.name == a.slug) else { continue };
        let opened = crate::tmux::open_agent_pane(
            &w.id,
            &bin.to_string_lossy(),
            &a.cwd.to_string_lossy(),
            a.pid,
            a.session_id.as_deref(),
        );
        if opened.is_ok() {
            paned.insert(key);
        }
    }
}

/// A `Done` task counts as "waiting on you" in the corner only once it has
/// sat unanswered this long. Every turn ends `Done`; without a gate the
/// count flips on and off with each prompt you type — the flicker the old
/// status bar measured and designed out.
const RIGHT_WAIT_GATE_SECS: u64 = 60;

/// Push the snapshot into tmux: one `@tenx_status` user option per open task
/// window (what `status-left` shows in that window) and a global `@tenx_right`
/// (what *else* is waiting on you). Called only on a real change, so a quiet
/// session costs nothing. Rows are `workspace::task_json`, which already
/// carries the live chips — this doesn't re-read anything.
fn push_status(tasks: &[serde_json::Value], windows: &[crate::tmux::Window]) {
    if windows.is_empty() {
        return;
    }
    let current = windows.iter().find(|w| w.active).map(|w| w.name.as_str());
    for w in windows.iter().filter(|w| w.name != crate::tmux::HOME_WINDOW) {
        let task = tasks.iter().find(|t| t["slug"].as_str() == Some(w.name.as_str()));
        let text = match task {
            Some(t) => status_line(t, &w.name),
            None => String::new(), // idle: the format falls back to the window name
        };
        let _ = crate::tmux::set_window_option(&w.id, "@tenx_status", &text);
    }

    // The corner is about the tasks you are *not* looking at.
    let (mut locked, mut need, mut waiting) = (0usize, 0usize, 0usize);
    for t in tasks.iter().filter(|t| t["slug"].as_str() != current) {
        if has_secrets_pending(t) {
            locked += 1;
            continue;
        }
        match TaskStatus::from_token(t["status"].as_str().unwrap_or("")) {
            s if s.needs_you() => need += 1,
            TaskStatus::Done if t["age_secs"].as_u64().unwrap_or(0) >= RIGHT_WAIT_GATE_SECS => waiting += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if locked > 0 {
        parts.push(format!("#[fg={}]🔒 {locked} secret{}", crate::palette::ACCENT.hex(), if locked == 1 { "" } else { "s" }));
    }
    if need > 0 {
        parts.push(format!("#[fg={}]● {need} need input", crate::palette::WARN.hex()));
    }
    if waiting > 0 {
        parts.push(format!("#[fg={}]✔ {waiting} waiting", crate::palette::SUCCESS.hex()));
    }
    let sep = format!("#[fg={}] · ", crate::palette::MUTED.hex());
    let mut right = parts.join(&sep);
    if !right.is_empty() {
        right.push_str("#[default]");
    }
    let _ = crate::tmux::set_global_option("@tenx_right", &right);
}

fn has_secrets_pending(t: &serde_json::Value) -> bool {
    let non_empty = |k: &str| t[k].as_array().is_some_and(|a| !a.is_empty());
    non_empty("secrets_pending") || non_empty("secrets_pending_set")
}

/// One window's `status-left` text from its snapshot row: glyph, title,
/// Claude's waiting reason, then the PR and port chips the row carries.
fn status_line(t: &serde_json::Value, slug: &str) -> String {
    let status = TaskStatus::from_token(t["status"].as_str().unwrap_or(""));
    let title = t["title"].as_str().unwrap_or(slug);
    // tmux style directives (`#[fg=…]`): the option is expanded with `E:` in
    // the generated config, so the glyph gets its status colour and the
    // title its own, same table as the overlay (`palette::status_color`).
    let text_fg = crate::palette::TEXT.hex();
    let mut text = if has_secrets_pending(t) {
        format!("#[fg={}]🔒#[fg={text_fg},bold] {title}", crate::palette::ACCENT.hex())
    } else {
        let glyph = format!("#[fg={}]{}", crate::palette::status_color(status).hex(), status.glyph());
        match (status, t["waiting_for"].as_str()) {
            (TaskStatus::Blocked, Some(reason)) => {
                format!("{glyph}#[fg={text_fg},bold] {title}#[fg={},nobold] · {reason}", crate::palette::WARN.hex())
            }
            _ => format!("{glyph}#[fg={text_fg},bold] {title}"),
        }
    };
    text.push_str("#[nobold]");
    if let Some(prs) = t["prs"].as_array() {
        for p in prs {
            let color = match p["checks"].as_str() {
                Some("failure") => crate::palette::DANGER.hex(),
                Some("success") => crate::palette::SUCCESS.hex(),
                _ => crate::palette::INFO.hex(),
            };
            if let Some(chip) = p["chip"].as_str() {
                text.push_str(&format!("  #[fg={color}]{chip}"));
            }
        }
    }
    if let Some(ports) = t["ports"].as_array().filter(|p| !p.is_empty()) {
        let ports: Vec<String> = ports.iter().filter_map(|p| p.as_u64()).map(|p| format!(":{p}")).collect();
        text.push_str(&format!("  #[fg={}]{}", crate::palette::MUTED.hex(), ports.join(" ")));
    }
    text.push_str("#[default]");
    text
}

/// A change key over everything the status bar renders *except* age.
///
/// Age advances on its own every tick, so digesting it would make "push only on
/// change" mean "push every 2 s forever" — and the bar can carry age forward
/// itself between pushes. Everything else here is a real state change worth a
/// repaint.
fn digest(tasks: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for t in tasks {
        out.push_str(&format!(
            "{}/{} {} {} {} {}\n",
            t["ws_dir"], t["slug"], t["status"], t["waiting_for"], t["sessions"], t["agents"],
        ));
    }
    out
}

/// Deliver one notification through the platform backend (`cli::notify`).
fn notify(note: &Note, title: &str) {
    let subtitle = format!("{} · {}", note.task, note.workspace);
    let body = note.reason.clone().unwrap_or_else(|| "waiting for you".into());
    crate::cli::notify::platform().notify(title, &subtitle, &body);
}

// ── Single-instance bookkeeping ───────────────────────────────────────────────

/// One watcher per tmux server: a non-default `TENX_TMUX_SOCKET` (a build
/// being tried alongside an installed one) gets its own pidfile, so the two
/// never mistake each other for "already running".
fn pid_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("$HOME not set")?;
    let sock = crate::tmux::socket();
    let name = if sock == crate::tmux::SOCKET { "watch.pid".to_string() } else { format!("watch-{sock}.pid") };
    Ok(PathBuf::from(home).join(".config/tenx").join(name))
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
