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
pub fn rename_tab_in(session: &str, tab_id: u32, name: &str) -> Result<()> {
    let status = cmd()
        .args(["-s", session, "action", "rename-tab", "--tab-id", &tab_id.to_string(), name])
        .status()
        .context("run zellij -s <session> rename-tab")?;
    if !status.success() {
        bail!("rename-tab failed for tab {tab_id} in '{session}'");
    }
    Ok(())
}

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
    let err = cmd()
        .args(["--session", SESSION, "--new-session-with-layout", &layout_name])
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
    if !session_exists(SESSION)? {
        let layout_name = write_home_layout(tenx_bin)?;
        args.push("--layout".to_string());
        args.push(layout_name);
    }
    let status = cmd().args(&args).status().context("run zellij action switch-session")?;
    if !status.success() {
        bail!("switch-session to '{SESSION}' failed");
    }
    Ok(())
}

/// The single session's home tab: the overlay running full-screen as the base
/// pane. No tab-bar anywhere — tasks live in invisible tabs and the overlay is
/// the only switcher. The status-bar stays for zellij keybinding hints; the
/// default_tab_template ensures manually created tabs (Ctrl+t n) inherit it.
fn home_layout(tenx_bin: &str) -> String {
    format!(
        r#"layout {{
    default_tab_template {{
        children
        pane size=2 borderless=true {{
            plugin location="zellij:status-bar"
        }}
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

pub fn find_tab_by_id(id: u32) -> Result<Option<Tab>> {
    Ok(list_tabs()?.into_iter().find(|t| t.tab_id == id))
}

pub fn rename_tab_by_id(id: u32, name: &str) -> Result<()> {
    let status = cmd()
        .args(["action", "rename-tab", "--tab-id", &id.to_string(), name])
        .status()
        .context("run zellij action rename-tab")?;
    if !status.success() {
        bail!("rename-tab failed for tab id {id}");
    }
    Ok(())
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
    // {claude_args} first: it embeds {name}, which the next replace fills in.
    Ok(tmpl
        .replace("{claude_args}", claude_args)
        .replace("{name}", opts.name)
        .replace("{cwd}", opts.cwd))
}

fn default_task_layout(_workspace_dir: &str) -> String {
    r#"layout {
    default_tab_template {
        children
        pane size=2 borderless=true {
            plugin location="zellij:status-bar"
        }
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
