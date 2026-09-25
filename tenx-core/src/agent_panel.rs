//! Opening a subagent in Claude Code's own agent view, from outside its pane.
//!
//! Claude Code lists a session's subagents in a panel under its prompt
//! (measured on 2.1.282):
//!
//! ```text
//! ──────────────────────────────────────────────────────────────
//!   ⏵⏵ bypass permissions on · 2 shells · ← 3 agents · ↓ to manage
//!   ⏺ main
//!   ◯ general-purpose  Alpha sleeper              5s · ↓ 30.4k tokens
//!   ◯ general-purpose  Beta sleeper               5s · ↓ 30.4k tokens
//! ```
//!
//! `↓` walks into it (the first press may land on a pill of the status line,
//! the next on `main`, then down the rows), the selected row is marked `❯`,
//! `Enter` opens that agent's view — its transcript, with a
//! `─ Alpha sleeper ─` title over a `Message @general-purpose…` prompt — and
//! `Escape` with a row selected only clears the selection. There is no way to
//! name an agent from outside, so tenx does what a finger would: press `↓`,
//! read the pane, and stop on the row that shows the subagent's description.
//! [`next_step`] is that decision, one capture at a time; the binary does the
//! pressing and capturing (`tui::client`).
//!
//! Idle agents' rows are hidden after a while and surplus idle ones collapse
//! into one row, so a finished subagent may not be there to select: the walk
//! then gives up and clears its selection. (`/tasks` drops a finished
//! subagent on the same schedule, about 30 s after it ends, so it is no way
//! around this: after that Claude Code has no view of the subagent at all, and
//! the caller follows its transcript instead.)

/// Which view to put in the pane: the session's own conversation, or one of
/// its subagents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    Main,
    Agent(AgentRef<'a>),
}

/// How to find a subagent's row. A row reads `<type>  <text>`, and the text
/// is the spawn's description only at first: once the subagent has been at
/// it a while, Claude shows a live summary there instead (`general-purpose
/// Running third background sleep call`). So a row that still shows the
/// description is taken; failing that, rows are listed in launch order, and
/// the subagent is the `nth` of the `peers` running subagents of its type —
/// used only when the panel lists exactly that many rows of the type, so a
/// hidden or extra row makes it give up rather than open the wrong agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRef<'a> {
    pub label: &'a str,
    pub agent_type: &'a str,
    /// Its place among the session's running subagents of `agent_type`, in
    /// launch order; `None` for one that isn't running (only its label can
    /// find it).
    pub nth: Option<usize>,
    pub peers: usize,
}

impl<'a> Target<'a> {
    /// A subagent known by its label alone.
    pub fn label(label: &'a str) -> Target<'a> {
        Target::Agent(AgentRef { label, agent_type: "", nth: None, peers: 0 })
    }

    /// Which of `rows` is this target's.
    fn index(self, rows: &[PanelRow]) -> Option<usize> {
        let is_main = |r: &PanelRow| r.text == "main" || r.text.starts_with("main ");
        match self {
            Target::Main => rows.iter().position(is_main),
            Target::Agent(a) => rows.iter().position(|r| row_matches(&r.text, a.label)).or_else(|| {
                let nth = a.nth?;
                let prefix = format!("{} ", a.agent_type);
                let typed: Vec<usize> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| !a.agent_type.is_empty() && !is_main(r) && r.text.starts_with(&prefix))
                    .map(|(i, _)| i)
                    .collect();
                (typed.len() == a.peers).then(|| typed.get(nth).copied()).flatten()
            }),
        }
    }
}

/// What to do next while walking the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelStep {
    /// Press `↓`: into the panel, or toward a row further down.
    Down,
    /// Press `↑`: toward a row further up (the selection stays on the last
    /// row opened, so the next target is as often above it as below).
    Up,
    /// The subagent's row is selected: press `Enter`.
    Open,
    /// It isn't there. `clear`: a row is selected, press `Escape` to leave
    /// the panel as it was (never when nothing is selected — then `Escape`
    /// would reach the prompt, or interrupt an agent being viewed).
    GiveUp { clear: bool },
}

/// Give up after this many presses: well past any panel a person can read.
pub const MAX_PRESSES: u32 = 16;

/// Presses allowed before anything in the panel is selected — the first can
/// land on a status-line pill.
const PRESSES_TO_ENTER: u32 = 3;

