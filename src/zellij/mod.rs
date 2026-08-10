use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Find the zellij binary by searching common install locations.
/// Falls back to bare "zellij" (relies on PATH) if nothing is found.
pub fn find_bin() -> Option<PathBuf> {
    let home = env::var("HOME").unwrap_or_default();
    for dir in [
        format!("{home}/.cargo/bin"),
        format!("{home}/.local/bin"),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
    ] {
        let p = PathBuf::from(dir).join("zellij");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn cmd() -> Command {
    Command::new(find_bin().unwrap_or_else(|| PathBuf::from("zellij")))
}

// ── Session identity ──────────────────────────────────────────────────────────

/// The single global tenx session. All workspaces' tasks live here as
/// (invisible) tabs; the overlay is the only task list/switcher.
pub const SESSION: &str = "tenx";

/// Returns the name of the current zellij session, if inside one.
pub fn current_session() -> Option<String> {
    env::var("ZELLIJ_SESSION_NAME").ok()
}

pub fn is_inside_session() -> bool {
    env::var("ZELLIJ").is_ok()
}

// ── Pushing state to plugins ──────────────────────────────────────────────────

/// The pipe the status bar listens on. Namespaced because a pipe sent without
/// `--plugin` is broadcast to *every* listening plugin in the session, so the
/// name is the only thing distinguishing our traffic from anyone else's.
pub const STATUS_PIPE: &str = "tenx::status";

/// Broadcast a status payload to every plugin in the tenx session listening on
/// [`STATUS_PIPE`]. Best-effort throughout: a status bar that misses an update
/// gets the next one, and nothing in tenx should fail because a pipe didn't land.
///
/// Broadcast rather than `--plugin <url>`-addressed, which looks more precise
/// and is worse in two ways: it *launches* a fresh instance (with a pane) when
/// the URL isn't already running, and it matches on url+configuration, so the
/// per-tab config that tells each bar which task it belongs to would make every
/// one of them a different target.
///
/// The payload goes in on stdin, not as the positional `PAYLOAD` argument:
/// `zellij pipe <payload>` holds the pipe open and never returns, which in a
/// long-lived caller is a process that accumulates forever. Closing stdin is
/// what tells the CLI to deliver and exit (~20 ms).
pub fn pipe_status(payload: &str) {
    use std::io::Write as _;
    use std::process::Stdio;

    let Ok(mut child) = cmd()
        .args(["-s", SESSION, "pipe", "--name", STATUS_PIPE])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    // Reaped, not detached: this runs on a loop for the life of the session, and
    // unwaited children would pile up as zombies. Safe to block on — a pipe to a
    // session that's gone exits 1 immediately rather than hanging or creating it.
    let _ = child.wait();
}

// ── Session management ────────────────────────────────────────────────────────

/// Parse session names from `zellij list-sessions` output.
/// Output is one session per line; active session may be marked with a suffix.
pub fn list_sessions() -> Result<Vec<String>> {
    let out = cmd()
        .args(["list-sessions"])
        .output()
        .context("run zellij list-sessions")?;
    // zellij writes session info to stdout; strip ANSI codes before parsing
    // since some versions emit colour even when output is piped.
    let text = String::from_utf8_lossy(&out.stdout);
    let sessions = text
        .lines()
        .filter_map(|l| {
            let clean = strip_ansi(l);
            // Each line: "session-name [Created N ago] (current)"
            // Take the first whitespace-delimited token = session name.
            clean
                .split_whitespace()
                .next()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .collect();
    Ok(sessions)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            // consume until a letter (the SGR terminator)
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn session_exists(name: &str) -> Result<bool> {
    Ok(list_sessions()?.iter().any(|s| s == name))
}

/// List the tabs of another (possibly detached) session by name. Read-only and
/// works cross-session.
pub fn list_tabs_in(session: &str) -> Result<Vec<Tab>> {
    let out = cmd()
        .args(["-s", session, "action", "list-tabs", "--json"])
        .output()
        .context("run zellij -s <session> list-tabs")?;
    let tabs: Vec<Tab> = serde_json::from_slice(&out.stdout).context("parse list-tabs json")?;
    Ok(tabs)
}

/// Jump to a task that lives in *another* workspace's session: focus its tab in
/// that session (by stable id, falling back to title), then switch the current
/// client to that session in place — landing on the exact tab. If the tab can't
/// be resolved (stale id / not open there), degrades to a plain session switch
/// that lands on the session's last-focused tab.
pub fn switch_to_task(session: &str, tab_id: Option<u32>, tab_title: &str) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }

    // Resolve the tab in the target session, then pre-focus it there.
    if let Some(id) = resolve_tab_in(session, tab_id, tab_title) {
        let _ = cmd()
            .args(["-s", session, "action", "go-to-tab-by-id", &id.to_string()])
            .status();
    }

    // `switch-session` takes the target session as a positional arg, not --name.
    let status = cmd()
        .args(["action", "switch-session", session])
        .status()
        .context("run zellij action switch-session")?;
    if !status.success() {
        bail!("switch-session failed (zellij may be too old) for '{session}'");
    }
    Ok(())
}

/// Close a tab by id in a specific session (works cross-session).
pub fn close_tab_in(session: &str, tab_id: u32) -> Result<()> {
    let status = cmd()
        .args(["-s", session, "action", "close-tab-by-id", &tab_id.to_string()])
        .status()
        .context("run zellij -s <session> close-tab-by-id")?;
    if !status.success() {
        bail!("close-tab-by-id failed for tab {tab_id} in '{session}'");
    }
    Ok(())
}

/// Rename a tab by id in a specific session (works cross-session).
/// Best-effort: pre-focus a task's tab in `session` so a subsequent `attach`
/// lands on it. Used by the overlay when jumping from *outside* zellij, where we
/// can't `switch-session` in place and must attach instead. Silently no-ops if
/// the tab can't be resolved (stale id / not open) — attach still succeeds.
pub fn pre_focus_tab(session: &str, tab_id: Option<u32>, tab_title: &str) {
    if let Some(id) = resolve_tab_in(session, tab_id, tab_title) {
        let _ = cmd()
            .args(["-s", session, "action", "go-to-tab-by-id", &id.to_string()])
            .status();
    }
}

/// Find a task's tab id in `session`: prefer the stored id if it's still
/// present, otherwise match by title (tab names are stable — set at creation,
/// changed only by an explicit rename).
fn resolve_tab_in(session: &str, tab_id: Option<u32>, title: &str) -> Option<u32> {
    let tabs = list_tabs_in(session).ok()?;
    if let Some(id) = tab_id
        && tabs.iter().any(|t| t.tab_id == id)
    {
        return Some(id);
    }
    tabs.iter().find(|t| t.name == title).map(|t| t.tab_id)
}

/// Attach to an existing session, replacing the current process.
/// This is blocking — zellij takes over the terminal.
pub fn attach_session(name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = cmd().args(["attach", name]).exec();
    // exec() only returns on error
    Err(err).context(format!("exec zellij attach {name}"))
}

/// The tenx zellij theme + frame styling, appended to the user's base config
/// when launching a tenx session (see `write_session_config`). Built inline
/// (rather than a `themes/tenx.kdl` file) so the generated config is fully
/// self-contained and independent of zellij's theme-dir resolution.
///
/// Colours come from [`crate::palette`] — the *same* source the overlay TUI
/// uses — so zellij's chrome and the overlay read as one design.
///
/// Uses zellij's **semantic component** theme format (not the legacy 16-colour
/// palette) so pane-border colours are set *explicitly*: the focused pane's
/// frame is the purple accent, unfocused frames are muted grey. The palette
/// format leaves zellij to derive frame colours on its own, which lands on a
/// near-default grey — so borders looked unthemed.
fn theme_overlay() -> String {
    use crate::palette::*;
    // One component block: `base` (fg) + `background` + four emphasis accents.
    let comp = |name: &str, base: &Rgb, bg: &Rgb| {
        format!(
            "    {name} {{\n        base {base}\n        background {bg}\n        \
             emphasis_0 {e0}\n        emphasis_1 {e1}\n        emphasis_2 {e2}\n        \
             emphasis_3 {e3}\n    }}\n",
            name = name,
            base = base.rgb(),
            bg = bg.rgb(),
            e0 = ACCENT.rgb(),
            e1 = WARN.rgb(),
            e2 = INFO.rgb(),
            e3 = SUCCESS.rgb(),
        )
    };
    let mut t = String::from(
        "\n// ─── tenx theme (appended by tenx; applies to tenx sessions only) ───\n\
         // Semantic components mirror the overlay palette (crate::palette).\n\
         themes {\n    tenx {\n",
    );
    t.push_str(&comp("text_unselected", &TEXT, &GROUND));
    t.push_str(&comp("text_selected", &BRIGHT, &GROUND));
    t.push_str(&comp("ribbon_unselected", &MUTED, &GROUND));
    t.push_str(&comp("ribbon_selected", &GROUND, &ACCENT));
    t.push_str(&comp("frame_unselected", &MUTED, &GROUND));
    t.push_str(&comp("frame_selected", &ACCENT, &GROUND)); // focused pane border → purple
    t.push_str(&comp("frame_highlight", &WARN, &GROUND));
    t.push_str(&comp("list_unselected", &TEXT, &GROUND));
    t.push_str(&comp("list_selected", &BRIGHT, &ACCENT));
    t.push_str(&comp("table_title", &ACCENT, &GROUND));
    t.push_str(&comp("table_cell_unselected", &TEXT, &GROUND));
    t.push_str(&comp("table_cell_selected", &BRIGHT, &ACCENT));
    // exit_code_* also require the full field set in zellij 0.44 (base alone
    // fails to parse with "Missing theme color: emphasis_0").
    t.push_str(&comp("exit_code_success", &SUCCESS, &GROUND));
    t.push_str(&comp("exit_code_error", &DANGER, &GROUND));
    t.push_str("    }\n}\n");
    t.push_str("theme \"tenx\"\nui {\n    pane_frames {\n        rounded_corners true\n    }\n}\n");
    t
}

/// Generate a tenx-scoped zellij config — the user's base `config.kdl` plus the
/// tenx theme overlay — and return its path. Regenerated on every session
/// creation so it never drifts from the user's base config (which carries the
/// tenx overlay keybind, plugin alias, and the user's own keybinds — all of
/// which the tenx session must keep). Launching zellij against this file themes
/// tenx sessions *only*, without touching the global config other sessions use.
///
/// Any active `theme "..."` line in the base config is dropped so the appended
/// `theme "tenx"` is unambiguous.
fn write_session_config() -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME not set")?;
    let cfg_dir = PathBuf::from(&home).join(".config/zellij");
    fs::create_dir_all(&cfg_dir).context("create zellij config dir")?;
    let base = fs::read_to_string(cfg_dir.join("config.kdl")).unwrap_or_default();
    // Strip any active (non-comment) top-level `theme "..."` selection so ours wins.
    let base: String = base
        .lines()
        .filter(|l| !l.trim_start().starts_with("theme "))
        .collect::<Vec<_>>()
        .join("\n");
    let generated = cfg_dir.join("tenx-session.kdl");
    fs::write(&generated, format!("{base}\n{}", theme_overlay()))
        .context("write tenx session config")?;
    Ok(generated)
}

/// Write the home layout to the zellij layouts dir and return its layout name
/// (usable with `--new-session-with-layout` / `switch-session --layout`).
fn write_home_layout(tenx_bin: &str) -> Result<String> {
    let home = env::var("HOME").context("HOME not set")?;
    let layouts_dir = PathBuf::from(&home).join(".config/zellij/layouts");
    fs::create_dir_all(&layouts_dir).context("create zellij layouts dir")?;
    let layout_path = layouts_dir.join(format!("{SESSION}.kdl"));
    fs::write(&layout_path, home_layout(tenx_bin)).context("write session layout file")?;
    Ok(SESSION.to_string())
}

/// Create the global tenx session with the home overlay as its only tab, then
/// attach.
///
/// Uses `--new-session-with-layout` which always creates a fresh session from a
/// layout file — unlike `--layout-string`/`--layout` which *append* tabs to an
/// existing session and still produce a default shell tab on new session creation.
pub fn create_and_attach_session(tenx_bin: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let layout_name = write_home_layout(tenx_bin)?;
    // Theme this session only: launch against a generated config (user base +
    // tenx theme overlay) via the global `--config` flag.
    let config = write_session_config()?;
    let config = config.to_string_lossy();
    let mut command = cmd();
    // Start the server from $HOME, not from wherever tenx happened to be run.
    //
    // A zellij server outlives the directory it was started in, and every
    // plugin `run_command` is issued with a cwd of "." (hardcoded in
    // zellij-tile), so once that directory is deleted *every* plugin command
    // fails with ENOENT — not because the binary is missing, but because the
    // working directory is. The overlay polls every 1.5s, so it becomes a
    // permanent error loop and an overlay that can never list a task again.
    //
    // tenx is habitually run from inside a task, and tasks get deleted. Seen in
    // the wild: a session whose cwd was `tasks/flickering/tenx`, still spinning
    // days after that task was removed, ~25 MB/day of identical log lines.
    // Confirmed by experiment — a session started in a scratch directory logged
    // nothing until the directory was deleted underneath it, then ~0.75 errors
    // a second, one per poll.
    if let Ok(home) = env::var("HOME") {
        command.current_dir(home);
    }
    let err = command
        .args(["--config", &config, "--session", SESSION, "--new-session-with-layout", &layout_name])
        .exec();
    Err(err).context(format!("exec zellij --session {SESSION}"))
}

/// Switch the current client to the tenx session in place (from inside a
/// *different* zellij session), creating it from the home layout if it doesn't
/// exist yet.
pub fn switch_to_tenx_session(tenx_bin: &str) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    let mut args = vec!["action".to_string(), "switch-session".to_string(), SESSION.to_string()];
    // Only pass --layout when the session doesn't exist yet: switch-session
    // applies the layout on creation; for an existing session it must not
    // append tabs.
    let mut command = cmd();
    if !session_exists(SESSION)? {
        let layout_name = write_home_layout(tenx_bin)?;
        args.push("--layout".to_string());
        args.push(layout_name);
        // Best-effort theming for the switch-session creation path: `action`
        // takes no --config flag, so point the (creating) server at the tenx
        // config via env. The guaranteed path is `create_and_attach_session`.
        command.env("ZELLIJ_CONFIG_FILE", write_session_config()?);
    }
    let status = command.args(&args).status().context("run zellij action switch-session")?;
    if !status.success() {
        bail!("switch-session to '{SESSION}' failed");
    }
    Ok(())
}

/// Absolute path to the status-bar wasm — the newest content-addressed copy
/// `make install` wrote, falling back to the unversioned name.
///
/// Layouts must name it by path, not by a `plugins` alias: an alias is resolved
/// against the *user's* config, and a task tab's bar carries per-task
/// configuration that an alias can't supply.
///
/// **The filename carries a content hash on purpose.** The zellij *server*
/// caches each plugin's compiled module keyed by path for the life of the
/// session, so overwriting the wasm in place changes nothing: a task tab opened
/// afterwards is handed the module compiled the first time the server read that
/// path, and keeps running last week's code until the session is restarted.
/// Measured — a tab created after an in-place overwrite rendered the *old*
/// build. `zellij action start-or-reload-plugin` does evict the cache, but it
/// also spawns a half-screen plugin pane that the bar (which handles no keys)
/// can't dismiss, which is a poor thing to leave behind on every install.
///
/// A new build lands at a new path, so it gets a new cache entry for free. No
/// eviction, no stray pane, and it works in a session that has been up for days.
pub fn statusbar_wasm() -> String {
    let home = env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(&home).join(".local/share/tenx");

    // The pointer `make install` writes, naming the file it just installed.
    //
    // This used to be "newest by mtime", which is a guess, and the guess was
    // wrong the moment anything else touched the directory — restoring an older
    // build's path (which `make install` deliberately does, so panes in a
    // running session stay loadable) gave that copy the freshest mtime and it
    // won resolution over the build actually being installed. An install knows
    // exactly which file it wrote; asking it beats inferring.
    if let Ok(name) = fs::read_to_string(dir.join(CURRENT_STATUSBAR)) {
        let candidate = dir.join(name.trim());
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    // No pointer (an install predating it, or a hand-managed directory): fall
    // back to newest-by-mtime, then to the unversioned name.
    let newest = fs::read_dir(&dir).ok().and_then(|entries| {
        entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("tenx-statusbar-") && name.ends_with(".wasm")
            })
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            .map(|e| e.path())
    });
    newest
        .unwrap_or_else(|| dir.join("tenx-statusbar.wasm"))
        .to_string_lossy()
        .into_owned()
}

