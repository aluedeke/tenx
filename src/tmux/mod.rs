//! The tmux session layer.
//!
//! tenx runs its own tmux **server** on a dedicated socket (`tmux -L tenx`),
//! started against a generated config, so tenx's theme, status line, hooks and
//! the Ctrl+w popup keybind never touch the user's own `~/.tmux.conf`. The
//! server holds one session (`tenx`); **windows are tasks** (named by slug,
//! tracked by their stable `@id`), panes are whatever the layout spawned. Any
//! client — a local terminal, or an SSH login from a phone — attaches to the
//! same server with `tenx`, so every surface is the one true session.
//!
//! Tabless on purpose: the generated config blanks tmux's own window list. The
//! overlay (`tenx overlay`, the home window's permanent pane and the Ctrl+w
//! `display-popup`) is the only switcher, exactly as before — the difference
//! is that `display-popup` is per-client and runs the same binary, so there is
//! one overlay implementation instead of a native one plus a wasm one.
//!
//! Every function here is a `tmux -L tenx …` subprocess. `find_bin` doesn't
//! trust `$PATH` because hooks and spawned panes run with whatever environment
//! tmux's server inherited, which can be surprisingly bare.

use anyhow::{bail, Context, Result};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
/// overlay, the popup, the watcher's children) stays on the same server. Lets
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
/// The overlay's window, created with the session and never closed.
pub const HOME_WINDOW: &str = "home";
/// Minimum tmux: `display-popup -T` and `popup-border-style` are 3.3 (the
/// popup itself is 3.2, but the generated config uses both).
pub const MIN_VERSION: (u32, u32) = (3, 3);
/// A client narrower or shorter than this gets the overlay full screen
/// instead of as a bordered 85% popup (see `render_config`).
pub const SMALL_CLIENT_COLS: u32 = 100;
pub const SMALL_CLIENT_ROWS: u32 = 30;
/// Per-task cache of the window id (`@12`) last opened for it. A fast path
/// only — `find_window` by slug is the source of truth, and a stale id (server
/// restarted) is simply treated as "not open".
pub const WINDOW_ID_FILE: &str = ".tenx-window-id";

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
}

/// The bell/activity flags of every task window, keyed by window name (= task
/// slug). The home window is excluded: its bells are nobody's task.
pub fn signals_from(windows: &[Window]) -> crate::workspace::Signals {
    windows
        .iter()
        .filter(|w| w.name != HOME_WINDOW)
        .map(|w| (w.name.clone(), crate::workspace::Signal { bell: w.bell, activity: w.activity }))
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

/// Inside *any* tmux client (ours or the user's own server).
pub fn inside_any_tmux() -> bool {
    env::var_os("TMUX").is_some()
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
            "tmux {}.{} is too old — tenx needs {}.{}+ (display-popup with a title, popup styling)",
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
/// versioned Cellar path that the next `brew upgrade` deletes — and the popup
/// binding dies with it. The path we were *invoked* by (`argv[0]`, made
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
/// than this one: tmux read its config (and the popup's binary path) once, at
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
/// never drift from the installed binary (the popup keybind embeds its path).
pub fn config_path() -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME not set")?;
    let sock = socket();
    let name = if sock == SOCKET { "tmux.conf".to_string() } else { format!("tmux-{sock}.conf") };
    Ok(PathBuf::from(home).join(".config").join("tenx").join(name))
}

/// Write the generated config and return its path. Only read by tmux when the
/// *server* starts, so a running session keeps its config until restarted —
/// same as the zellij version, and fine: nothing in here changes per task.
pub fn write_config(tenx_bin: &str) -> Result<PathBuf> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    fs::write(&path, render_config(tenx_bin)).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// The whole server config. Theme colours come from [`crate::palette`] — the
/// same constants the overlay draws with, so chrome and overlay read as one
/// design. The window list is blanked (tabless: the overlay is the switcher);
/// `status-left` shows this window's task and status from the `@tenx_status`
/// user option `tenx watch` maintains, falling back to the window name (the
/// slug) until the first push.
pub fn render_config(tenx_bin: &str) -> String {
    let q = tenx_cmd(tenx_bin);
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
set -g mouse on
set -g history-limit 50000
set -g renumber-windows on
set -g set-titles on
set -g set-titles-string "tenx: #W"

# Attention: a bell from *any* process in a task's pane flags its window
# (`window_bell_flag`), which `tenx watch` reads alongside Claude Code's own
# session state. Silent here — the overlay and the status line are the display.
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
# The border cells need the ground too: without a bg they take the terminal's
# own default (black), which reads as a thick dark frame around the popup.
# Accent for the line itself: the popup sits on the same surface as the panes
# beneath it, so a muted frame would leave it with no edge at all.
set -g popup-border-style "fg={accent},bg={ground}"
set -g popup-border-lines rounded
set -g popup-style "bg={ground},fg={text}"

# Every pane sits on the same ground as the chrome, with the palette's text
# colour as the default foreground — not the terminal's own white-on-black,
# which reads harsher and makes the popup look like a different app.
set -g window-style "fg={text},bg={ground}"
set -g window-active-style "fg={text},bg={ground}"

# Tabless: the overlay is the only task list. Hide tmux's window list entirely.
set -g window-status-format ""
set -g window-status-current-format ""
set -g window-status-separator ""

# Left: this window's task + status (pushed by `tenx watch`), else its name.
set -g status-left-length 80
set -g status-left " #{{?#{{@tenx_status}},#{{E:@tenx_status}},#[fg={accent}]#W}} #[default]"
# Right: what else is waiting on you, pushed by `tenx watch`.
set -g status-right-length 100
set -g status-right "#{{E:@tenx_right}} "

# Ctrl+w: the overlay as a per-client popup. `-E` closes it when the overlay
# exits, which it does right after a jump. On a small client (a phone) the
# popup fills the screen with no border — an 85% window on 40 columns wastes
# a fifth of them; `if-shell -F` evaluates the format against the client
# that pressed the key.
bind -n C-w if-shell -F "#{{||:#{{<:#{{client_width}},{small_cols}}},#{{<:#{{client_height}},{small_rows}}}}}" {{
    display-popup -E -B -w 100% -h 100% {tenx} overlay
}} {{
    display-popup -E -w 85% -h 85% -T " tenx " {tenx} overlay
}}
"##,
        small_cols = SMALL_CLIENT_COLS,
        small_rows = SMALL_CLIENT_ROWS,
        socket = socket(),
        version = env!("CARGO_PKG_VERSION"),
        tenx = q,
        ground = palette::GROUND.hex(),
        text = palette::TEXT.hex(),
        bright = palette::BRIGHT.hex(),
        accent = palette::ACCENT.hex(),
        border = palette::BORDER.hex(),
        border_active = palette::BORDER_ACTIVE.hex(),
    )
}

