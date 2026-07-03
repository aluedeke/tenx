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

/// Derive the zellij session name from a workspace name.
pub fn session_name(workspace_name: &str) -> String {
    format!(
        "tenx:{}",
        workspace_name
            .to_lowercase()
            .replace(['/', ' ', '_'], "-")
    )
}

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

/// Attach to an existing session, replacing the current process.
/// This is blocking — zellij takes over the terminal.
pub fn attach_session(name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = cmd().args(["attach", name]).exec();
    // exec() only returns on error
    Err(err).context(format!("exec zellij attach {name}"))
}

/// Create a new named session with the TUI as the first (and only) tab, then attach.
///
/// Uses `--new-session-with-layout` which always creates a fresh session from a
/// layout file — unlike `--layout-string`/`--layout` which *append* tabs to an
/// existing session and still produce a default shell tab on new session creation.
pub fn create_and_attach_session(session: &str, tenx_bin: &str, workspace_dir: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;

    // Write the layout to the zellij layouts dir so --new-session-with-layout can find it.
    let home = env::var("HOME").context("HOME not set")?;
    let layouts_dir = PathBuf::from(&home).join(".config/zellij/layouts");
    fs::create_dir_all(&layouts_dir).context("create zellij layouts dir")?;
    let layout_name = session.replace(':', "-");
    let layout_path = layouts_dir.join(format!("{layout_name}.kdl"));
    fs::write(&layout_path, full_session_layout(tenx_bin, workspace_dir))
        .context("write session layout file")?;

    let err = cmd()
        .args(["--session", session, "--new-session-with-layout", &layout_name])
        .exec();
    Err(err).context(format!("exec zellij --session {session}"))
}

fn full_session_layout(tenx_bin: &str, workspace_dir: &str) -> String {
    // tab-bar and status-bar live in a default_tab_template so that manually
    // created tabs (e.g. zellij's Ctrl+t n) inherit the same chrome. Without a
    // template, zellij registers an empty new_tab_template and a manual new tab
    // opens as a bare full-screen pane with no tab/status bar.
    format!(
        r#"layout {{
    default_tab_template {{
        pane size=1 borderless=true {{
            plugin location="zellij:tab-bar"
        }}
        children
        pane size=2 borderless=true {{
            plugin location="zellij:status-bar"
        }}
    }}
    tab name="tenx" focus=true {{
        pane split_direction="vertical" {{
            pane command="{tenx_bin}" cwd="{workspace_dir}" size="65%" {{
                args "tasks"
            }}
            pane command="{tenx_bin}" cwd="{workspace_dir}" size="35%" {{
                args "repos"
            }}
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

pub fn tab_exists(name: &str) -> Result<bool> {
    Ok(list_tabs()?.iter().any(|t| t.name == name))
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

pub fn go_to_tab(name: &str) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    let status = cmd()
        .args(["action", "go-to-tab-name", name])
        .status()
        .context("run zellij action go-to-tab-name")?;
    if !status.success() {
        bail!("go-to-tab-name failed for tab '{name}'");
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

/// Close a tab by its stable tab id, regardless of the tab's current name.
/// Robust against notification renames (e.g. a leading 💬 indicator), which
/// change the tab name but not its id.
pub fn close_tab_by_id(id: u32) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    let tab = find_tab_by_id(id)?.with_context(|| format!("no tab with id {id}"))?;
    go_to_tab_position(tab.position)?;
    let status = cmd()
        .args(["action", "close-tab"])
        .status()
        .context("run zellij action close-tab")?;
    if !status.success() {
        bail!("close-tab failed for tab id {id}");
    }
    Ok(())
}

pub fn close_tab(name: &str) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    // Match by name ignoring a leading notification indicator, since the Stop
    // hook renames active tabs to "💬 <title>". Close by position so we don't
    // depend on go-to-tab-name matching the (possibly prefixed) live name.
    let bare = name.trim_start_matches('\u{1F4AC}').trim();
    let tab = list_tabs()?
        .into_iter()
        .find(|t| t.name.trim_start_matches('\u{1F4AC}').trim() == bare)
        .with_context(|| format!("no tab matching '{name}'"))?;
    go_to_tab_position(tab.position)?;
    let status = cmd()
        .args(["action", "close-tab"])
        .status()
        .context("run zellij action close-tab")?;
    if !status.success() {
        bail!("close-tab failed for tab '{name}'");
    }
    Ok(())
}

/// Open or switch to the tenx TUI tab within the current session.
pub fn open_tui_tab(tenx_bin: &str, workspace_dir: &str) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    const TAB: &str = "tenx";
    if tab_exists(TAB)? {
        return go_to_tab(TAB);
    }
    let layout = format!(
        r#"layout {{
    tab name="{TAB}" cwd="{workspace_dir}" focus=true {{
        pane command="{tenx_bin}" borderless=true {{
            args "tui"
        }}
    }}
}}"#
    );
    let status = cmd()
        .args(["action", "new-tab", "--name", TAB, "--cwd", workspace_dir, "--layout-string", &layout])
        .status()
        .context("open tenx TUI tab")?;
    if !status.success() {
        bail!("failed to open tenx tab");
    }
    Ok(())
}

pub struct TabOptions<'a> {
    pub name: &'a str,
    pub cwd: &'a str,
    pub workspace_dir: &'a str,
    pub layout_file: Option<&'a str>,
}

pub fn render_layout(opts: &TabOptions) -> Result<String> {
    let tmpl = if let Some(layout_file) = opts.layout_file {
        fs::read_to_string(layout_file)
            .with_context(|| format!("read layout file {layout_file}"))?
    } else {
        default_task_layout(opts.workspace_dir)
    };
    Ok(tmpl.replace("{name}", opts.name).replace("{cwd}", opts.cwd))
}

fn default_task_layout(_workspace_dir: &str) -> String {
    r#"layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location="zellij:tab-bar"
        }
        children
        pane size=2 borderless=true {
            plugin location="zellij:status-bar"
        }
    }
    tab name="{name}" cwd="{cwd}" focus=true {
        pane split_direction="vertical" {
            pane name="claude" command="claude" cwd="{cwd}" size="50%" close_on_exit=true
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