/// File naming the status-bar build to load, written by `make install`.
pub const CURRENT_STATUSBAR: &str = "tenx-statusbar.current";

/// A status-bar pane block for a layout, optionally bound to a task.
///
/// One row, not zellij's two: the bar it replaces is a single line, and the row
/// this frees is the one the old per-tab header pane used to occupy.
fn statusbar_pane(task: Option<(&str, &str, &str)>) -> String {
    let wasm = statusbar_wasm();
    let cfg = match task {
        // NB: `task_title`, not `title` — zellij consumes `title` as the pane
        // title and it never reaches the plugin's `load()` configuration.
        Some((slug, title, ws_dir)) => format!(
            "\n            task \"{slug}\"\n            task_title \"{title}\"\n            ws_dir \"{ws_dir}\"\n        "
        ),
        None => String::new(),
    };
    format!(
        "pane size=1 borderless=true {{\n        plugin location=\"file:{wasm}\" {{{cfg}}}\n    }}"
    )
}

/// The single session's home tab: the overlay running full-screen as the base
/// pane. No tab-bar anywhere — tasks live in invisible tabs and the overlay is
/// the only switcher. The home tab's bar has no task of its own, so it renders
/// the attention list alone; the default_tab_template ensures manually created
/// tabs (Ctrl+t n) inherit one too.
fn home_layout(tenx_bin: &str) -> String {
    let statusbar = statusbar_pane(None);
    format!(
        r#"layout {{
    default_tab_template {{
        children
        {statusbar}
    }}
    tab name="home" focus=true {{
        pane command="{tenx_bin}" {{
            args "overlay" "--home"
        }}
    }}
}}"#
    )
}