// ── Session lifecycle ─────────────────────────────────────────────────────────

/// Attach to the tenx session, creating the server (from the generated config)
/// and the session if needed. Replaces the current process: tmux takes over the
/// terminal. `new-session -A` is attach-or-create in one call, and `-f` is only
/// consulted when the server actually starts, so passing it always is safe.
///
/// The home window runs the overlay in a restart loop: in home mode the overlay
/// never quits on purpose, but if it ever dies the window would go with it —
/// and with it, when it's the last window, the whole session.
pub fn attach_or_create(tenx_bin: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let conf = write_config(tenx_bin)?;
    let home = env::var("HOME").context("HOME not set")?;
    let home_cmd = format!("while :; do {} overlay --home; sleep 1; done", tenx_cmd(tenx_bin));
    // Start the server from $HOME, not from wherever tenx was run: a server
    // outlives the directory it was started in, and tenx is habitually run
    // from inside a task that later gets deleted.
    let err = cmd()
        .current_dir(&home)
        .args(["-f", &conf.to_string_lossy()])
        .args(["new-session", "-A", "-s", SESSION, "-n", HOME_WINDOW, "-c", &home, &home_cmd])
        .exec();
    Err(err).context("exec tmux new-session")
}

/// Focus `window_id` for the session, then attach. Used when the overlay was
/// run from a plain terminal and the user jumped: the overlay tears down, and
/// this lands the new client on the chosen task.
pub fn attach_at(window_id: Option<&str>) -> Result<()> {
    use std::os::unix::process::CommandExt;
    if let Some(id) = window_id {
        let _ = select_window(id);
    }
    let err = cmd().args(["attach-session", "-t", SESSION]).exec();
    Err(err).context("exec tmux attach-session")
}

/// A message for the one situation tmux can't do in place: `tenx` run from a
/// client of a *different* tmux server. `switch-client` only works within one
/// server, and nesting a tmux inside a tmux is a trap, so say what to do.
pub fn foreign_client_hint() -> String {
    format!(
        "you're inside another tmux session — detach from it (prefix d) and run `tenx` again, \
         or attach from a second terminal with: tmux -L {} attach",
        socket()
    )
}

// ── Windows ───────────────────────────────────────────────────────────────────

const WINDOW_FORMAT: &str =
    "#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}\t#{window_bell_flag}\t#{window_activity_flag}";

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

/// The task window named exactly `name` (windows are named by task slug).
/// The home window is never a task window, whatever a task is called — a
/// task slugged `home` must not be able to select, kill or sweep the overlay.
pub fn find_window(name: &str) -> Result<Option<Window>> {
    if name == HOME_WINDOW {
        return Ok(None);
    }
    Ok(list_windows()?.into_iter().find(|w| w.name == name))
}

