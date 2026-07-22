//! tenx-zellij: the zellij adapter for the tenx overlay.
//!
//! This plugin is a *launcher*, not a UI: plugin panes have no PTY, so the
//! ratatui TUI can't render here. Instead the plugin runs in the background,
//! and on a `toggle` pipe message (bound to Ctrl+w) it opens the native
//! `tenx overlay` TUI in a floating **terminal** pane created at the
//! responsive size for the current screen — full-bleed on a phone, a centered
//! capped panel on a wide desktop. Because the size is computed *before* the
//! pane exists (from TabUpdate's display area), there is no measure-then-resize
//! race like the old `change-floating-pane-coordinates` approach had.

use std::collections::BTreeMap;
use zellij_tile::prelude::*;

// ── Responsive breakpoints (cells). At/under the phone thresholds the overlay
// fills the screen; past them it scales by percentage up to the desktop caps.
const PHONE_COLS: usize = 96;
const PHONE_ROWS: usize = 28;
const MAX_COLS: usize = 120;
const MAX_ROWS: usize = 46;

/// One axis: full-bleed at or below `full_below`, otherwise `pct` of the
/// available space clamped to [`full_below`, `max`].
fn responsive_dim(avail: usize, full_below: usize, pct: f32, max: usize) -> usize {
    if avail <= full_below {
        return avail;
    }
    let scaled = (avail as f32 * pct) as usize;
    scaled.clamp(full_below, max).min(avail)
}

/// Centered floating-pane coordinates for a `cols`×`rows` display area.
/// Values are passed as bare-integer strings (cells); the constructor parses
/// them into `PercentOrFixed::Fixed`.
///
/// Width is the device-class signal: a narrow screen means a phone (or a
/// squeezed window), so go full-bleed in BOTH axes — a portrait phone is
/// narrow but *tall*, and judging height on its own threshold would leave
/// vertical margins there forever. Only wide screens get the centered box.
fn geometry(cols: usize, rows: usize) -> Option<FloatingPaneCoordinates> {
    let (w, h) = if cols <= PHONE_COLS {
        (cols, rows)
    } else {
        (
            responsive_dim(cols, PHONE_COLS, 0.72, MAX_COLS),
            responsive_dim(rows, PHONE_ROWS, 0.85, MAX_ROWS),
        )
    };
    FloatingPaneCoordinates::new(
        Some(((cols - w) / 2).to_string()),
        Some(((rows - h) / 2).to_string()),
        Some(w.to_string()),
        Some(h.to_string()),
        Some(true), // pinned: stay on top
        None,
    )
}

/// Context marker attached to the command pane so we recognize our own pane in
/// CommandPaneOpened/Exited events.
const CTX_KEY: &str = "tenx";
const CTX_VAL: &str = "overlay";