// ── Tab management (within the current session) ───────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Tab {
    pub tab_id: u32,
    pub position: usize,
    pub name: String,
    pub active: bool,
}

pub fn list_tabs() -> Result<Vec<Tab>> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    let out = cmd()
        .args(["action", "list-tabs", "--json"])
        .output()
        .context("run zellij action list-tabs")?;
    let tabs: Vec<Tab> = serde_json::from_slice(&out.stdout).context("parse list-tabs json")?;
    Ok(tabs)
}

/// Find a live tab by exact name — the reliable task↔tab correlation. (Names
/// are kept synced to unique task titles; stored numeric ids collide across
/// sessions, so we never look tabs up by id.)
pub fn find_tab_by_name(name: &str) -> Result<Option<Tab>> {
    Ok(list_tabs()?.into_iter().find(|t| t.name == name))
}


pub fn go_to_tab_position(position: usize) -> Result<()> {
    // zellij go-to-tab takes a 1-based index
    let status = cmd()
        .args(["action", "go-to-tab", &(position + 1).to_string()])
        .status()
        .context("run zellij action go-to-tab")?;
    if !status.success() {
        bail!("go-to-tab failed for position {position}");
    }
    Ok(())
}

pub struct TabOptions<'a> {
    pub name: &'a str,
    pub cwd: &'a str,
    pub workspace_dir: &'a str,
    /// The task's display title (TASK.md's heading). Only the status bar uses
    /// it — the tab itself is named by slug so the correlation can't drift.
    pub title: &'a str,
    pub layout_file: Option<&'a str>,
    /// Resume the task's most recent claude conversation (`--continue`). Only
    /// safe when one exists: with no prior conversation, interactive
    /// `claude --continue` exits 1 ("No conversation found to continue"), which
    /// silently closes the `close_on_exit` pane. Callers must set this false for
    /// a task's first open. Ignored when `layout_file` is set (custom layouts
    /// spell out their own claude command).
    pub resume: bool,
}

