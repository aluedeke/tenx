//! A task window's tabs (`tenx_core::tabs`): one pane on screen over the whole
//! window, the others each in a window of [`TABS_SESSION`], a detached session
//! of the same server nobody attaches to. Showing a tab is `swap-pane`, so the
//! task keeps its one window and every lookup by window keeps working; the
//! listings that must see every pane of a task (its agent's pid, its ports)
//! read this session too (`super::task_panes`).
//!
//! Started tabs are lazy: the shell and nvim start the first time they are
//! shown. Each runs `tenx internal tab-ended` when its program ends, which
//! puts the agent back on screen, so typing `exit` in the shell doesn't close
//! the task's window. Waiting tabs whose window is gone (the agent ended) are
//! closed by the watcher (`close_orphans`). No tmux hooks: on Ubuntu's CI
//! runners tmux ran none.

use super::{run, SESSION};
use anyhow::{bail, Context, Result};
use tenx_core::tabs::{self, Step, Tab, CURRENT_OPTION, OWNER_OPTION};

/// The session waiting tabs live in.
pub const TABS_SESSION: &str = "tenx-tabs";

/// Make `window` a tabbed window with `agent_pane` on screen.
pub fn mark_tabbed(window: &str, agent_pane: &str) -> Result<()> {
    // The strip is the pane's top line.
    run(&[
        "set-option", "-w", "-t", window, CURRENT_OPTION, Tab::Agent.name(), ";",
        "set-option", "-w", "-t", window, Tab::Agent.pane_option(), agent_pane, ";",
        "set-option", "-w", "-t", window, "pane-border-status", "top",
    ])?;
    // On a ground of its own: no line drawn around it, and none between it
    // and a background agent's log. tmux 3.4 doesn't know `spaces`, and then
    // draws the line; a separate command, so the rest is set anyway.
    let _ = run(&["set-option", "-w", "-t", window, "pane-border-lines", "spaces"]);
    // A line between the strip and the tab under it: the strip's whole row
    // underlined, in the border's colour.
    let line = format!("fg={c},underscore,us={c}", c = crate::palette::BORDER_ACTIVE.hex());
    let _ = run(&[
        "set-option", "-w", "-t", window, "pane-border-style", &line, ";",
        "set-option", "-w", "-t", window, "pane-active-border-style", &line,
    ]);
    Ok(())
}

/// What a tabbed window knows about itself.
struct State {
    current: Tab,
    panes: Vec<(Tab, String)>,
    task_dir: String,
    name: String,
    size: (String, String),
}

impl State {
    fn pane(&self, tab: Tab) -> Option<&str> {
        self.panes.iter().find(|(t, _)| *t == tab).map(|(_, p)| p.as_str()).filter(|p| !p.is_empty())
    }
}

/// `None` for a window without tabs.
fn state(window: &str) -> Result<Option<State>> {
    let opts: Vec<String> = Tab::ALL.iter().map(|t| format!("#{{{}}}", t.pane_option())).collect();
    let format = format!(
        "#{{{CURRENT_OPTION}}}\t#{{{}}}\t#{{window_name}}\t#{{window_width}}\t#{{window_height}}\t{}",
        super::TASK_DIR_OPTION,
        opts.join("\t")
    );
    let out = run(&["display-message", "-p", "-t", window, &format])?;
    let f: Vec<&str> = out.trim_end_matches('\n').split('\t').collect();
    let [current, task_dir, name, w, h, rest @ ..] = &f[..] else { bail!("tmux printed no window state") };
    let Some(current) = Tab::parse(current) else { return Ok(None) };
    Ok(Some(State {
        current,
        panes: Tab::ALL.iter().copied().zip(rest.iter().map(|p| p.to_string())).collect(),
        task_dir: task_dir.to_string(),
        name: name.to_string(),
        size: (w.to_string(), h.to_string()),
    }))
}

/// Every live pane of the server, as (pane, dead).
fn live_panes() -> Vec<(String, bool)> {
    run(&["list-panes", "-a", "-F", "#{pane_id}\t#{pane_dead}"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(p, d)| (p.to_string(), d == "1")))
        .collect()
}

