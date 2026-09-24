//! Whether Shift+Enter can reach an agent. The client asks its terminal for
//! the kitty keyboard protocol and records the answer (`kitty` or `legacy`,
//! then the terminal's name); without the protocol Shift+Enter arrives as a
//! bare Enter and submits the prompt. `tenx doctor` turns that record into
//! advice here.

/// The doctor's line for a recorded `@tenx_keyboard` value, `None` when no
/// client has recorded one.
pub fn doctor_line(recorded: Option<&str>) -> String {
    let Some(recorded) = recorded else {
        return "keyboard: unknown (no tenx client has attached since the server started)".to_string();
    };
    let (mode, terminal) = recorded.split_once(' ').unwrap_or((recorded, ""));
    let terminal = if terminal.is_empty() { "the terminal" } else { terminal };
    if mode == "kitty" {
        return format!("keyboard: kitty protocol in {terminal} ✓ (Shift+Enter inserts a newline)");
    }
    let fix = match terminal {
        "WezTerm" => "set config.enable_kitty_keyboard = true in ~/.wezterm.lua, then quit and reopen WezTerm",
        _ => "enable the kitty keyboard protocol in the terminal's settings, or use Ctrl+J for a newline",
    };
    format!("keyboard: {terminal} doesn't report Shift+Enter (it submits instead of adding a newline) — {fix}")
}

#[cfg(test)]
mod tests {
    use super::doctor_line;

    #[test]
    fn names_the_fix_for_the_terminal() {
        assert!(doctor_line(Some("kitty ghostty")).contains("kitty protocol in ghostty ✓"));
        assert!(doctor_line(Some("legacy WezTerm")).contains("enable_kitty_keyboard = true"));
        assert!(doctor_line(Some("legacy")).contains("the terminal doesn't report Shift+Enter"));
        assert!(doctor_line(None).contains("unknown"));
    }
}
