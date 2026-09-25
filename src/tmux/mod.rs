//! The tmux session layer.
//!
//! tenx runs its own tmux **server** on a dedicated socket (`tmux -L tenx`),
//! started against a generated config, so tenx's theme, status line and hooks
//! never touch the user's own `~/.tmux.conf`. The
//! server holds one session (`tenx`); **windows are tasks** (named by slug,
//! tracked by their stable `@id`), panes are whatever the layout spawned. Any
//! client — a local terminal, or an SSH login from a phone — attaches to the
//! same server with `tenx`, so every surface is the one true session.
//!
//! Tabless on purpose: the generated config blanks tmux's own window list. The
//! task list is the only switcher: the column of `tenx`'s client
//! (`tui::client`), which embeds the session beside it. tmux itself has no
//! list; a plain `tmux -L tenx attach` is for debugging.
//!
//! Every function here is a `tmux -L tenx …` subprocess. `find_bin` doesn't
//! trust `$PATH` because hooks and spawned panes run with whatever environment
//! tmux's server inherited, which can be surprisingly bare.

use anyhow::{bail, Context, Result};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::palette;

/// The dedicated server socket name. Every tenx invocation passes `-L <socket>`.
/// `TENX_TMUX_SOCKET` overrides it so tests (and a second, throwaway server)
/// never touch the real one.
pub const SOCKET: &str = "tenx";

pub fn socket() -> String {
    env::var("TENX_TMUX_SOCKET").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| SOCKET.to_string())
}

/// `env TENX_TMUX_SOCKET=<socket> <bin>` when running on a non-default
/// socket, else just `<bin>` — so every tenx the server spawns (the home
/// watcher's children) stays on the same server. Lets
/// a build be tried side by side with an installed one: same workspaces,
/// separate server, config and watcher.
fn tenx_cmd(tenx_bin: &str) -> String {
    let sock = socket();
    if sock == SOCKET {
        shell_quote(tenx_bin)
    } else {
        format!("env TENX_TMUX_SOCKET={} {}", shell_quote(&sock), shell_quote(tenx_bin))
    }
}
/// The one session on that server.
pub const SESSION: &str = "tenx";
/// The column's window, created with the session and never closed.
pub const HOME_WINDOW: &str = "home";
/// Minimum tmux: 3.3 — what the generated config and the format strings the
/// binary relies on were written against.
pub const MIN_VERSION: (u32, u32) = (3, 3);
/// A terminal narrower than this gets no column beside the task; the list
/// shows over the whole screen on Ctrl+w instead (`tui::client`).
pub const SMALL_CLIENT_COLS: u32 = 100;
/// Per-task cache of the window id (`@12`) last opened for it. A fast path
/// only — `find_task_window` is the source of truth, and a stale id (server
/// restarted) is simply treated as "not open".
pub const WINDOW_ID_FILE: &str = ".tenx-window-id";
/// tmux window option carrying the task directory a window was opened for.
///
/// Window *names* are task slugs, and a slug is only unique within one
/// workspace, so a name can't say which task a window is. This can: it's set
/// once when the window is created, it survives a pane `cd`, and unlike a
/// cached `@id` it can't be aliased by tmux reusing ids after a server
/// restart. Anything that selects or closes a task's window correlates on
/// this, not on the name.
pub const TASK_DIR_OPTION: &str = "@tenx_task_dir";

/// What the last `tenx` client learned about its terminal's keyboard, as
/// `kitty` or `legacy`, then the terminal's name (`legacy WezTerm`). Only the
/// client can ask: `tenx doctor` runs inside a pane, where the terminal it
/// sees is tmux.
pub const KEYBOARD_OPTION: &str = "@tenx_keyboard";

/// One tmux window as `list-windows` reports it.
#[derive(Debug, Clone)]
pub struct Window {
    /// Stable for the server's life (`@12`) — but *only* the server's life:
    /// ids restart at `@0` after a restart, which is why a cached id is never
    /// used to kill anything (see `WINDOW_ID_FILE`).
    pub id: String,
    pub name: String,
    /// The session's current window (what an attaching client lands on).
    pub active: bool,
    /// A process in the window rang the bell / produced output since the
    /// window was last visited — the generic attention signal (see
    /// `tenx_core::status::Signal`).
    pub bell: bool,
    pub activity: bool,
    /// When anything in the window last produced output (tmux's
    /// `window_activity`). Unlike `activity`, this doesn't reset when the
    /// window is visited, so it answers "how long has this been quiet?" —
    /// the only age `sweep` can read for a task with no session left to ask.
    /// `None` when tmux reports it unparseably; callers treat that as "no
    /// idea how old", never as "old enough to close".
    pub last_activity: Option<SystemTime>,
    /// The task this window was opened for (`TASK_DIR_OPTION`). `None` for a
    /// window opened by a tenx that predates the option, or one a user made
    /// by hand — `window_owned_by` then falls back to the panes' paths.
    pub task_dir: Option<PathBuf>,
}

