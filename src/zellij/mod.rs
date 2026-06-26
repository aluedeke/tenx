use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

// ── Session identity ──────────────────────────────────────────────────────────

/// Derive the zellij session name from a workspace name.
/// Lowercases and replaces spaces/slashes with hyphens for a clean shell name.
pub fn session_name(workspace_name: &str) -> String {
    format!(
        "tenx-{}",
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
    let out = Command::new("zellij")
        .args(["list-sessions", "--short"])
        .output()
        .context("run zellij list-sessions")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let sessions = text
        .lines()
        .map(|l| l.split_whitespace().next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(sessions)
}

pub fn session_exists(name: &str) -> Result<bool> {
    Ok(list_sessions()?.iter().any(|s| s == name))
}

/// Attach to an existing session, replacing the current process.
/// This is blocking — zellij takes over the terminal.
pub fn attach_session(name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = Command::new("zellij").args(["attach", name]).exec();
    // exec() only returns on error
    Err(err).context(format!("exec zellij attach {name}"))
}

/// Create a new named session and attach to it, using a layout that opens
/// the tenx TUI as the first tab. The layout file is written to the tenx
/// cache dir and left there (cheap, idempotent).
pub fn create_and_attach_session(session: &str, tenx_bin: &str, workspace_dir: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let layout_path = session_layout_path(session)?;
    let layout = make_session_layout(tenx_bin, workspace_dir);
    if let Some(parent) = layout_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&layout_path, &layout)
        .with_context(|| format!("write session layout to {}", layout_path.display()))?;

    let err = Command::new("zellij")
        .args(["--session", session, "--layout", &layout_path.to_string_lossy()])
        .exec();
    Err(err).context(format!("exec zellij --session {session}"))
}

fn session_layout_path(session: &str) -> Result<PathBuf> {
    let home = env::var("HOME").context("$HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".cache")
        .join("tenx")
        .join(format!("{session}.kdl")))
}

fn make_session_layout(tenx_bin: &str, workspace_dir: &str) -> String {
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
    tab name="tenx" cwd="{workspace_dir}" focus=true {{
        pane command="{tenx_bin}" {{
            args "tui"
        }}
    }}
}}
"#
    )
}

// ── Tab management (within the current session) ───────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Tab {
    #[allow(dead_code)]
    pub position: usize,
    pub name: String,
    #[allow(dead_code)]
    pub active: bool,
}

pub fn list_tabs() -> Result<Vec<Tab>> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    let out = Command::new("zellij")
        .args(["action", "list-tabs", "--json"])
        .output()
        .context("run zellij action list-tabs")?;
    let tabs: Vec<Tab> = serde_json::from_slice(&out.stdout).context("parse list-tabs json")?;
    Ok(tabs)
}

pub fn tab_exists(name: &str) -> Result<bool> {
    Ok(list_tabs()?.iter().any(|t| t.name == name))
}

pub fn go_to_tab(name: &str) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    let status = Command::new("zellij")
        .args(["action", "go-to-tab-name", name])
        .status()
        .context("run zellij action go-to-tab-name")?;
    if !status.success() {
        bail!("go-to-tab-name failed for tab '{name}'");
    }
    Ok(())
}

pub fn close_tab(name: &str) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session");
    }
    go_to_tab(name)?;
    let status = Command::new("zellij")
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
        pane command="{tenx_bin}" {{
            args "tui"
        }}
    }}
}}"#
    );
    let status = Command::new("zellij")
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
    pub layout_file: Option<&'a str>,
}

const DEFAULT_TASK_LAYOUT: &str = r#"layout {
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
            pane name="claude" command="claude" size="50%" close_on_exit=true
            pane split_direction="horizontal" size="50%" {
                pane name="nvim" command="nvim"
                pane name="shell"
            }
        }
    }
}
"#;

pub fn render_layout(opts: &TabOptions) -> Result<String> {
    let tmpl = if let Some(layout_file) = opts.layout_file {
        fs::read_to_string(layout_file)
            .with_context(|| format!("read layout file {layout_file}"))?
    } else {
        DEFAULT_TASK_LAYOUT.to_string()
    };
    Ok(tmpl.replace("{name}", opts.name).replace("{cwd}", opts.cwd))
}

/// Open a task tab or switch to it if already open.
pub fn open_or_switch(opts: &TabOptions) -> Result<()> {
    if !is_inside_session() {
        bail!("not inside a zellij session — start zellij or use --no-open");
    }
    if tab_exists(opts.name)? {
        return go_to_tab(opts.name);
    }
    let kdl = render_layout(opts)?;
    let status = Command::new("zellij")
        .args(["action", "new-tab", "--name", opts.name, "--cwd", opts.cwd, "--layout-string", &kdl])
        .status()
        .context("run zellij action new-tab")?;
    if !status.success() {
        bail!("zellij new-tab failed for '{}'", opts.name);
    }
    Ok(())
}
