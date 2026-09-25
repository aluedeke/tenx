//! The tenx colour palette — the single source of truth shared by both the
//! column TUI (ratatui) and the tmux chrome theme, so the whole surface
//! reads as one design: charcoal ground, purple accent, muted greys.
//!
//! Two consumers, two representations from the same values:
//! - the column calls [`Rgb::color`] to get a ratatui [`Color`];
//! - the generated tmux config (`tmux::render_config`) calls [`Rgb::hex`].
//!
//! Roles are semantic, not colour names — call sites say what a colour *means*
//! (`ACCENT`, `DANGER`), so the palette can shift without touching them.

use ratatui::style::Color;

/// An 8-bit-per-channel colour.
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// As a ratatui colour, for the column TUI.
    pub const fn color(&self) -> Color {
        Color::Rgb(self.0, self.1, self.2)
    }

    /// As `#rrggbb`, for the generated tmux config (`fg=#a78bfa`).
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// Primary accent — selection, prompt, active highlights, rename mode. (purple)
pub const ACCENT: Rgb = Rgb(0xa7, 0x8b, 0xfa);
/// Muted secondary text — workspace column, ages, dividers.
pub const MUTED: Rgb = Rgb(120, 127, 140);
/// Body text — task names. Bright enough that a regular-weight glyph in a
/// light terminal font doesn't read as grey.
pub const TEXT: Rgb = Rgb(214, 219, 227);
/// Selected row: a background bar, not a colour swap, so the row keeps its
/// own status colours while selected.
pub const SEL_BG: Rgb = Rgb(33, 39, 52);
pub const SEL_TEXT: Rgb = Rgb(236, 239, 245);
/// Resting rows' glyph.
pub const IDLE: Rgb = Rgb(96, 104, 120);
/// Frames: pane borders, the popup border, the column's boxes. Quiet, so a
/// frame never competes with the content it holds; the active pane's frame
/// is one step lighter, not a colour.
pub const BORDER: Rgb = Rgb(52, 58, 70);
pub const BORDER_ACTIVE: Rgb = Rgb(96, 104, 120);
/// Chip backgrounds (label on a tinted pill): needs-input, secrets.
pub const CHIP_INPUT_BG: Rgb = Rgb(58, 46, 24);
pub const CHIP_SECRETS_BG: Rgb = Rgb(42, 36, 64);
/// "current" chip foreground.
pub const CURRENT: Rgb = Rgb(120, 160, 230);
/// Bright/primary text — done & blocked rows, key labels, titles.
pub const BRIGHT: Rgb = Rgb(0xe2, 0xe6, 0xee);
/// Failure / error / destructive.
pub const DANGER: Rgb = Rgb(0xe5, 0x70, 0x7b);
/// Success / done (green).
pub const SUCCESS: Rgb = Rgb(122, 194, 124);
/// Warning / needs input (amber).
pub const WARN: Rgb = Rgb(228, 168, 84);
/// Info / working (blue).
pub const INFO: Rgb = Rgb(104, 150, 220);
/// Ground — the charcoal background; also the foreground on colour-filled chips.
pub const GROUND: Rgb = Rgb(0x17, 0x18, 0x20);

/// The colour a task's status glyph is drawn in — one table for the column
/// (`.color()`) and the tmux status line (`.hex()`).
pub fn status_color(status: tenx_core::status::TaskStatus) -> &'static Rgb {
    use tenx_core::status::TaskStatus::*;
    match status {
        Blocked | Signaled => &WARN,
        Working => &INFO,
        Done => &SUCCESS,
        Idle => &IDLE,
    }
}

/// Workspace name colours — a set of their own, softer than the status hues
/// above so a workspace name tells projects apart without reading as a
/// status (an amber workspace would look like it needs input).
pub const WORKSPACE: [Rgb; 8] = [
    Rgb(94, 180, 170),  // teal
    Rgb(206, 134, 168), // rose
    Rgb(170, 176, 100), // olive
    Rgb(196, 168, 130), // sand
    Rgb(120, 180, 206), // sky
    Rgb(186, 146, 206), // orchid
    Rgb(214, 142, 116), // coral
    Rgb(140, 176, 138), // sage
];

/// The colour a workspace's name is drawn in, picked from its name so it is
/// the same in the column, the tmux status line, and across restarts.
/// FNV-1a rather than `std`'s hasher, whose output isn't promised stable.
pub fn workspace_color(name: &str) -> &'static Rgb {
    let hash = name.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    &WORKSPACE[hash as usize % WORKSPACE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_color_is_stable_per_name() {
        assert_eq!(workspace_color("acme").hex(), workspace_color("acme").hex());
        // Pinned, so a hasher change that would recolour everyone's workspaces fails here.
        assert_eq!(workspace_color("").hex(), WORKSPACE[0x811c_9dc5usize % WORKSPACE.len()].hex());
    }

    #[test]
    fn workspace_colors_spread_across_names() {
        let names = ["tenx", "acme", "infra", "web", "api", "mobile"];
        let distinct: std::collections::HashSet<String> = names.iter().map(|n| workspace_color(n).hex()).collect();
        assert!(distinct.len() >= 3, "{distinct:?}");
    }
}
