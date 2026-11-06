//! A task window's tabs: the agent, a shell and nvim (on `TASK.md`), one at a time over
//! the whole window. tmux has no hidden panes, so a tab that isn't shown waits
//! in a window of a session of its own (`tmux::TABS_SESSION`), and showing it
//! is a `swap-pane` with the one on screen — the task keeps its one window,
//! which is what everything else finds a task by. This is what a switch
//! *means*; `tmux::switch_tab` does it.
//!
//! The strip is tmux's: the top line of the window's pane (`strip_format`).
//! tmux makes no click there reach a key binding, so a client that draws the
//! session itself matches a click against the line it shows (`tab_at`).
//!
//! What a tabbed window remembers is window options on the task window:
//! [`CURRENT_OPTION`] the tab on screen (its presence is what makes a window
//! tabbed), and one [`Tab::pane_option`] per tab started, the pane it runs in.

/// The tab on screen, by [`Tab::name`]. Set on a window built with tabs only.
pub const CURRENT_OPTION: &str = "@tenx_tab";

/// On a waiting tab's window: the task window it belongs to (`@12`).
pub const OWNER_OPTION: &str = "@tenx_tab_of";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Agent,
    Shell,
    Nvim,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Agent, Tab::Shell, Tab::Nvim];

    /// What the options and commands call it.
    pub fn name(self) -> &'static str {
        match self {
            Tab::Agent => "agent",
            Tab::Shell => "shell",
            Tab::Nvim => "nvim",
        }
    }

    /// What the tab strip shows.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Agent => "agent",
            Tab::Shell => "shell",
            Tab::Nvim => "nvim",
        }
    }

    /// Its number on the strip and its key (`Alt+1`…).
    pub fn number(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) + 1
    }

    /// A tab by name or by number.
    pub fn parse(s: &str) -> Option<Tab> {
        let s = s.trim();
        Tab::ALL.into_iter().find(|t| t.name() == s || t.number().to_string() == s)
    }

    /// The window option holding the pane this tab runs in (`%7`).
    pub fn pane_option(self) -> &'static str {
        match self {
            Tab::Agent => "@tenx_tab_agent",
            Tab::Shell => "@tenx_tab_shell",
            Tab::Nvim => "@tenx_tab_nvim",
        }
    }

    /// The shell command a new tab runs: its program (`shell` as a login
    /// shell, or nvim on `TASK.md`), then `tenx internal tab-ended`, so the
    /// tab's end puts the agent back without a tmux hook — tmux doesn't run
    /// them everywhere (Ubuntu's CI runners ran none). `tenx` is the quoted
    /// command that runs tenx. The agent's tab is never started here: it is
    /// the window's first pane.
    pub fn command(self, shell: &str, tenx: &str) -> String {
        let program = match self {
            Tab::Nvim => "nvim TASK.md".to_string(),
            Tab::Agent | Tab::Shell => format!("{} -l", quote(shell)),
        };
        format!("{program}; {tenx} internal tab-ended \"$TMUX_PANE\"")
    }
}

/// POSIX single-quote quoting.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// How a window is laid out when it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// The agent over the whole window, a shell and nvim on `TASK.md` as tabs.
    Tabs,
    /// The agent left, `TASK.md` top-right, a shell bottom-right.
    Split,
    /// An executable that lays the window out itself.
    Script(String),
}

/// The `layout` setting: the workspace's, else the global one. Empty or
/// `tabs` is the default, `split` the three panes tenx had before.
pub fn layout(workspace: &str, global: &str) -> Layout {
    let value = if workspace.trim().is_empty() { global.trim() } else { workspace.trim() };
    match value {
        "" | "tabs" => Layout::Tabs,
        "split" => Layout::Split,
        script => Layout::Script(script.to_string()),
    }
}

/// The tab really on screen: the one whose pane is in the window (`shown`,
/// its panes). The window's own [`CURRENT_OPTION`] only says what the last
/// switch meant to show — two switches at once (a phone's tap arrives as a
/// tap and as a click) used to leave it naming another tab than the one shown.
pub fn on_screen(panes: &[(Tab, String)], shown: &[String]) -> Option<Tab> {
    panes.iter().find(|(_, p)| !p.is_empty() && shown.contains(p)).map(|(t, _)| *t)
}

/// What showing a tab takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// It is already on screen.
    Stay,
    /// Swap this waiting pane in.
    Show(String),
    /// Start it first: it never ran, or its program ended.
    Start,
    /// The agent ended; its tab can't be started again here.
    AgentGone,
}

/// The step to show `target` when `current` is on screen. `target_pane` is
/// what the window remembers for it, `alive` whether that pane still runs.
pub fn step(current: Tab, target: Tab, target_pane: Option<&str>, alive: bool) -> Step {
    if current == target {
        return Step::Stay;
    }
    match target_pane.filter(|p| !p.is_empty()) {
        Some(p) if alive => Step::Show(p.to_string()),
        _ if target == Tab::Agent => Step::AgentGone,
        _ => Step::Start,
    }
}

/// What a tab shows on the strip, spaces included — also what a click is
/// matched against ([`tab_at`]).
fn strip_text(tab: Tab) -> String {
    format!(" {} {} ", tab.number(), tab.label())
}