#[derive(Default)]
struct State {
    /// Path to the native tenx binary (from plugin config or pipe args).
    tenx_bin: String,
    /// Latest display area of the active tab (cols, rows).
    display: Option<(usize, usize)>,
    /// Terminal pane ids of overlay panes we opened (normally 0 or 1).
    overlay_panes: Vec<u32>,
    /// A spawn is in flight (pane not yet reported open).
    spawning: bool,
    permissions_ok: bool,
    /// A toggle arrived before permissions/display were ready.
    pending_toggle: bool,
    /// We've hidden our own pane (only relevant when cold-launched with one).
    hidden: bool,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.tenx_bin = configuration
            .get("tenx_bin")
            .cloned()
            .unwrap_or_else(|| "tenx".into());
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ]);
        subscribe(&[
            EventType::TabUpdate,
            EventType::CommandPaneOpened,
            EventType::CommandPaneExited,
            EventType::PaneClosed,
            EventType::PermissionRequestResult,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                // Receiving any app-state event proves permissions are active —
                // a reliable hide trigger even when the permission grant was
                // cached (PermissionRequestResult may not fire then). When the
                // plugin is preloaded headless via `load_plugins` this is a
                // no-op; it only matters if we were cold-launched with a pane.
                if !self.hidden {
                    self.hidden = true;
                    self.permissions_ok = true;
                    hide_self();
                }
                if let Some(t) = tabs.iter().find(|t| t.active) {
                    let display = (t.display_area_columns, t.display_area_rows);
                    let changed = self.display != Some(display);
                    self.display = Some(display);
                    // Terminal window resized while the overlay is open (e.g. a
                    // phone rotation): re-fit the pane. Event-driven — zellij
                    // pushes TabUpdate on display changes, we never poll.
                    if changed && !self.overlay_panes.is_empty() {
                        if let Some(coords) = geometry(display.0, display.1) {
                            change_floating_panes_coordinates(
                                self.overlay_panes
                                    .iter()
                                    .map(|&id| (PaneId::Terminal(id), coords.clone()))
                                    .collect(),
                            );
                        }
                    }
                }
                self.run_pending();
            }
            Event::CommandPaneOpened(id, ctx) => {
                if ctx.get(CTX_KEY).map(String::as_str) == Some(CTX_VAL) {
                    if self.overlay_panes.contains(&id) {
                        // Re-announcement of a pane we already track (can
                        // happen around client attach) — not a duplicate.
                    } else if self.overlay_panes.is_empty() {
                        self.overlay_panes.push(id);
                    } else {
                        // Self-heal: we are the only legitimate spawner, so a
                        // second overlay pane is never valid (seen e.g. when an
                        // extra client is attached and the open applies twice).
                        close_terminal_pane(id);
                    }
                    self.spawning = false;
                }
            }
            // Keep bookkeeping honest if the overlay pane goes away through any
            // path that skips CommandPaneExited (client disconnects, manual
            // close): a stale id would make the next toggle a silent no-op.
            Event::PaneClosed(PaneId::Terminal(id)) => {
                self.overlay_panes.retain(|&p| p != id);
            }
            Event::CommandPaneExited(id, _status, ctx) => {
                if ctx.get(CTX_KEY).map(String::as_str) == Some(CTX_VAL) {
                    // The TUI process ended (jump/quit) — remove the pane so it
                    // doesn't linger with zellij's "exited" hold banner.
                    close_terminal_pane(id);
                    self.overlay_panes.retain(|&p| p != id);
                    self.spawning = false;
                }
            }
            Event::PermissionRequestResult(status) => {
                self.permissions_ok = matches!(status, PermissionStatus::Granted);
                // Fresh first-run grant: safe to hide now that the prompt is
                // answered (hiding earlier would hide the prompt itself).
                if self.permissions_ok && !self.hidden {
                    self.hidden = true;
                    hide_self();
                }
                self.run_pending();
            }
            _ => {}
        }
        false
    }

    fn pipe(&mut self, msg: PipeMessage) -> bool {
        if msg.name == "toggle" {
            if let Some(bin) = msg.args.get("tenx_bin") {
                self.tenx_bin = bin.clone();
            }
            if self.ready() {
                self.toggle();
            } else {
                // First message often arrives before the permission grant and
                // the first TabUpdate; run as soon as both are in.
                self.pending_toggle = true;
            }
        }
        false
    }

    // Only shown if the plugin ever gets a visible pane (it normally doesn't).
    fn render(&mut self, _rows: usize, _cols: usize) {
        println!("tenx-zellij launcher — Ctrl+w toggles the tenx overlay");
    }
}

impl State {
    fn ready(&self) -> bool {
        self.permissions_ok && self.display.is_some()
    }

    fn run_pending(&mut self) {
        if self.pending_toggle && self.ready() {
            self.pending_toggle = false;
            self.toggle();
        }
    }

    fn toggle(&mut self) {
        // Open → close (toggle off).
        if !self.overlay_panes.is_empty() {
            for id in self.overlay_panes.drain(..) {
                close_terminal_pane(id);
            }
            return;
        }
        if self.spawning {
            return; // double-tap while the pane is still opening
        }
        let Some((cols, rows)) = self.display else {
            return;
        };
        self.spawning = true;
        let mut ctx = BTreeMap::new();
        ctx.insert(CTX_KEY.to_string(), CTX_VAL.to_string());
        let cmd = CommandToRun {
            path: self.tenx_bin.clone().into(),
            args: vec!["overlay".into()],
            cwd: None,
        };
        open_command_pane_floating(cmd, geometry(cols, rows), ctx);
    }
}