/// Decide the next key from the pane as it is now: `target` is the row to
/// reach; `previous` the selected row before the last press; `presses` how
/// many `↓` were sent.
pub fn next_step(capture: &str, target: Target, previous: Option<&str>, presses: u32) -> PanelStep {
    let rows = panel_rows(capture);
    let Some(at) = rows.iter().position(|r| r.selected) else {
        // Not in the panel yet: `↓` walks in (past a status-line pill, maybe).
        return if presses >= PRESSES_TO_ENTER { PanelStep::GiveUp { clear: false } } else { PanelStep::Down };
    };
    let to = target.index(&rows);
    if to == Some(at) {
        return PanelStep::Open;
    }
    // The last press moved nothing, or the walk has gone on too long.
    if previous == Some(rows[at].text.as_str()) || presses >= MAX_PRESSES {
        return PanelStep::GiveUp { clear: true };
    }
    match to {
        Some(to) if to > at => PanelStep::Down,
        Some(_) => PanelStep::Up,
        None => PanelStep::GiveUp { clear: true },
    }
}

/// One row of the agent panel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PanelRow {
    /// Under the cursor (`❯`).
    selected: bool,
    /// The view the pane shows (`⏺`).
    viewed: bool,
    /// Its text, whitespace collapsed, without the markers.
    text: String,
}

/// The agent panel's rows, top to bottom: the lines below the pane's last
/// rule that read `[❯ ]<glyph> <text>`. The prompt echoes in the transcript
/// above also start with `❯`, and the status line's `⏵⏵` is no row.
fn panel_rows(capture: &str) -> Vec<PanelRow> {
    let lines: Vec<String> = capture.lines().map(crate::dialog::strip_ansi).collect();
    let start = lines.iter().rposition(|l| is_rule(l)).map_or(0, |i| i + 1);
    lines[start..]
        .iter()
        .filter_map(|l| {
            let t = l.trim_start();
            let (selected, t) = match t.strip_prefix("❯ ") {
                Some(rest) => (true, rest),
                None => (false, t),
            };
            let mut chars = t.chars();
            let glyph = chars.next()?;
            if glyph.is_alphanumeric() || glyph.is_ascii_punctuation() || chars.next() != Some(' ') {
                return None;
            }
            let text = flat(chars.as_str());
            (!text.is_empty()).then_some(PanelRow { selected, viewed: glyph == '⏺', text })
        })
        .collect()
}

/// The selected row of the agent panel, whitespace collapsed, without its
/// `❯` and status glyph — or `None` when no row is selected. Only lines below
/// the last rule of the pane count: the prompt echoes in the transcript above
/// also start with `❯`.
pub fn selected_row(capture: &str) -> Option<String> {
    panel_rows(capture).into_iter().find(|r| r.selected).map(|r| r.text)
}

/// The panel row of the view the pane shows — Claude marks it `⏺` (`⏺ main`,
/// `❯ ⏺ general-purpose  Alpha sleeper`) — or `None` when the panel lists no
/// such row (no subagents listed: the pane shows its main view).
pub fn viewed_row(capture: &str) -> Option<String> {
    panel_rows(capture).into_iter().find(|r| r.viewed).map(|r| r.text)
}

/// Whether the pane already shows `target`, so nothing needs pressing. Main
/// is showing unless the panel marks a subagent's row as the viewed one.
pub fn showing(capture: &str, target: Target) -> bool {
    let rows = panel_rows(capture);
    match (rows.iter().position(|r| r.viewed), target) {
        (Some(at), t) => t.index(&rows) == Some(at),
        (None, Target::Main) => true,
        (None, Target::Agent(a)) => view_open(capture, a.label),
    }
}

/// Whether the agent view of `label` is showing: its title sits in a rule,
/// `───── Alpha sleeper ─`.
pub fn view_open(capture: &str, label: &str) -> bool {
    let key = match_key(label);
    capture.lines().map(crate::dialog::strip_ansi).any(|l| {
        let t = l.trim();
        t.starts_with('─') && t.ends_with('─') && flat(t).contains(&key)
    })
}

/// A row shows the subagent when the text after its type starts with the
/// start of its label — the panel truncates long descriptions — and it is
/// never the `main` row.
fn row_matches(row: &str, label: &str) -> bool {
    let key = match_key(label);
    let row = flat(row);
    let body = row.split_once(' ').map_or(row.as_str(), |(_, b)| b);
    !key.is_empty() && row != "main" && body.starts_with(&key)
}

