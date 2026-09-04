//! The tenx colour palette — the single source of truth shared by both the
//! overlay TUI (ratatui) and the tmux chrome theme, so the whole surface
//! reads as one design: charcoal ground, purple accent, muted greys.
//!
//! Two consumers, two representations from the same values:
//! - the overlay calls [`Rgb::color`] to get a ratatui [`Color`];
//! - the generated tmux config (`tmux::render_config`) calls [`Rgb::hex`].
//!
//! Roles are semantic, not colour names — call sites say what a colour *means*
//! (`ACCENT`, `DANGER`), so the palette can shift without touching them.

use ratatui::style::Color;

/// An 8-bit-per-channel colour.
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// As a ratatui colour, for the overlay TUI.
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
/// Frames: pane borders, the popup border, the overlay's boxes. Quiet, so a
/// frame never competes with the content it holds; the active pane's frame
/// is one step lighter, not a colour.
pub const BORDER: Rgb = Rgb(52, 58, 70);
pub const BORDER_ACTIVE: Rgb = Rgb(96, 104, 120);
/// Chip backgrounds (label on a tinted pill): needs-input, current, secrets.
pub const CHIP_INPUT_BG: Rgb = Rgb(58, 46, 24);
pub const CHIP_CURRENT_BG: Rgb = Rgb(28, 40, 60);
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

/// The colour a task's status glyph is drawn in — one table for the overlay
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