/// Slugs that can't be task names because they collide with tmux windows
/// tenx owns itself.
pub fn is_reserved_slug(slug: &str) -> bool {
    slug == HOME_WINDOW
}

pub fn select_window(id: &str) -> Result<()> {
    run(&["select-window", "-t", id]).map(drop)
}

/// The visible contents of a pane, with its colours (`-e` keeps the SGR
/// sequences) — what the overlay's preview panel shows for the selected task.
pub fn capture_pane(target: &str) -> Result<String> {
    run(&["capture-pane", "-p", "-e", "-t", target])
}

/// Type one tmux key name (`Enter`, `Escape`) into a pane — how the overlay
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

/// Open a small pane at the bottom of `window_id` following a background
/// agent's transcript (`tenx internal agent-log`). `-d` keeps focus where it
/// is: the agent appearing is news, not an interruption. Sized in lines, not
/// a share, so it costs the same on a tall window as a short one.
pub fn open_agent_pane(window_id: &str, tenx_bin: &str, cwd: &str, pid: u32, session_id: Option<&str>) -> Result<()> {
    let session = session_id.map(|s| format!(" --session {}", shell_quote(s))).unwrap_or_default();
    let command = format!("{} internal agent-log {} {pid}{session}", tenx_cmd(tenx_bin), shell_quote(cwd));
    run(&["split-window", "-d", "-v", "-l", "12", "-t", window_id, "-c", cwd, &command]).map(drop)
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
    /// Resume the task's most recent claude conversation (`--continue`). Only
    /// safe when one exists: without it, `claude --continue` exits 1 and the
    /// pane closes at once.
    pub resume: bool,
}

/// Create a task's window and its panes, returning the window's stable id.
/// The window becomes the session's current one, so a client that's attached
/// (or about to attach) lands on it.
///
/// Built-in layout — claude on the left, nvim on `TASK.md` top-right, a shell
/// bottom-right — mirrors the zellij default. A pane whose command exits
/// closes (tmux's default), which is what `close_on_exit` did.
pub fn open_task_window(opts: &TaskWindow) -> Result<String> {
    // `tenx:` (trailing colon) targets the session so the new window is
    // appended to it rather than treated as a window name to match.
    let session = format!("{SESSION}:");
    let claude = format!(
        "claude --name {}{}",
        shell_quote(opts.slug),
        if opts.resume { " --continue" } else { "" }
    );
    let first_cmd = if opts.layout_script.is_some() { None } else { Some(claude.as_str()) };
    let mut args = vec!["new-window", "-t", &session, "-n", opts.slug, "-c", opts.task_dir, "-P", "-F", "#{window_id}"];
    if let Some(c) = first_cmd {
        args.push(c);
    }
    let id = run(&args)?.trim().to_string();
    if id.is_empty() {
        bail!("tmux new-window returned no window id");
    }

    if let Some(script) = opts.layout_script {
        let status = Command::new(script)
            .env("TENX_WINDOW", &id)
            .env("TENX_SLUG", opts.slug)
            .env("TENX_TITLE", opts.title)
            .env("TENX_TASK_DIR", opts.task_dir)
            .env("TENX_WS_DIR", opts.workspace_dir)
            .env("TENX_CLAUDE_CMD", &claude)
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

    #[test]
    fn home_is_never_a_task_window() {
        let windows = vec![
            Window { id: "@0".into(), name: HOME_WINDOW.into(), active: true, bell: true, activity: true },
            Window { id: "@1".into(), name: "foo".into(), active: false, bell: true, activity: false },
        ];
        let s = signals_from(&windows);
        assert!(!s.contains_key(HOME_WINDOW));
        assert!(s["foo"].bell);
        assert!(is_reserved_slug("home") && !is_reserved_slug("homer"));
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
        let conf = render_config("/usr/local/bin/tenx");
        assert!(conf.contains(&format!("set -g @tenx_version \"{}\"", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn config_embeds_binary_and_palette() {
        let c = render_config("/usr/local/bin/tenx");
        assert!(c.contains("display-popup -E -w 85% -h 85% -T \" tenx \" "));
        assert!(c.contains("display-popup -E -B -w 100% -h 100% "));
        assert!(c.contains("#{||:#{<:#{client_width},100},#{<:#{client_height},30}}"));
        // The popup border must sit on the ground, not the terminal's default bg.
        assert!(c.contains(&format!("popup-border-style \"fg={},bg={}\"", palette::ACCENT.hex(), palette::GROUND.hex())));
        assert!(c.contains("'/usr/local/bin/tenx' overlay"));
        assert!(c.contains(&palette::ACCENT.hex()));
        assert!(c.contains("set -g monitor-bell on"));
        assert!(c.contains("#{?#{@tenx_status},#{E:@tenx_status},"));
        assert!(c.contains(",*:dim@"));
        assert!(c.contains("set -g window-style \"fg="));
    }
}