/// The part of a label to look for: its first words, up to what a narrow
/// pane still shows before truncating.
fn match_key(label: &str) -> String {
    flat(label).chars().take(24).collect::<String>().trim_end().to_string()
}

fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| c == '─')
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────────";

    fn pane(prompt: &str, footer: &[&str]) -> String {
        let mut s = format!("❯ Launch two background agents\n⏺ 2 background agents launched\n{RULE}\n{prompt}\n{RULE}\n");
        for l in footer {
            s.push_str(l);
            s.push('\n');
        }
        s
    }

    const ALPHA: &str = "  ◯ general-purpose  Alpha sleeper            5s · ↓ 30.4k tokens";
    const BETA: &str = "  ◯ general-purpose  Beta sleeper             5s · ↓ 30.4k tokens";

    #[test]
    fn nothing_selected_until_the_panel_is_entered() {
        let idle = pane("❯ ", &["  ⏵⏵ bypass permissions on · ← 3 agents · ↓ to manage", "  ⏺ main", ALPHA, BETA]);
        assert_eq!(selected_row(&idle), None);
        // The echoed prompt above the rules is not a row.
        assert_eq!(next_step(&idle, Target::label("Alpha sleeper"), None, 0), PanelStep::Down);
        // The first ↓ lands on a status-line pill: still no row.
        let pill = pane("❯ ", &["  ⏵⏵ bypass permissions on · 2 shells · Enter to view tasks", "", "  ⏺ main", ALPHA]);
        assert_eq!(next_step(&pill, Target::label("Alpha sleeper"), None, 1), PanelStep::Down);
        // No panel ever shows up (text in the prompt, say): give up, keep hands off.
        assert_eq!(next_step(&idle, Target::label("Alpha sleeper"), None, 3), PanelStep::GiveUp { clear: false });
    }

    #[test]
    fn walks_down_to_the_row_and_opens_it() {
        let on_main = pane("❯ ", &["  ↑/↓ to select", "", "❯ ⏺ main", ALPHA, BETA]);
        assert_eq!(selected_row(&on_main).as_deref(), Some("main"));
        assert_eq!(next_step(&on_main, Target::label("Alpha sleeper"), None, 2), PanelStep::Down);
        assert_eq!(next_step(&on_main, Target::Main, None, 2), PanelStep::Open);
        let on_alpha = pane("❯ ", &["  ⏺ main", "❯ ◯ general-purpose  Alpha sleeper    5s · ↓ 30.4k tokens", BETA]);
        assert_eq!(selected_row(&on_alpha).as_deref(), Some("general-purpose Alpha sleeper 5s · ↓ 30.4k tokens"));
        assert_eq!(next_step(&on_alpha, Target::label("Alpha sleeper"), Some("main"), 3), PanelStep::Open);
        // Beta is further down.
        assert_eq!(next_step(&on_alpha, Target::label("Beta sleeper"), Some("main"), 3), PanelStep::Down);
    }

    #[test]
    fn walks_up_to_a_row_above_the_selection() {
        // The selection stays on the last row opened (Eta); Zeta and main are above.
        let on_eta = pane("❯ Message @general-purpose…", &["  ◯ main", ALPHA, "❯ ⏺ general-purpose  Eta sleeper   6s"]);
        assert_eq!(next_step(&on_eta, Target::label("Alpha sleeper"), None, 0), PanelStep::Up);
        assert_eq!(next_step(&on_eta, Target::Main, None, 0), PanelStep::Up);
        assert_eq!(next_step(&on_eta, Target::label("Eta sleeper"), None, 0), PanelStep::Open);
        // A target the panel doesn't list: give up without walking.
        assert_eq!(next_step(&on_eta, Target::label("Gamma"), None, 0), PanelStep::GiveUp { clear: true });
    }

    #[test]
    fn finds_a_row_showing_a_live_summary_by_type_and_order() {
        // Claude has replaced both descriptions with what the agents are doing.
        let panel = pane("❯ ", &[
            "❯ ⏺ main",
            "  ◯ general-purpose  Running third background sleep call   1m 35s",
            "  ◯ Explore  Reading src/tui/column.rs                      40s",
            "  ◯ general-purpose  Waiting on the build                   20s",
        ]);
        let second_gp = Target::Agent(AgentRef { label: "Test agent for switching views", agent_type: "general-purpose", nth: Some(1), peers: 2 });
        assert_eq!(next_step(&panel, second_gp, None, 0), PanelStep::Down);
        let rows = panel_rows(&panel);
        assert_eq!(second_gp.index(&rows), Some(3));
        let explore = Target::Agent(AgentRef { label: "Map the hooks", agent_type: "Explore", nth: Some(0), peers: 1 });
        assert_eq!(explore.index(&rows), Some(2));
        // A count that doesn't add up (a row hidden, one we don't know): no guess.
        let unsure = Target::Agent(AgentRef { label: "Unknown one", agent_type: "general-purpose", nth: Some(0), peers: 3 });
        assert_eq!(unsure.index(&rows), None);
        // A description still on its row wins over the order.
        let described = pane("❯ ", &["  ⏺ main", "  ◯ general-purpose  Alpha sleeper   5s", "  ◯ general-purpose  Beta sleeper   5s"]);
        let beta = Target::Agent(AgentRef { label: "Beta sleeper", agent_type: "general-purpose", nth: Some(0), peers: 2 });
        assert_eq!(beta.index(&panel_rows(&described)), Some(2));
        // And the view already up is recognised the same way.
        let viewing = pane("❯ Message @general-purpose…", &["  ◯ main", "  ⏺ general-purpose  Running third background sleep call   1m 35s"]);
        let first = Target::Agent(AgentRef { label: "Test agent for switching views", agent_type: "general-purpose", nth: Some(0), peers: 1 });
        assert!(showing(&viewing, first));
    }

    #[test]
    fn gives_up_at_the_bottom_and_clears() {
        let on_beta = pane("❯ ", &["  ⏺ main", ALPHA, "❯ ◯ general-purpose  Beta sleeper   5s"]);
        let row = selected_row(&on_beta).unwrap();
        assert_eq!(next_step(&on_beta, Target::label("Gamma"), Some(&row), 5), PanelStep::GiveUp { clear: true });
        assert_eq!(next_step(&on_beta, Target::label("Gamma"), Some("main"), MAX_PRESSES), PanelStep::GiveUp { clear: true });
    }

    #[test]
    fn a_long_description_matches_its_truncated_row() {
        let row = "general-purpose Map the tenx session registry and hoo… 2m";
        assert!(row_matches(row, "Map the tenx session registry and hooks in detail"));
        assert!(!row_matches("main", "main"));
        assert!(!row_matches(row, ""));
    }

    #[test]
    fn knows_which_view_the_pane_shows() {
        // Main view, rows hidden (idle agents): main is showing.
        let main_idle = pane("❯ ", &["  ⏵⏵ auto mode on · ← 3 agents"]);
        assert_eq!(viewed_row(&main_idle), None);
        assert!(showing(&main_idle, Target::Main));
        assert!(!showing(&main_idle, Target::label("Alpha sleeper")));
        // Main view with rows listed: `⏺ main`.
        let main_rows = pane("❯ ", &["  ⏺ main", ALPHA]);
        assert_eq!(viewed_row(&main_rows).as_deref(), Some("main"));
        assert!(showing(&main_rows, Target::Main));
        // Viewing Alpha, its row selected or not.
        for alpha in ["  ⏺ general-purpose  Alpha sleeper   5s", "❯ ⏺ general-purpose  Alpha sleeper   5s"] {
            let viewing = pane("❯ Message @general-purpose…", &["  ◯ main", alpha, BETA]);
            assert!(showing(&viewing, Target::label("Alpha sleeper")));
            assert!(!showing(&viewing, Target::Main));
            assert!(!showing(&viewing, Target::label("Beta sleeper")));
        }
        // The transcript's own `⏺` lines above the rules don't count.
        assert_eq!(viewed_row(&format!("⏺ Agent(Alpha)\n{RULE}\n❯ \n{RULE}\n  ⏵⏵ auto\n")), None);
    }

    #[test]
    fn the_opened_view_is_recognised_by_its_title() {
        let view = format!(
            "❯ Run this bash command: sleep 90\n⏺ Done!\n{RULE}─── Alpha sleeper ─\n❯ Message @general-purpose…\n{RULE}\n  ↑/↓ to select\n"
        );
        assert!(view_open(&view, "Alpha sleeper"));
        assert!(!view_open(&view, "Beta sleeper"));
        // The typed prompt is not a row either.
        assert_eq!(selected_row(&view), None);
    }
}