pub fn render_layout(opts: &TabOptions) -> Result<String> {
    let tmpl = if let Some(layout_file) = opts.layout_file {
        fs::read_to_string(layout_file)
            .with_context(|| format!("read layout file {layout_file}"))?
    } else {
        default_task_layout(opts.workspace_dir)
    };
    let claude_args = if opts.resume {
        r#"args "--name" "{name}" "--continue""#
    } else {
        r#"args "--name" "{name}""#
    };
    // {tenx}: absolute path to this binary, for layout panes that run tenx
    // itself — PATH is unreliable in zellij-spawned commands.
    let tenx = std::env::current_exe().context("cannot determine tenx binary path")?;
    // `workspace_dir`, NOT `cwd`: the bar identifies its own task by the same
    // key the payload uses, `<workspace dir>/<slug>` (`workspace::task_json`).
    // Passing the task directory built `<workspace>/tasks/<slug>/<slug>`, which
    // matches nothing, so the left side sat on "idle" forever.
    let statusbar = statusbar_pane(Some((opts.name, opts.title, opts.workspace_dir)));
    // {claude_args} first: it embeds {name}, which the next replace fills in.
    let rendered = tmpl
        .replace("{claude_args}", claude_args)
        .replace("{statusbar}", &statusbar)
        .replace("{name}", opts.name)
        .replace("{cwd}", opts.cwd)
        .replace("{tenx}", &tenx.to_string_lossy());
    // `{statusbar}` above is the supported hook for layout authors. This is the
    // fallback for the ones that never got edited: a workspace layout file
    // naming zellij's own bar gains the tenx one without anyone touching it,
    // which matters because those files live outside this repo. It keeps
    // whatever `size=` the layout declared — a 2-row pane just renders the bar
    // in its first row — since rewriting the surrounding pane block by string
    // match would break on any formatting the author chose differently.
    Ok(rendered.replace(
        "plugin location=\"zellij:status-bar\"",
        &format!("plugin location=\"file:{}\"", statusbar_wasm()),
    ))
}