/// Show `tab` in task window `window`. A window without tabs is an error.
///
/// One switch at a time (`lock`), and the tab on screen is read from the
/// window's panes (`tabs::on_screen`): a phone's tap arrives as a tap and as
/// a click, and two switches interleaved swapped panes the other had already
/// moved.
pub fn switch_tab(window: &str, tab: Tab) -> Result<()> {
    let _lock = lock()?;
    let Some(mut st) = state(window)? else { bail!("this window has no tabs") };
    // Every option set redraws every client showing the window: set only
    // what changes.
    let said = st.current;
    let in_window: Vec<String> = run(&["list-panes", "-t", window, "-F", "#{pane_id}"])?.lines().map(str::to_string).collect();
    if let Some(t) = tabs::on_screen(&st.panes, &in_window) {
        st.current = t;
    }
    let live = live_panes();
    let target = st.pane(tab);
    let alive = target.is_some_and(|p| live.iter().any(|(id, dead)| id == p && !dead));
    let pane = match tabs::step(st.current, tab, target, alive) {
        Step::Stay => {
            // Say what is shown, in case a switch left the option behind.
            if said != tab {
                run(&["set-option", "-w", "-t", window, CURRENT_OPTION, tab.name()])?;
            }
            return Ok(());
        }
        Step::AgentGone => bail!("the agent has ended — close this task's window and open it again"),
        Step::Show(p) => p,
        Step::Start => start_tab(window, &st, tab)?,
    };
    // The pane on screen is the current tab's; if that's gone, the window's active one.
    let shown = match st.pane(st.current).filter(|p| in_window.iter().any(|id| id == p)) {
        Some(p) => p.to_string(),
        None => run(&["display-message", "-p", "-t", window, "#{pane_id}"])?.trim().to_string(),
    };
    // At the window's size before it's on screen, so the program redraws
    // once. Only ever a waiting tab's window: `resize-window` fixes a
    // window's size for good, and a task window must keep following its
    // client — a phone's keyboard shrinks it.
    let (w, h) = (&st.size.0, &st.size.1);
    let home = run(&["display-message", "-p", "-t", &pane, "#{window_id}"]).unwrap_or_default();
    if !home.trim().is_empty() && home.trim() != window {
        let _ = run(&["resize-window", "-t", home.trim(), "-x", w, "-y", h]);
    }
    let mut args = vec![
        "swap-pane", "-s", &pane, "-t", &shown, ";",
        "select-pane", "-t", &pane, ";",
        "set-option", "-w", "-t", window, CURRENT_OPTION, tab.name(),
    ];
    // A size an older tenx fixed on the task window by mistake.
    let fixed = run(&["show-options", "-wv", "-t", window, "window-size"]).unwrap_or_default();
    if !fixed.trim().is_empty() {
        args.extend([";", "set-option", "-wu", "-t", window, "window-size"]);
    }
    run(&args)?;
    Ok(())
}

/// The tab switches' lock, per server: `flock(2)`, so the kernel drops it
/// however a switch ends.
fn lock() -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let home = std::env::var("HOME").context("$HOME not set")?;
    let dir = std::path::PathBuf::from(home).join(".config/tenx");
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("tabs-{}.lock", super::socket())))?;
    // SAFETY: `file` owns the fd for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("lock the tab switch");
    }
    Ok(file)
}

/// `tenx internal tab <tab> --window <id>`, from a key or a click: what goes
/// wrong is said on the status line, since `run-shell` would put stderr over
/// the pane.
pub fn tab_command(tab: &str, window: &str) {
    let result = match Tab::parse(tab) {
        Some(tab) => switch_tab(window, tab),
        None => Err(anyhow::anyhow!("no tab called '{tab}'")),
    };
    if let Err(e) = result {
        let _ = run(&["display-message", "-t", window, &format!("{e:#}")]);
    }
}

