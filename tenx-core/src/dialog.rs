//! Answering a Claude Code permission prompt from outside its pane.
//!
//! Claude Code has no API for a third program to answer a pending prompt: the
//! `PermissionRequest` hook must decide *before* the dialog appears, and the
//! inter-session messaging socket is a mailbox, not a permission channel. What
//! it does have is a plain terminal dialog, in the pane its session registry
//! names (`"tmux":"tenx:@14.%40"`):
//!
//! ```text
//!  Do you want to proceed?
//!  ❯ 1. Yes
//!    2. No
//!  Esc to cancel · Tab to amend
//! ```
//!
//! So the overlay answers it the way a finger would: `tmux send-keys` into
//! that pane. `Enter` takes the highlighted option (always "Yes" — the dialog
//! opens on it), `Escape` cancels. Never a digit for "No": in three-option
//! dialogs `2` is "Yes, and don't ask again".
//!
//! The risk is the race between the overlay's last look and the keystroke —
//! the prompt may have been answered from the pane, or the session may have
//! moved on to something else that also takes `Enter` (an `AskUserQuestion`
//! picks its first option). [`permission_dialog_visible`] is the guard: the
//! binary captures the pane right before sending and only proceeds if the
//! dialog is still on screen.

/// What to send to the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
}

impl Answer {
    /// The `tmux send-keys` key name.
    pub fn key(self) -> &'static str {
        match self {
            Answer::Yes => "Enter",
            Answer::No => "Escape",
        }
    }

    pub fn verb(self) -> &'static str {
        match self {
            Answer::Yes => "approved",
            Answer::No => "denied",
        }
    }
}

/// The `waitingFor` value Claude Code writes for a tool permission dialog.
/// Other waits ("input needed", an elicitation, a sandbox request) are not
/// answerable with a blind `Enter`.
pub const PERMISSION_PROMPT: &str = "permission prompt";

/// True if a pane capture ends in Claude Code's permission dialog: the
/// question and its first option, both within the last few non-blank lines.
/// Deliberately literal — a future wording change fails closed (the overlay
/// refuses to send) rather than open.
pub fn permission_dialog_visible(capture: &str) -> bool {
    let plain = strip_ansi(capture);
    let tail: Vec<&str> = plain.lines().rev().filter(|l| !l.trim().is_empty()).take(12).collect();
    let has_question = tail.iter().any(|l| l.contains("Do you want to proceed?") || l.contains("Do you want to"));
    let has_yes = tail.iter().any(|l| l.trim_start().trim_start_matches('❯').trim_start().starts_with("1. Yes"));
    has_question && has_yes
}

/// Drop terminal escape sequences (CSI such as colours, and OSC), keeping
/// the text. A capture taken with `-e` — the one the preview wants — puts
/// colour changes in the middle of the dialog's lines (`1. ` and `Yes` are
/// coloured separately), so matching happens on the stripped text.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then one final byte 0x40..=0x7e.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC \.
            Some(']') => {
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\x07' || (prev == '\x1b' && c == '\\') {
                        break;
                    }
                    prev = c;
                }
            }
            _ => {}
        }
    }
    out
}

/// `"tenx:@14.%40"` → `"%40"`: the pane id out of the registry's `tmux`
/// field. A pane id is a valid `send-keys`/`capture-pane` target on its own
/// and stays correct if the window is renumbered.
pub fn pane_id(tmux_field: &str) -> Option<String> {
    let pane = tmux_field.rsplit('.').next()?;
    (pane.starts_with('%') && pane[1..].chars().all(|c| c.is_ascii_digit())).then(|| pane.to_string())
}

/// The last `max` lines of a pane capture worth showing: trailing blank lines
/// dropped (a pane is padded to its height), then the tail.
pub fn preview_tail(capture: &str, max: usize) -> Vec<&str> {
    let mut lines: Vec<&str> = capture.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(max);
    lines.split_off(skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIALOG: &str = "\
 Bash command

   cargo build 2>&1 | tail -5
   Build the binary

 Do you want to proceed?
 ❯ 1. Yes
   2. No
 Esc to cancel · Tab to amend


";

    #[test]
    fn recognises_the_permission_dialog() {
        assert!(permission_dialog_visible(DIALOG));
        let three = DIALOG.replace("   2. No", "   2. Yes, and don't ask again for cargo build\n   3. No");
        assert!(permission_dialog_visible(&three));
    }

    /// The tail of a real `capture-pane -e` of the dialog: colours split
    /// the option number from its label.
    const COLOURED: &str = "\
 Do you want to proceed?
 \x1b[38;5;153m❯\x1b[39m \x1b[38;5;246m1. \x1b[38;5;153mYes\x1b[39m
   \x1b[38;5;246m2. \x1b[39mNo
 \x1b[38;5;246mEsc to cancel · Tab to amend\x1b[39m
";

    #[test]
    fn recognises_a_coloured_capture() {
        assert!(permission_dialog_visible(COLOURED));
        assert_eq!(strip_ansi("\x1b[1mx\x1b[0m\x1b]0;t\x07y"), "xy");
    }

    #[test]
    fn refuses_anything_else() {
        assert!(!permission_dialog_visible(""));
        assert!(!permission_dialog_visible("> \n\n  ? for shortcuts"));
        // An AskUserQuestion also numbers its options but asks something else.
        assert!(!permission_dialog_visible(" Which library?\n ❯ 1. Yes, chrono\n   2. time"));
        // The dialog scrolled away: question far above the tail.
        let gone = format!("{DIALOG}{}", "\n output line\n".repeat(20));
        assert!(!permission_dialog_visible(&gone));
    }

    #[test]
    fn extracts_the_pane_id() {
        assert_eq!(pane_id("tenx:@14.%40").as_deref(), Some("%40"));
        assert_eq!(pane_id("%3").as_deref(), Some("%3"));
        assert_eq!(pane_id("tenx:@14"), None);
        assert_eq!(pane_id(""), None);
    }

    #[test]
    fn preview_tail_drops_padding_and_keeps_the_end() {
        assert_eq!(preview_tail("a\nb\nc\n\n\n", 2), vec!["b", "c"]);
        assert_eq!(preview_tail("a\n", 5), vec!["a"]);
        assert!(preview_tail("\n\n", 5).is_empty());
    }

    #[test]
    fn answers_map_to_keys() {
        assert_eq!(Answer::Yes.key(), "Enter");
        assert_eq!(Answer::No.key(), "Escape");
    }
}