fn default_task_layout(_workspace_dir: &str) -> String {
    // No header pane: the task's name and live status are the left third of the
    // status bar now, which is the same information in one fewer row and one
    // fewer process per tab.
    r#"layout {
    default_tab_template {
        children
        {statusbar}
    }
    tab name="{name}" cwd="{cwd}" focus=true {
        pane split_direction="vertical" {
            pane name="claude" command="claude" cwd="{cwd}" size="50%" close_on_exit=true {
                // {claude_args}: "--name {name}" plus "--continue" only when a prior
                // conversation exists (see TabOptions::resume).
                {claude_args}
            }
            pane split_direction="horizontal" size="50%" {
                pane name="nvim" command="nvim" {
                    args "TASK.md"
                }
                pane name="shell"
            }
        }
    }
}"#.to_string()
}

/// Open a new task tab and return its stable tab_id.
pub fn open_or_switch(opts: &TabOptions) -> Result<u32> {
    if !is_inside_session() {
        bail!("not inside a zellij session — start zellij or use --no-open");
    }
    let kdl = render_layout(opts)?;
    let status = cmd()
        .args(["action", "new-tab", "--name", opts.name, "--cwd", opts.cwd, "--layout-string", &kdl])
        .status()
        .context("run zellij action new-tab")?;
    if !status.success() {
        bail!("zellij new-tab failed for '{}'", opts.name);
    }
    // The new tab becomes active — find it to get its stable tab_id.
    let tab = list_tabs()?
        .into_iter()
        .find(|t| t.active)
        .ok_or_else(|| anyhow::anyhow!("cannot find newly created tab"))?;
    Ok(tab.tab_id)
}