/// Start `tab` in a waiting window of its own; returns its pane.
fn start_tab(window: &str, st: &State, tab: Tab) -> Result<String> {
    let (w, h) = (st.size.0.as_str(), st.size.1.as_str());
    let format = "#{pane_id}";
    let target = format!("{TABS_SESSION}:");
    let mut args: Vec<&str> = if super::has_session(TABS_SESSION) {
        vec!["new-window", "-d", "-t", &target, "-n", &st.name, "-c", &st.task_dir, "-P", "-F", format]
    } else {
        vec!["new-session", "-d", "-s", TABS_SESSION, "-n", &st.name, "-c", &st.task_dir, "-x", w, "-y", h, "-P", "-F", format]
    };
    let shell = run(&["show-options", "-gv", "default-shell"]).map(|s| s.trim().to_string()).unwrap_or_default();
    let shell = if shell.is_empty() { "/bin/sh".to_string() } else { shell };
    let command = tab.command(&shell, &super::tenx_cmd(&super::self_bin()?.to_string_lossy()));
    args.push(&command);
    let pane = run(&args)?.trim().to_string();
    if pane.is_empty() {
        bail!("tmux started no pane for the {} tab", tab.label());
    }
    run(&[
        "set-option", "-w", "-t", &pane, super::TASK_DIR_OPTION, &st.task_dir, ";",
        "set-option", "-w", "-t", &pane, OWNER_OPTION, window, ";",
        "set-option", "-w", "-t", window, tab.pane_option(), &pane,
    ])
    .with_context(|| format!("set up the {} tab", tab.label()))?;
    Ok(pane)
}

/// If `pane` waits in a tab, show it — what has to happen before a caller
/// puts it on screen (`focus_pane_in`). Returns its task window then.
pub fn reveal(pane: &str) -> Option<String> {
    let out = run(&["display-message", "-p", "-t", pane, &format!("#{{session_name}}\t#{{{OWNER_OPTION}}}")]).ok()?;
    let (session, owner) = out.trim().split_once('\t')?;
    if session != TABS_SESSION || owner.is_empty() {
        return None;
    }
    let st = state(owner).ok()??;
    let tab = st.panes.iter().find(|(_, p)| p == pane).map(|(t, _)| *t)?;
    switch_tab(owner, tab).ok()?;
    Some(owner.to_string())
}

/// `tenx internal tab-ended <pane>`: a shell or nvim tab's program ended —
/// the tab's own command runs it last (`Tab::command`). On screen, the agent
/// comes back (and with the agent gone too, the window closes); the tab's
/// pane then ends with this process.
pub fn tab_ended(pane: &str) {
    let Ok(out) = run(&["display-message", "-p", "-t", pane, "#{session_name}\t#{window_id}"]) else { return };
    let Some((session, here)) = out.trim().split_once('\t') else { return };
    if session == TABS_SESSION || !matches!(state(here), Ok(Some(_))) {
        return;
    }
    if switch_tab(here, Tab::Agent).is_err() {
        let _ = super::kill_window(here);
    }
}

/// Kill the waiting tabs whose task window is gone — the agent ended and
/// its window with it. The watcher runs this every tick; with no waiting tab
/// it costs one `list-windows`.
pub fn close_orphans() {
    let format = format!("#{{window_id}}\t#{{{OWNER_OPTION}}}");
    let Ok(waiting) = run(&["list-windows", "-t", TABS_SESSION, "-F", &format]) else { return };
    let open = run(&["list-windows", "-t", SESSION, "-F", "#{window_id}"]).unwrap_or_default();
    for line in waiting.lines() {
        let Some((id, owner)) = line.split_once('\t') else { continue };
        if !owner.is_empty() && !open.lines().any(|w| w == owner) {
            let _ = run(&["kill-window", "-t", id]);
        }
    }
}

/// Kill the waiting tabs of task window `window` (being closed, or gone).
pub fn close_waiting(window: &str, task_dir: Option<&str>) {
    let format = format!("#{{window_id}}\t#{{{OWNER_OPTION}}}\t#{{{}}}", super::TASK_DIR_OPTION);
    let Ok(out) = run(&["list-windows", "-t", TABS_SESSION, "-F", &format]) else { return };
    for line in out.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let [id, owner, dir] = f[..] else { continue };
        // Window ids restart with the server, so the task decides too.
        if owner == window && task_dir.is_none_or(|d| d == dir) {
            let _ = run(&["kill-window", "-t", id]);
        }
    }
}

/// Put every tabbed task window back on its agent — before `tenx tmux
/// restart` snapshots the session, which rebuilds the windows as they look.
pub fn show_agents() {
    let Ok(out) = run(&["list-windows", "-t", SESSION, "-F", &format!("#{{window_id}}\t#{{{CURRENT_OPTION}}}")]) else { return };
    for line in out.lines() {
        if let Some((id, current)) = line.split_once('\t')
            && Tab::parse(current).is_some_and(|t| t != Tab::Agent)
        {
            let _ = switch_tab(id, Tab::Agent);
        }
    }
}
