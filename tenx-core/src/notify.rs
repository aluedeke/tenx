//! `tenx notify`: a notification an agent sends because you asked it to —
//! now, or when its turn is over (`--when-done`). The rules without I/O:
//! whether you are at the Mac looking at the task already (then only the
//! Mac shows it, not the phone), when an armed `--when-done` fires, and
//! what its text is.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The file in a task directory that arms `--when-done`.
pub const ARMED_FILE: &str = ".tenx-notify";

/// Input at the Mac this recent counts as being there.
pub const AT_MAC_IDLE_SECS: u64 = 120;

/// The longest body a notification gets; iOS shows about four lines.
pub const BODY_MAX: usize = 240;

/// You are at the Mac and looking at the task: the terminal attached to the
/// tenx session shows its window (`shown`, `None` with no terminal attached)
/// and the Mac had input within `AT_MAC_IDLE_SECS` (`idle_secs`, `None`
/// when unknown). Then a notification for it goes to the Mac alone.
pub fn at_mac(idle_secs: Option<u64>, shown: Option<&Path>, task: &Path) -> bool {
    idle_secs.is_some_and(|s| s < AT_MAC_IDLE_SECS) && shown.is_some_and(|d| d == task)
}

/// An armed `--when-done`: when it was armed (Unix ms), and the text, if
/// the agent gave one — else the agent's last reply is the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Armed {
    pub armed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// An armed notification fires once the task is done with a turn that
/// ended after it was armed — not on a `done` from before, which is what a
/// task you just sent a message to still shows for a moment.
pub fn fires(armed: &Armed, done: bool, changed_ms: Option<u64>) -> bool {
    done && changed_ms.is_some_and(|c| c >= armed.armed_at_ms)
}

/// A reply as a notification's body: its Markdown as plain text on one
/// line, cut at a word to `max` characters.
pub fn summary(markdown: &str, max: usize) -> String {
    let text = crate::readaloud::spoken_text(markdown).split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn at_the_mac_needs_recent_input_and_the_task_on_screen() {
        let task = PathBuf::from("/ws/tasks/a");
        let other = PathBuf::from("/ws/tasks/b");
        assert!(at_mac(Some(5), Some(&task), &task));
        assert!(!at_mac(Some(5), Some(&other), &task));
        assert!(!at_mac(Some(5), None, &task));
        assert!(!at_mac(Some(AT_MAC_IDLE_SECS), Some(&task), &task));
        assert!(!at_mac(None, Some(&task), &task));
    }

    #[test]
    fn fires_only_on_a_turn_that_ended_after_arming() {
        let armed = Armed { armed_at_ms: 1_000, title: None, message: None };
        assert!(fires(&armed, true, Some(1_000)));
        assert!(fires(&armed, true, Some(5_000)));
        assert!(!fires(&armed, true, Some(999)));
        assert!(!fires(&armed, false, Some(5_000)));
        assert!(!fires(&armed, true, None));
    }

    #[test]
    fn armed_file_round_trips_and_omits_what_is_unset() {
        let armed = Armed { armed_at_ms: 7, title: None, message: Some("CI is green".into()) };
        let json = serde_json::to_string(&armed).unwrap();
        assert_eq!(json, r#"{"armed_at_ms":7,"message":"CI is green"}"#);
        assert_eq!(serde_json::from_str::<Armed>(&json).unwrap(), armed);
    }

    #[test]
    fn summary_is_plain_one_line_and_cut_at_a_word() {
        assert_eq!(summary("## Done\n\n- tests **pass**\n- PR opened", 100), "Done. tests pass. PR opened.");
        let long = "word ".repeat(100);
        let s = summary(&long, 30);
        assert!(s.chars().count() <= 30, "{s}");
        assert!(s.ends_with("word…"), "{s}");
    }
}