/// The bell/activity flags of every task window, keyed by the task it was
/// opened for — falling back to the window name (= task slug) for a window
/// that predates `TASK_DIR_OPTION`. The home window is excluded: its bells are
/// nobody's task.
///
/// Not keyed by name alone: a slug is unique only within a workspace, so two
/// namesakes collapsed into one entry and shared a bell between them.
pub fn signals_from(windows: &[Window]) -> crate::workspace::Signals {
    windows
        .iter()
        .filter(|w| w.name != HOME_WINDOW)
        .map(|w| {
            let key = w.task_dir.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|| w.name.clone());
            (key, crate::workspace::Signal { bell: w.bell, activity: w.activity })
        })
        .collect()
}

/// `signals_from(list_windows())` — empty when the server is down.
pub fn signals() -> crate::workspace::Signals {
    signals_from(&list_windows().unwrap_or_default())
}

// ── Binary & environment ──────────────────────────────────────────────────────

/// Find the tmux binary without trusting `$PATH`. Homebrew, distro packages,
/// and `~/.local/bin` cover macOS and Linux; the bare name is the last resort.
pub fn find_bin() -> PathBuf {
    let home = env::var("HOME").unwrap_or_default();
    for dir in [
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
        format!("{home}/.local/bin"),
        "/home/linuxbrew/.linuxbrew/bin".to_string(),
    ] {
        let p = PathBuf::from(dir).join("tmux");
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("tmux")
}

fn cmd() -> Command {
    let mut c = Command::new(find_bin());
    c.args(["-L", &socket()]);
    c
}

fn run(args: &[&str]) -> Result<String> {
    let out = cmd().args(args).output().with_context(|| format!("run tmux {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("tmux {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Inside a client of *our* server. `$TMUX` is `<socket path>,<pid>,<index>`,
/// and `-L tenx` names the socket file `tenx`, so the basename is the test —
/// no subprocess needed, which matters because this runs on every keystroke
/// path that decides "switch in place" vs "attach".
pub fn inside_tenx_session() -> bool {
    let sock = socket();
    env::var("TMUX")
        .ok()
        .and_then(|v| v.split(',').next().map(|p| Path::new(p).file_name() == Some(OsStr::new(sock.as_str()))))
        .unwrap_or(false)
}

/// Whether the tenx server is up with its session. `has-session` exits 1 (and
/// fails to connect) when there's no server, which is exactly "not running".
/// (No `=` exact-match prefix on session targets: tmux only honours it for
/// window names, and misparses it here.)
pub fn server_running() -> bool {
    cmd()
        .args(["has-session", "-t", SESSION])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `tmux -V` → (major, minor); errors below [`MIN_VERSION`] with a message
/// that says what to install, rather than failing later on an unknown flag.
pub fn check_version() -> Result<(u32, u32)> {
    let out = Command::new(find_bin()).arg("-V").output().context("run tmux -V (is tmux installed?)")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = parse_version(&text).with_context(|| format!("unrecognised tmux version: {}", text.trim()))?;
    if version < MIN_VERSION {
        bail!(
            "tmux {}.{} is too old — tenx needs {}.{}+",
            version.0,
            version.1,
            MIN_VERSION.0,
            MIN_VERSION.1
        );
    }
    Ok(version)
}

/// "tmux 3.6a" → (3, 6); "tmux next-3.7" → (3, 7).
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let token = text.split_whitespace().last()?;
    let digits: String =
        token.trim_start_matches("next-").chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut parts = digits.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

// ── This binary ───────────────────────────────────────────────────────────────

/// The path to embed for this binary wherever tmux keeps it for the server's
/// life: the generated config's Ctrl+w binding, the home window's restart
/// loop, the watcher, agent panes.
///
/// `current_exe()` is the wrong answer for that on Linux: it reads
/// `/proc/self/exe`, which resolves symlinks, so a Homebrew install yields the
/// versioned Cellar path that the next `brew upgrade` deletes — and the panes
/// the config spawns die with it. The path we were *invoked* by (`argv[0]`, made
/// absolute but with symlinks kept) is the stable `bin/tenx` link for any
/// package-managed install. macOS `current_exe()` already behaves that way;
/// this makes both platforms agree. Falls back to `current_exe()` when
/// `argv[0]` is unusable (empty, or not found on `PATH`).
pub fn self_bin() -> Result<PathBuf> {
    let argv0 = env::args_os().next().map(PathBuf::from);
    let cwd = env::current_dir().ok();
    let resolved = argv0.and_then(|a| resolve_argv0(&a, cwd.as_deref(), crate::cli::notify::which, crate::cli::notify::is_executable));
    match resolved {
        Some(p) => Ok(p),
        None => env::current_exe().context("locate the tenx binary"),
    }
}

/// The decision behind [`self_bin`], with the filesystem abstracted so it can
/// be unit-tested: an `argv[0]` with a slash is a path (relative to `cwd`), a
/// bare name is looked up on `PATH`; either must exist and be executable.
fn resolve_argv0(
    argv0: &Path,
    cwd: Option<&Path>,
    which: impl Fn(&str) -> Option<PathBuf>,
    is_exe: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if argv0.as_os_str().is_empty() {
        return None;
    }
    let is_bare = argv0.components().count() == 1 && !argv0.to_string_lossy().contains('/');
    if is_bare {
        return which(&argv0.to_string_lossy());
    }
    let abs = if argv0.is_absolute() { argv0.to_path_buf() } else { cwd?.join(argv0) };
    // `components()` drops interior `.` segments (`/work/./target` → `/work/target`)
    // without touching symlinks, which is the whole point of this function.
    let abs: PathBuf = abs.components().collect();
    is_exe(&abs).then_some(abs)
}

/// The version of the tenx that started the running server, from the
/// `@tenx_version` option the generated config sets. `None` when no server is
/// up or the config predates the option.
pub fn server_version() -> Option<String> {
    let v = run(&["show-option", "-gqv", "@tenx_version"]).ok()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Text for the user when the running server was started by another tenx
/// than this one: tmux read its config (and this binary's path) once, at
/// server start, so an upgrade only lands after a restart.
pub fn stale_server_hint(running: &str) -> Option<String> {
    let mine = env!("CARGO_PKG_VERSION");
    (running != mine).then(|| {
        format!(
            "tenx {mine} is installed but the running session was started by tenx {running}.\n\
             Restart it to pick up the new binary: tmux -L {} kill-server, then tenx.",
            socket()
        )
    })
}

// ── Config ────────────────────────────────────────────────────────────────────

/// `~/.config/tenx/tmux.conf`, regenerated on every session creation so it can
/// never drift from the installed binary (the config embeds its path).
pub fn config_path() -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME not set")?;
    let sock = socket();
    let name = if sock == SOCKET { "tmux.conf".to_string() } else { format!("tmux-{sock}.conf") };
    Ok(PathBuf::from(home).join(".config").join("tenx").join(name))
}

/// Write the generated config and return its path. Only read by tmux when the
/// *server* starts, so a running session keeps its config until restarted —
/// same as the zellij version, and fine: nothing in here changes per task.
pub fn write_config() -> Result<PathBuf> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    fs::write(&path, render_config()).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// The whole server config. Theme colours come from [`crate::palette`] — the
/// same constants the column draws with, so chrome and column read as one
/// design. The window list is blanked (tabless: the column is the switcher);
/// `status-left` shows this window's task and status from the `@tenx_status`
/// user option `tenx watch` maintains, falling back to the window name (the
/// slug) until the first push.
pub fn render_config() -> String {
    format!(
        r##"# Generated by tenx — do not edit; regenerated whenever the tenx session is created.
# This server (`tmux -L {socket}`) is tenx's own; your ~/.tmux.conf is untouched.

# The tenx that wrote this config; `tenx` compares it with its own version on
# attach and says when a restart is due.
set -g @tenx_version "{version}"

set -g default-terminal "tmux-256color"
set -ga terminal-overrides ",*:Tc"
# No "faint" (SGR 2): terminals draw it at half brightness, and agents use
# it for most of their secondary text — hard to read on a dark ground.
# zellij never honoured it either, which is what that text used to look like.
set -ga terminal-overrides ",*:dim@"
set -g escape-time 10
set -g focus-events on
# Extended keys: agents (Codex, Claude, pi) read Shift+Enter for a newline, and
# pi warns at startup without this. Passthrough lets an agent's OSC escapes
# (Codex desktop notifications, pi progress) reach the outer terminal instead of
# being swallowed here.
set -s extended-keys on
set -as terminal-features "xterm*:extkeys"
set -g allow-passthrough on
# OSC 8 hyperlinks: tmux keeps them per cell but only sends them to a client
# whose terminal claims the feature, and it infers that for none — without
# this a link printed in a pane reaches the client as plain text.
set -as terminal-features ",*:hyperlinks"
set -g mouse on
set -g history-limit 50000
set -g renumber-windows on
set -g set-titles on
set -g set-titles-string "tenx: #W"

# Attention: a bell from *any* process in a task's pane flags its window
# (`window_bell_flag`), which `tenx watch` reads alongside Claude Code's own
# session state. Silent here — the column and the status line are the display.
set -g monitor-bell on
set -g bell-action any
set -g visual-bell off

# ── Theme (crate::palette) ───────────────────────────────────────────────────
set -g status on
set -g status-position bottom
set -g status-interval 5
set -g status-justify left
set -g status-style "bg={ground},fg={text}"
set -g message-style "bg={ground},fg={bright}"
set -g message-command-style "bg={ground},fg={accent}"
set -g mode-style "bg={accent},fg={ground}"
set -g pane-border-style "fg={border}"
set -g pane-active-border-style "fg={border_active}"
set -g pane-border-lines single

# Every pane sits on the same ground as the chrome, with the palette's text
# colour as the default foreground — not the terminal's own white-on-black,
# which reads harsher next to the column.
set -g window-style "fg={text},bg={ground}"
set -g window-active-style "fg={text},bg={ground}"

# Tabless: the column is the only task list. Hide tmux's window list entirely.
set -g window-status-format ""
set -g window-status-current-format ""
set -g window-status-separator ""

# Left: this window's task + status (pushed by `tenx watch`), else its name.
set -g status-left-length 80
set -g status-left " #{{?#{{@tenx_status}},#{{E:@tenx_status}},#[fg={accent}]#W}} #[default]"
# Right: what else is waiting on you, pushed by `tenx watch`.
set -g status-right-length 100
set -g status-right "#{{E:@tenx_right}} "

"##,
        socket = socket(),
        version = env!("CARGO_PKG_VERSION"),
        ground = palette::GROUND.hex(),
        text = palette::TEXT.hex(),
        bright = palette::BRIGHT.hex(),
        accent = palette::ACCENT.hex(),
        border = palette::BORDER.hex(),
        border_active = palette::BORDER_ACTIVE.hex(),
    )
}

// ── Session lifecycle ─────────────────────────────────────────────────────────

/// Start the server (from the generated config) and the session detached if
/// they aren't running, for the client to attach to in a pty of its own
/// (`tui::client`). `-f` is only consulted when the server actually starts.
///
/// Window 0 is `home`: a plain shell in `$HOME`, never a task window, so the
/// session always has a window and a fresh install has somewhere to land.
pub fn ensure_session() -> Result<()> {
    if server_running() {
        return Ok(());
    }
    let conf = write_config()?;
    let home = env::var("HOME").context("HOME not set")?;
    // Start the server from $HOME, not from wherever tenx was run: a server
    // outlives the directory it was started in, and tenx is habitually run
    // from inside a task that later gets deleted.
    let status = cmd()
        .current_dir(&home)
        .args(["-f", &conf.to_string_lossy()])
        .args(["new-session", "-d", "-s", SESSION, "-n", HOME_WINDOW, "-c", &home])
        .status()
        .context("run tmux new-session")?;
    if !status.success() {
        bail!("tmux new-session exited with {status}");
    }
    Ok(())
}

/// The `tmux -L <socket> attach-session -t tenx` a client runs in its pty.
pub fn attach_command() -> (PathBuf, Vec<String>) {
    (find_bin(), vec!["-L".into(), socket(), "attach-session".into(), "-t".into(), SESSION.into()])
}

// ── Windows ───────────────────────────────────────────────────────────────────

const WINDOW_FORMAT: &str = "#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}\t#{window_bell_flag}\t#{window_activity_flag}\t#{window_activity}\t#{@tenx_task_dir}";

/// Every window of the session. Empty (not an error) when the server is down
/// — one subprocess either way: a failed `list-windows` *is* the liveness
/// check, so there's no separate `has-session` round trip on a poll path.
pub fn list_windows() -> Result<Vec<Window>> {
    let out = cmd()
        .args(["list-windows", "-t", SESSION, "-F", WINDOW_FORMAT])
        .stdin(Stdio::null())
        .output()
        .context("run tmux list-windows")?;
    if !out.status.success() {
        return Ok(vec![]);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().filter_map(parse_window).collect())
}

/// Every pane in the session as (window name, pane pid) — the process roots
/// `live::ports_by_window` walks. Empty when the server is down.
pub fn list_pane_pids() -> Result<Vec<(String, u32)>> {
    if !server_running() {
        return Ok(vec![]);
    }
    let text = run(&["list-panes", "-s", "-t", SESSION, "-F", "#{window_name}\t#{pane_pid}"])?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let (w, p) = l.split_once('\t')?;
            Some((w.to_string(), p.trim().parse().ok()?))
        })
        .collect())
}

/// Every window's panes and where each one currently sits, keyed by window id.
/// Empty when the server is down.
///
/// Windows are named by task slug, and a slug is only unique *within* a
/// workspace — so a name is not an identity. This is: a task's window is the
/// one whose panes are actually in the task's directory tree. `sweep` needs
/// that distinction because it acts on the window it matched, and matching the
/// wrong one means closing somebody else's live session.
pub fn pane_paths_by_window() -> Result<HashMap<String, Vec<PathBuf>>> {
    if !server_running() {
        return Ok(HashMap::new());
    }
    let text = run(&["list-panes", "-s", "-t", SESSION, "-F", "#{window_id}\t#{pane_current_path}"])?;
    let mut out: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for line in text.lines() {
        let Some((id, path)) = line.split_once('\t') else { continue };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        // Resolved, because tmux reports where a pane *really* is: on macOS a
        // task under `/var/folders/…` comes back as `/private/var/folders/…`,
        // and a raw prefix test would then match nothing at all.
        let path = PathBuf::from(path);
        out.entry(id.to_string()).or_default().push(fs::canonicalize(&path).unwrap_or(path));
    }
    Ok(out)
}

/// Does `window_id` belong to the task rooted at `task_dir`? True when any of
/// its panes sits in the task's tree — the task dir itself, or a repo checked
/// out inside it (an agent that `cd`s into a worktree is still in its task).
///
/// Deliberately conservative: a window whose every pane has wandered *outside*
/// the tree reads as "not this task's", so a caller that closes things closes
/// nothing rather than the wrong thing.
pub fn window_owned_by(w: &Window, paths: &HashMap<String, Vec<PathBuf>>, task_dir: &Path) -> bool {
    // Both sides resolved, or a symlink anywhere above the task (`/var` →
    // `/private/var`, a symlinked home) makes every window look like nobody's.
    let task = resolved(task_dir);
    // A window tenx opened says which task it is, whatever its panes have
    // since `cd`'d to — the answer, when it's there.
    if let Some(tagged) = &w.task_dir {
        return resolved(tagged) == task;
    }
    // Opened before the option existed: fall back to where the panes are.
    // A window whose every pane has wandered outside the tree reads as
    // nobody's, so a caller that closes things closes nothing rather than
    // the wrong thing.
    paths.get(&w.id).is_some_and(|ps| ps.iter().any(|p| p.starts_with(&task)))
}

fn resolved(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The window belonging to the task at `task_dir` — the one correlation any
/// caller that selects or closes a window should use. Narrows by name (cheap,
/// and a task's window is always named by its slug) and then settles which of
/// the namesakes is actually this task's.
///
/// `None` means "this task has no window", and a caller may then open one. It
/// deliberately does *not* fall back to "some window with the right name":
/// that's how a task in one workspace ends up driving a namesake's live
/// session in another.
pub fn find_task_window(slug: &str, task_dir: &Path) -> Result<Option<Window>> {
    if slug == HOME_WINDOW {
        return Ok(None); // a task slugged `home` must never reach the column
    }
    let named: Vec<Window> = list_windows()?.into_iter().filter(|w| w.name == slug).collect();
    if named.is_empty() {
        return Ok(None);
    }
    // Only an untagged window needs the panes looked up — after one reopen
    // every window carries its task, and this costs no extra subprocess.
    let paths = if named.iter().any(|w| w.task_dir.is_none()) {
        pane_paths_by_window().unwrap_or_default()
    } else {
        HashMap::new()
    };
    Ok(named.into_iter().find(|w| window_owned_by(w, &paths, task_dir)))
}

/// Slugs that can't be task names because they collide with tmux windows
/// tenx owns itself.
pub fn is_reserved_slug(slug: &str) -> bool {
    slug == HOME_WINDOW
}

pub fn select_window(id: &str) -> Result<()> {
    run(&["select-window", "-t", id]).map(drop)
}

/// Make `pane` the one on screen: its window the session's current, and the
/// pane that window's active one.
pub fn focus_pane(pane: &str) -> Result<()> {
    run(&["select-window", "-t", pane])?;
    run(&["select-pane", "-t", pane]).map(drop)
}

/// The visible contents of a pane, with its colours (`-e` keeps the SGR
/// sequences) — what `A`/`D` check before answering a permission prompt.
pub fn capture_pane(target: &str) -> Result<String> {
    run(&["capture-pane", "-p", "-e", "-t", target])
}

/// Type one tmux key name (`Enter`, `Escape`) into a pane — how the column
/// answers a permission dialog without visiting the window. Callers check the
/// dialog is actually on screen first (`tenx_core::dialog`).
pub fn send_keys(target: &str, key: &str) -> Result<()> {
    run(&["send-keys", "-t", target, key]).map(drop)
}

pub fn kill_window(id: &str) -> Result<()> {
    run(&["kill-window", "-t", id]).map(drop)
}

/// Set a per-window user option (`@tenx_status`) — the push channel the
/// status line reads, so a change costs one subprocess and steady state none.
pub fn set_window_option(id: &str, option: &str, value: &str) -> Result<()> {
    run(&["set-option", "-w", "-t", id, option, value]).map(drop)
}

pub fn set_global_option(option: &str, value: &str) -> Result<()> {
    run(&["set-option", "-g", option, value]).map(drop)
}

/// Read an option's value from the running server, trying global then server
/// scope (`allow-passthrough` is `-g`, `extended-keys` is `-s`). `None` if
/// unset or unreadable. For `tenx doctor`.
pub fn show_global_option(option: &str) -> Option<String> {
    for scope in ["-gv", "-sv"] {
        if let Ok(v) = run(&["show-options", scope, option]) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Open a small pane at the bottom of `window_id` following a background
/// agent's transcript (`tenx internal agent-log`). `-d` keeps focus where it
/// is: the agent appearing is news, not an interruption. Sized in lines, not
/// a share, so it costs the same on a tall window as a short one.
pub fn open_agent_pane(window_id: &str, tenx_bin: &str, cwd: &str, pid: u32, session_id: Option<&str>, agent: &str) -> Result<()> {
    let session = session_id.map(|s| format!(" --session {}", shell_quote(s))).unwrap_or_default();
    let agent_flag = format!(" --agent {}", shell_quote(agent));
    let command = format!("{} internal agent-log {} {pid}{session}{agent_flag}", tenx_cmd(tenx_bin), shell_quote(cwd));
    run(&["split-window", "-d", "-v", "-l", "12", "-t", window_id, "-c", cwd, &command]).map(drop)
}

/// The `tenx internal agent-log` arguments that follow one subagent's
/// transcript as a popup (`--popup`: q/Esc closes it).
pub fn subagent_log_args(cwd: &str, session_pid: u32, agent: &str, transcript: &str, title: &str) -> String {
    format!(
        "internal agent-log {} {session_pid} --agent {} --transcript {} --title {} --popup",
        shell_quote(cwd),
        shell_quote(agent),
        shell_quote(transcript),
        shell_quote(title.trim())
    )
}

/// Where no popup can be aimed (no tmux client found for the embedded
/// terminal): the same viewer as a pane split into the task's window, focused.
pub fn open_subagent_pane(window_id: &str, tenx_bin: &str, cwd: &str, args: &str) -> Result<()> {
    let command = format!("{} {args}", tenx_cmd(tenx_bin));
    run(&["split-window", "-v", "-l", "40%", "-t", window_id, "-c", cwd, &command]).map(drop)
}

// ── Popups ────────────────────────────────────────────────────────────────────

/// The name of the tmux client whose process is `pid` — the tenx client's
/// own embedded `tmux attach`, so a popup lands in front of the person who
/// asked for it and not on some other attached terminal.
pub fn client_by_pid(pid: u32) -> Option<String> {
    let out = run(&["list-clients", "-F", "#{client_pid} #{client_name}"]).ok()?;
    out.lines().find_map(|l| {
        let (p, name) = l.split_once(' ')?;
        (p.parse::<u32>().ok()? == pid).then(|| name.to_string())
    })
}

/// Run `tenx <args>` in a popup over `client`, in `cwd`, titled `title`.
/// Blocks until the popup closes (`-E`: when the command exits) and returns
/// the command's exit status — so call it off the drawing thread: the
/// client's own loop is what forwards keystrokes into the popup.
pub fn popup_tenx(client: &str, cwd: &str, title: &str, tenx_bin: &str, args: &str) -> Result<i32> {
    let command = format!("{} {args}", tenx_cmd(tenx_bin));
    // Drawn in the column's colours, not the terminal's defaults: its ground
    // and body text, and the frame of the active pane (a popup has the
    // keyboard). Per-popup flags rather than the server's popup options, so
    // no restart is needed for them to apply.
    let body = format!("bg={},fg={}", palette::GROUND.hex(), palette::TEXT.hex());
    let frame = format!("fg={}", palette::BORDER_ACTIVE.hex());
    let status = cmd()
        .args(["display-popup", "-c", client, "-E", "-d", cwd, "-w", "80%", "-h", "70%", "-T", title])
        .args(["-b", "single", "-s", &body, "-S", &frame, &command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("run tmux display-popup")?;
    Ok(status.code().unwrap_or(1))
}

// ── Current window ────────────────────────────────────────────────────────────

/// The name of the session's current window (`None` for the home window
/// or when the server is down) — the task a client is looking at, asked
/// live rather than read from the watcher's snapshot, which can lag a
/// switch by a couple of seconds.
pub fn current_task() -> Option<String> {
    let name = run(&["display-message", "-p", "-t", SESSION, "#{window_name}"]).ok()?.trim().to_string();
    (!name.is_empty() && name != HOME_WINDOW).then_some(name)
}

/// What a task window needs to be built.
pub struct TaskWindow<'a> {
    /// Window name — the immutable slug, so task↔window correlation can't drift.
    pub slug: &'a str,
    pub title: &'a str,
    pub task_dir: &'a str,
    pub workspace_dir: &'a str,
    /// Executable to run instead of the built-in layout (`config.toml`'s
    /// `layout`). Gets the window with a plain shell in its first pane.
    pub layout_script: Option<&'a str>,
    /// The agent token (`claude`, `codex`, `pi`) — exported as `TENX_AGENT` for
    /// layout scripts.
    pub agent: &'a str,
    /// The fully-built command to run in the task's first pane (e.g.
    /// `claude --name 'slug' --continue`). Built by `agent::AgentKind`, which
    /// already decided any resume flag, so tmux just runs it.
    pub agent_cmd: &'a str,
    /// Create the window without making it the session's current one.
    ///
    /// `new-window` selects what it creates, and the session's current window
    /// is what every attached client shows — so opening a task the ordinary
    /// way drags every terminal to it. A task created from the column wants
    /// its window (and its agent) running, but not your screen.
    pub detached: bool,
}

/// Create a task's window and its panes, returning the window's stable id.
/// The window becomes the session's current one — so a client that's attached
/// (or about to attach) lands on it — unless `opts.detached`.
///
/// Built-in layout — claude on the left, nvim on `TASK.md` top-right, a shell
/// bottom-right — mirrors the zellij default. A pane whose command exits
/// closes (tmux's default), which is what `close_on_exit` did.
pub fn open_task_window(opts: &TaskWindow) -> Result<String> {
    // `tenx:` (trailing colon) targets the session so the new window is
    // appended to it rather than treated as a window name to match.
    let session = format!("{SESSION}:");
    let first_cmd = if opts.layout_script.is_some() { None } else { Some(opts.agent_cmd) };
    let mut args = vec!["new-window", "-t", &session, "-n", opts.slug, "-c", opts.task_dir, "-P", "-F", "#{window_id}"];
    if opts.detached {
        args.push("-d");
    }
    if let Some(c) = first_cmd {
        args.push(c);
    }
    let id = run(&args)?.trim().to_string();
    if id.is_empty() {
        bail!("tmux new-window returned no window id");
    }
    // Stamp the task on the window before anything can look it up.
    let _ = run(&["set-option", "-w", "-t", &id, TASK_DIR_OPTION, opts.task_dir]);

    if let Some(script) = opts.layout_script {
        let status = Command::new(script)
            .env("TENX_WINDOW", &id)
            .env("TENX_SLUG", opts.slug)
            .env("TENX_TITLE", opts.title)
            .env("TENX_TASK_DIR", opts.task_dir)
            .env("TENX_WS_DIR", opts.workspace_dir)
            .env("TENX_AGENT", opts.agent)
            .env("TENX_AGENT_CMD", opts.agent_cmd)
            // Back-compat: layout scripts written for the Claude-only tenx read
            // TENX_CLAUDE_CMD. Kept one release; prefer TENX_AGENT_CMD.
            .env("TENX_CLAUDE_CMD", opts.agent_cmd)
            .env("TENX_TMUX", format!("{} -L {}", find_bin().display(), socket()))
            .status()
            .with_context(|| format!("run layout script {script}"))?;
        if !status.success() {
            bail!("layout script {script} exited with {status}");
        }
        return Ok(id);
    }

    // Right half: nvim on TASK.md, then split that pane for the shell below it.
    // `-d` keeps focus where it is so the final `select-pane` is deterministic.
    run(&["split-window", "-h", "-t", &id, "-c", opts.task_dir, "-l", "50%", "nvim TASK.md"])?;
    run(&["split-window", "-v", "-t", &id, "-c", opts.task_dir, "-l", "50%"])?;
    let _ = run(&["select-pane", "-t", &format!("{id}.0")]);
    Ok(id)
}

fn parse_window(line: &str) -> Option<Window> {
    let mut f = line.split('\t');
    Some(Window {
        id: f.next()?.to_string(),
        name: f.nth(1)?.to_string(), // skip the index column

        active: f.next()? == "1",
        bell: f.next()? == "1",
        activity: f.next()? == "1",
        // Absent on a tmux that doesn't report it: the window still lists,
        // it just has no age — `sweep` then leaves it alone.
        last_activity: f
            .next()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs)),
        // Empty when the option isn't set (tmux prints nothing for it).
        task_dir: f.next().map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from),
    })
}

/// POSIX single-quote quoting for the shell strings tmux runs.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_and_next_versions() {
        assert_eq!(parse_version("tmux 3.6a\n"), Some((3, 6)));
        assert_eq!(parse_version("tmux 3.2\n"), Some((3, 2)));
        assert_eq!(parse_version("tmux next-3.7\n"), Some((3, 7)));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn parses_window_lines() {
        let w = parse_window("@3\t2\tadd-repos\t1\t0\t1").unwrap();
        assert_eq!((w.id.as_str(), w.name.as_str()), ("@3", "add-repos"));
        assert!(w.active && !w.bell && w.activity);
        assert!(parse_window("@3\tx").is_none());
    }

    fn win(id: &str, name: &str, task_dir: Option<&str>) -> Window {
        Window {
            id: id.into(),
            name: name.into(),
            active: false,
            bell: false,
            activity: false,
            last_activity: None,
            task_dir: task_dir.map(PathBuf::from),
        }
    }

    #[test]
    fn a_tagged_window_belongs_to_the_task_it_was_opened_for() {
        // Two windows with the same name — the shape every "which window is
        // this task's" question has to survive.
        let a = PathBuf::from("/ws-a/tasks/dup");
        let b = PathBuf::from("/ws-b/tasks/dup");
        let wa = win("@1", "dup", Some("/ws-a/tasks/dup"));
        let wb = win("@2", "dup", Some("/ws-b/tasks/dup"));
        let no_panes = HashMap::new();
        assert!(window_owned_by(&wa, &no_panes, &a));
        assert!(!window_owned_by(&wa, &no_panes, &b));
        assert!(window_owned_by(&wb, &no_panes, &b));
        assert!(!window_owned_by(&wb, &no_panes, &a));
        // The tag is the answer even when the panes have wandered off — the
        // case the pane-path fallback alone gets wrong.
        let wandered = HashMap::from([("@1".to_string(), vec![PathBuf::from("/elsewhere")])]);
        assert!(window_owned_by(&wa, &wandered, &a));
    }

    #[test]
    fn a_window_belongs_to_the_task_its_panes_sit_in() {
        // Two windows with the same name — the shape `sweep` has to tell
        // apart, since a slug is only unique within a workspace.
        let a = PathBuf::from("/ws-a/tasks/dup");
        let b = PathBuf::from("/ws-b/tasks/dup");
        let paths = HashMap::from([
            ("@1".to_string(), vec![a.join("repo"), a.clone()]),
            ("@2".to_string(), vec![b.clone()]),
        ]);
        // Untagged: opened by a tenx that predates `TASK_DIR_OPTION`.
        let w1 = win("@1", "dup", None);
        let w2 = win("@2", "dup", None);
        assert!(window_owned_by(&w1, &paths, &a));
        assert!(!window_owned_by(&w1, &paths, &b));
        assert!(window_owned_by(&w2, &paths, &b));
        // A task with no window of its own owns none of them, however it's
        // named — the case that used to close somebody else's session.
        assert!(!window_owned_by(&w1, &paths, Path::new("/ws-c/tasks/dup")));
        assert!(!window_owned_by(&win("@9", "dup", None), &paths, &a));
    }

    #[test]
    fn home_is_never_a_task_window() {
        let windows = vec![
            Window { id: "@0".into(), name: HOME_WINDOW.into(), active: true, bell: true, activity: true, last_activity: None, task_dir: None },
            Window { id: "@1".into(), name: "foo".into(), active: false, bell: true, activity: false, last_activity: None, task_dir: None },
        ];
        let s = signals_from(&windows);
        assert!(!s.contains_key(HOME_WINDOW));
        assert!(s["foo"].bell); // untagged: still keyed by slug
        assert!(is_reserved_slug("home") && !is_reserved_slug("homer"));
    }

    #[test]
    fn namesake_windows_do_not_share_a_bell() {
        let mut a = win("@1", "dup", Some("/ws-a/tasks/dup"));
        a.bell = true;
        let b = win("@2", "dup", Some("/ws-b/tasks/dup"));
        let s = signals_from(&[a, b]);
        // Keyed by task, so the second no longer overwrites the first.
        assert!(s["/ws-a/tasks/dup"].bell);
        assert!(!s["/ws-b/tasks/dup"].bell);
        assert!(!s.contains_key("dup"));
    }

    #[test]
    fn argv0_keeps_the_invoked_path_and_falls_back_sensibly() {
        let which = |name: &str| (name == "tenx").then(|| PathBuf::from("/opt/homebrew/bin/tenx"));
        let is_exe = |p: &Path| p == Path::new("/home/me/.local/bin/tenx") || p == Path::new("/work/target/release/tenx");
        let cwd = Some(Path::new("/work"));
        // Bare name → PATH lookup, symlink kept.
        assert_eq!(resolve_argv0(Path::new("tenx"), cwd, which, is_exe), Some(PathBuf::from("/opt/homebrew/bin/tenx")));
        // Absolute path → itself, if executable.
        assert_eq!(
            resolve_argv0(Path::new("/home/me/.local/bin/tenx"), cwd, which, is_exe),
            Some(PathBuf::from("/home/me/.local/bin/tenx"))
        );
        // Relative path → against cwd.
        assert_eq!(
            resolve_argv0(Path::new("./target/release/tenx"), cwd, which, is_exe),
            Some(PathBuf::from("/work/target/release/tenx"))
        );
        // Unknown name, missing file, empty → None (caller falls back to current_exe).
        assert_eq!(resolve_argv0(Path::new("nope"), cwd, which, is_exe), None);
        assert_eq!(resolve_argv0(Path::new("/gone/tenx"), cwd, which, is_exe), None);
        assert_eq!(resolve_argv0(Path::new(""), cwd, which, is_exe), None);
        assert_eq!(resolve_argv0(Path::new("./tenx"), None, which, is_exe), None);
    }

    #[test]
    fn stale_server_hint_only_on_mismatch() {
        assert!(stale_server_hint(env!("CARGO_PKG_VERSION")).is_none());
        // A version no build can have, so the mismatch is real whatever
        // Cargo.toml says.
        let hint = stale_server_hint("0.0.0-stale").unwrap();
        assert!(hint.contains("0.0.0-stale") && hint.contains("kill-server"));
    }

    #[test]
    fn config_stamps_this_version() {
        let conf = render_config();
        assert!(conf.contains(&format!("set -g @tenx_version \"{}\"", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn config_embeds_palette() {
        let c = render_config();
        assert!(!c.contains("display-popup"), "no popup: the column is outside tmux");
        assert!(c.contains(&palette::ACCENT.hex()));
        assert!(c.contains("set -g monitor-bell on"));
        // The non-Claude TUIs need these: extended keys for Shift+Enter (pi
        // warns without it) and passthrough so agent OSC escapes reach outside.
        assert!(c.contains("set -s extended-keys on"));
        assert!(c.contains("set -g allow-passthrough on"));
        assert!(c.contains(",*:hyperlinks"));
        assert!(c.contains("#{?#{@tenx_status},#{E:@tenx_status},"));
        assert!(c.contains(",*:dim@"));
        assert!(c.contains("set -g window-style \"fg="));
    }
}
