//! The tenx colour palette — the single source of truth shared by both the
//! overlay TUI (ratatui) and the zellij chrome theme, so the whole surface
//! reads as one design: charcoal ground, purple accent, muted greys.
//!
//! Two consumers, two representations from the same values:
//! - the overlay calls [`Rgb::color`] to get a ratatui [`Color`];
//! - the zellij theme (`zellij::theme_overlay`) calls [`Rgb::rgb`] to emit
//!   `"r g b"` triples for the theme KDL's semantic components.
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

    /// As `"r g b"` space-separated decimals, for zellij semantic theme
    /// components (`frame_selected { base R G B }`).
    pub fn rgb(&self) -> String {
        format!("{} {} {}", self.0, self.1, self.2)
    }
}

/// Primary accent — selection, prompt, active highlights, rename mode. (purple)
pub const ACCENT: Rgb = Rgb(0xa7, 0x8b, 0xfa);
/// Muted secondary text — dividers, hints, dim labels, frame borders.
pub const MUTED: Rgb = Rgb(0x6b, 0x72, 0x80);
/// Mid-weight body text — working / idle rows.
pub const TEXT: Rgb = Rgb(0xc8, 0xcc, 0xd4);
/// Bright/primary text — done & blocked rows, key labels, titles.
pub const BRIGHT: Rgb = Rgb(0xe2, 0xe6, 0xee);
/// Failure / error / destructive.
pub const DANGER: Rgb = Rgb(0xe5, 0x70, 0x7b);
/// Success / additions.
pub const SUCCESS: Rgb = Rgb(0x9c, 0xc3, 0x79);
/// Warning / workspace headers (amber).
pub const WARN: Rgb = Rgb(0xd6, 0xa1, 0x5b);
/// Info / links.
pub const INFO: Rgb = Rgb(0x7a, 0xa2, 0xf7);
/// Ground — the charcoal background; also the foreground on colour-filled chips.
pub const GROUND: Rgb = Rgb(0x17, 0x18, 0x20);
