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

/// What to do next while walking the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelStep {
    /// Press `↓` again.
    Down,
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

/// Decide the next key from the pane as it is now. `label` is what the
/// subagent's row shows (its description, else its type); `previous` the
/// selected row before the last press; `presses` how many `↓` were sent.
pub fn next_step(capture: &str, label: &str, previous: Option<&str>, presses: u32) -> PanelStep {
    let selected = selected_row(capture);
    match selected.as_deref() {
        Some(row) if row_matches(row, label) => PanelStep::Open,
        // The last press moved nothing: the bottom of the panel.
        Some(row) if previous == Some(row) => PanelStep::GiveUp { clear: true },
        Some(_) if presses >= MAX_PRESSES => PanelStep::GiveUp { clear: true },
        None if presses >= PRESSES_TO_ENTER => PanelStep::GiveUp { clear: false },
        _ => PanelStep::Down,
    }
}

/// The selected row of the agent panel, whitespace collapsed, without its
/// `❯` and status glyph — or `None` when no row is selected. Only lines below
/// the last rule of the pane count: the prompt echoes in the transcript above
/// also start with `❯`.
pub fn selected_row(capture: &str) -> Option<String> {
    let lines: Vec<String> = capture.lines().map(crate::dialog::strip_ansi).collect();
    let start = lines.iter().rposition(|l| is_rule(l)).map_or(0, |i| i + 1);
    lines[start..].iter().find_map(|l| {
        let rest = l.trim_start().strip_prefix("❯ ")?;
        let mut chars = rest.chars();
        let glyph = chars.next()?;
        // A panel row: a status glyph and a space, not typed text.
        if glyph.is_alphanumeric() || glyph.is_ascii_punctuation() || chars.next() != Some(' ') {
            return None;
        }
        let text: String = chars.as_str().split_whitespace().collect::<Vec<_>>().join(" ");
        (!text.is_empty()).then_some(text)
    })
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

/// A row shows the subagent when it contains the start of its label — the
/// panel truncates long descriptions, and a row never reads just `main`.
fn row_matches(row: &str, label: &str) -> bool {
    let key = match_key(label);
    !key.is_empty() && row != "main" && flat(row).contains(&key)
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
        assert_eq!(next_step(&idle, "Alpha sleeper", None, 0), PanelStep::Down);
        // The first ↓ lands on a status-line pill: still no row.
        let pill = pane("❯ ", &["  ⏵⏵ bypass permissions on · 2 shells · Enter to view tasks", "", "  ⏺ main", ALPHA]);
        assert_eq!(next_step(&pill, "Alpha sleeper", None, 1), PanelStep::Down);
        // No panel ever shows up (text in the prompt, say): give up, keep hands off.
        assert_eq!(next_step(&idle, "Alpha sleeper", None, 3), PanelStep::GiveUp { clear: false });
    }

    #[test]
    fn walks_down_to_the_row_and_opens_it() {
        let on_main = pane("❯ ", &["  ↑/↓ to select", "", "❯ ⏺ main", ALPHA, BETA]);
        assert_eq!(selected_row(&on_main).as_deref(), Some("main"));
        assert_eq!(next_step(&on_main, "Alpha sleeper", None, 2), PanelStep::Down);
        let on_alpha = pane("❯ ", &["  ⏺ main", "❯ ◯ general-purpose  Alpha sleeper    5s · ↓ 30.4k tokens", BETA]);
        assert_eq!(selected_row(&on_alpha).as_deref(), Some("general-purpose Alpha sleeper 5s · ↓ 30.4k tokens"));
        assert_eq!(next_step(&on_alpha, "Alpha sleeper", Some("main"), 3), PanelStep::Open);
        // Beta is further down.
        assert_eq!(next_step(&on_alpha, "Beta sleeper", Some("main"), 3), PanelStep::Down);
    }

    #[test]
    fn gives_up_at_the_bottom_and_clears() {
        let on_beta = pane("❯ ", &["  ⏺ main", ALPHA, "❯ ◯ general-purpose  Beta sleeper   5s"]);
        let row = selected_row(&on_beta).unwrap();
        assert_eq!(next_step(&on_beta, "Gamma", Some(&row), 5), PanelStep::GiveUp { clear: true });
        assert_eq!(next_step(&on_beta, "Gamma", Some("main"), MAX_PRESSES), PanelStep::GiveUp { clear: true });
    }

    #[test]
    fn a_long_description_matches_its_truncated_row() {
        let row = "general-purpose Map the tenx session registry and hoo… 2m";
        assert!(row_matches(row, "Map the tenx session registry and hooks in detail"));
        assert!(!row_matches("main", "main"));
        assert!(!row_matches(row, ""));
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