/// tmux's `pane-border-format`: in a tabbed window the tab strip, on the top
/// line of its first pane (the one tabs swap through; a background agent's
/// log below it gets an empty line), the tab on screen in `on`/`on_bg`, the
/// others in `off`. Elsewhere tmux's own format. The row's own
/// background (`tmux::mark_tabbed`) sets it off from the tab under it.
pub fn strip_format(on: &str, on_bg: &str, off: &str) -> String {
    let tabs: String = Tab::ALL
        .iter()
        .map(|t| {
            let text = strip_text(*t);
            format!("#{{?#{{==:#{{{CURRENT_OPTION}}},{name}}},#[fg={on} bg={on_bg} bold]{text},#[fg={off}]{text}}}#[default] ", name = t.name())
        })
        .collect();
    let own = "#{?pane_active,#[reverse],}#{pane_index}#[default] #{pane_title}";
    format!("#{{?#{{{CURRENT_OPTION}}},#{{?#{{==:#{{pane_index}},0}},{tabs},}},{own}}}")
}

/// The tab under column `x` of `row`, the strip's line as it is on screen —
/// how a client that draws the session itself makes the strip clickable.
pub fn tab_at(row: &str, x: usize) -> Option<Tab> {
    let chars: Vec<char> = row.chars().collect();
    Tab::ALL.into_iter().find(|t| {
        let text: Vec<char> = strip_text(*t).chars().collect();
        chars.windows(text.len()).enumerate().any(|(i, w)| w == text.as_slice() && (i..i + text.len()).contains(&x))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_parse_by_name_and_number() {
        assert_eq!(Tab::parse("shell"), Some(Tab::Shell));
        assert_eq!(Tab::parse("3"), Some(Tab::Nvim));
        assert_eq!(Tab::parse(" agent"), Some(Tab::Agent));
        assert_eq!(Tab::parse("TASK.md"), None);
        assert_eq!(Tab::parse("4"), None);
    }

    #[test]
    fn a_tab_tells_tenx_when_it_ends() {
        assert_eq!(Tab::Shell.command("/bin/zsh", "'/x/tenx'"), "'/bin/zsh' -l; '/x/tenx' internal tab-ended \"$TMUX_PANE\"");
        assert_eq!(Tab::Nvim.command("/bin/zsh", "'/x/tenx'"), "nvim TASK.md; '/x/tenx' internal tab-ended \"$TMUX_PANE\"");
    }

    #[test]
    fn layout_setting() {
        assert_eq!(layout("", ""), Layout::Tabs);
        assert_eq!(layout("", "split"), Layout::Split);
        assert_eq!(layout("tabs", "split"), Layout::Tabs);
        assert_eq!(layout(" ./layout.sh ", "split"), Layout::Script("./layout.sh".into()));
    }

    #[test]
    fn the_tab_on_screen_is_the_one_whose_pane_is_shown() {
        let panes = vec![(Tab::Agent, "%1".to_string()), (Tab::Shell, "%2".to_string()), (Tab::Nvim, String::new())];
        assert_eq!(on_screen(&panes, &["%2".into()]), Some(Tab::Shell));
        assert_eq!(on_screen(&panes, &["%9".into(), "%1".into()]), Some(Tab::Agent), "beside a background agent's log");
        assert_eq!(on_screen(&panes, &["%9".into()]), None);
        assert_eq!(on_screen(&panes, &["".into()]), None);
    }

    #[test]
    fn steps() {
        assert_eq!(step(Tab::Agent, Tab::Agent, Some("%1"), true), Step::Stay);
        assert_eq!(step(Tab::Agent, Tab::Shell, Some("%4"), true), Step::Show("%4".into()));
        assert_eq!(step(Tab::Agent, Tab::Shell, Some("%4"), false), Step::Start);
        assert_eq!(step(Tab::Agent, Tab::Nvim, None, false), Step::Start);
        assert_eq!(step(Tab::Shell, Tab::Agent, Some("%1"), false), Step::AgentGone);
        assert_eq!(step(Tab::Shell, Tab::Agent, Some(""), false), Step::AgentGone);
    }

    #[test]
    fn strip_format_marks_each_tab_on_the_first_pane() {
        let f = strip_format("#000", "#fff", "#888");
        assert!(f.starts_with("#{?#{@tenx_tab},#{?#{==:#{pane_index},0},"));
        for t in Tab::ALL {
            assert!(f.contains(&format!("#{{==:#{{@tenx_tab}},{}}}", t.name())));
        }
        assert!(f.contains(" 3 nvim "));
        assert!(f.ends_with(",#{?pane_active,#[reverse],}#{pane_index}#[default] #{pane_title}}"), "{f}");
        assert!(!f.contains('"'), "it goes into the config in double quotes");
    }

    #[test]
    fn a_click_finds_the_tab_under_it() {
        let row = "   1 agent   2 shell   3 nvim                   ";
        assert_eq!(tab_at(row, 3), Some(Tab::Agent));
        assert_eq!(tab_at(row, 9), Some(Tab::Agent));
        assert_eq!(tab_at(row, 13), Some(Tab::Shell));
        assert_eq!(tab_at(row, 26), Some(Tab::Nvim));
        assert_eq!(tab_at(row, 40), None);
        assert_eq!(tab_at("the agent's own screen", 5), None);
    }
}
