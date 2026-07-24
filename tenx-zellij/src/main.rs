//! tenx-zellij: the tenx task overlay, rendered natively as a zellij plugin.
//!
//! ## Why this is a plugin, not a launcher
//!
//! The overlay is a per-*client* thing ("show *me* the switcher, over the tab
//! *I'm* looking at") but zellij has no per-client pane object to build that
//! from — panes are session-global, tab focus is per-client. The old design
//! (a background plugin that spawned a floating *terminal* pane running the
//! ratatui overlay) had to reconcile that mismatch by hand, and lost: zellij
//! runs one plugin instance per (plugin, client), so each attached client got
//! its own launcher with its own pane bookkeeping, and they stacked duplicate
//! overlays that no single instance could see to clean up.
//!
//! A plugin *pane* dissolves the problem. `LaunchOrFocusPlugin { move_to_
//! focused_tab: true }` keeps exactly one session-wide pane and *moves* it to
//! the summoning client's current tab — the singleton is enforced by zellij,
//! and every key/mouse event this instance receives is already attributed to
//! the client who summoned it. Jumping via the host API (`go_to_tab_name`) is
//! therefore correct for phone and desktop alike, with no "last-active-client"
//! guessing (the bug that forced Enter-only jumps in the terminal overlay).
//!
//! ## Where the data lives
//!
//! Plugins run in a wasm sandbox with no filesystem access, so task discovery
//! stays in the native binary: `tenx overlay --json` (run via `run_command`,
//! results delivered as `RunCommandResult`) is the single source of truth,
//! polled on a timer and refreshed after any mutating action. Jumps to a
//! not-yet-open task shell out to `tenx task open --ws-dir <dir> <slug>`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use serde::Deserialize;
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

// ── Palette (truecolor; zellij's pane renderer supports it) ─────────────────
const C_BORDER: Color = Color::Rgb(52, 58, 70);
const C_TEXT: Color = Color::Rgb(214, 219, 227); // task names
const C_DIM: Color = Color::Rgb(120, 127, 140); // workspace, times
const C_FAINT: Color = Color::Rgb(88, 94, 106); // column headers, hints
const C_SEL_BG: Color = Color::Rgb(33, 39, 52);
const C_SEL_TEXT: Color = Color::Rgb(236, 239, 245);
// Status accent colors (icon + badges).
const C_WORKING: Color = Color::Rgb(104, 150, 220); // blue — a turn in flight
const C_BLOCKED: Color = Color::Rgb(228, 168, 84); // amber — needs input
const C_DONE: Color = Color::Rgb(122, 194, 124); // green — finished
const C_FAILED: Color = Color::Rgb(226, 96, 92); // red — errored
const C_IDLE: Color = Color::Rgb(96, 104, 120); // muted — resting
// Badge chip backgrounds (dim, so the accent text reads on top).
const C_BADGE_INPUT_BG: Color = Color::Rgb(58, 46, 24);
const C_CURRENT_FG: Color = Color::Rgb(120, 160, 230);
const C_CURRENT_BG: Color = Color::Rgb(28, 40, 60);
const C_TOGGLE_ON_BG: Color = Color::Rgb(48, 56, 72);

/// One task row, deserialized from `tenx overlay --json`.
#[derive(Debug, Clone, Deserialize)]
struct Task {
    ws: String,
    ws_dir: String,
    slug: String,
    title: String,
    status: String,
    age_secs: Option<u64>,
}

#[derive(Deserialize)]
struct TaskDump {
    tasks: Vec<Task>,
}

/// Context marker on our `run_command` calls so we can tell the data refresh
/// apart from a fire-and-forget mutation in `RunCommandResult`.
const CTX_KIND: &str = "kind";
const KIND_TASKS: &str = "tasks";
/// Any mutation (create/rename/delete/open) uses this marker; on its result we
/// re-read task data so the list reflects the change.
const KIND_MUTATE: &str = "mutate";

/// Poll interval for re-reading task state (status glyphs, ages, open flags).
const POLL_SECS: f64 = 1.5;
/// Spinner frame interval when at least one task is working (animation on).
const ANIM_SECS: f64 = 0.11;
/// In animated mode, poll data once every N frames (≈ POLL_SECS worth).
const POLL_EVERY: usize = (POLL_SECS / ANIM_SECS) as usize;
/// Braille spinner frames for the "working" status icon.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ── Responsive pane geometry ── At/under the phone thresholds the overlay
// fills the whole screen; past them it scales by percentage up to a desktop cap
// and centers. The plugin sizes ITS OWN floating pane (LaunchOrFocusPlugin only
// gives a default fraction of the terminal, which is cramped on a phone).
const PHONE_COLS: usize = 96;
const PHONE_ROWS: usize = 28;
const MAX_COLS: usize = 120;
const MAX_ROWS: usize = 46;

fn responsive_dim(avail: usize, full_below: usize, pct: f32, max: usize) -> usize {
    if avail <= full_below {
        return avail;
    }
    ((avail as f32 * pct) as usize).clamp(full_below, max).min(avail)
}

/// Target pane size for a display area: full-bleed on a narrow (phone) screen,
/// a centered capped panel on desktop.
fn geometry_wh(cols: usize, rows: usize) -> (usize, usize) {
    if cols <= PHONE_COLS {
        (cols, rows)
    } else {
        (
            responsive_dim(cols, PHONE_COLS, 0.72, MAX_COLS),
            responsive_dim(rows, PHONE_ROWS, 0.85, MAX_ROWS),
        )
    }
}

/// Centered floating-pane coordinates for a `cols`×`rows` display area.
fn geometry(cols: usize, rows: usize) -> Option<FloatingPaneCoordinates> {
    let (w, h) = geometry_wh(cols, rows);
    FloatingPaneCoordinates::new(
        Some((cols.saturating_sub(w) / 2).to_string()),
        Some((rows.saturating_sub(h) / 2).to_string()),
        Some(w.to_string()),
        Some(h.to_string()),
        Some(true), // pinned: stay on top
        None,
    )
}

/// What the overlay is doing: the list, or a modal capturing text / a y/n.
#[derive(Default)]
enum Mode {
    #[default]
    List,
    /// Typing a new task name; created in `ws_dir` (of the selected task).
    Create { name: String, ws: String, ws_dir: String },
    /// Editing the selected task's title; buffer pre-filled with the old one.
    Rename { buffer: String, slug: String, ws_dir: String },
    /// Confirming deletion of the selected task.
    ConfirmDelete { title: String, slug: String, ws_dir: String },
}

/// How the list is organised: one flat activity-sorted section, or grouped by
/// workspace. Toggled with → (the header's segmented control mirrors it).
#[derive(Default, Clone, Copy, PartialEq)]
enum Grouping {
    #[default]
    Recent,
    Workspace,
}

/// A rendered list line: a section header, or a task (position in `filtered`).
enum Disp {
    Header(String),
    Task(usize),
}

#[derive(Default)]
struct State {
    /// Absolute path to the native tenx binary (from plugin config).
    tenx_bin: String,
    tasks: Vec<Task>,
    /// Indices into `tasks` matching the current filter, in display order.
    filtered: Vec<usize>,
    /// Selected position within `filtered`.
    selected: usize,
    /// Identity (ws_dir, slug) of the selected task, so the highlight follows
    /// the task across re-sorts (the list re-orders by activity every poll —
    /// tracking by position alone would open a different task on Enter).
    selected_key: Option<(String, String)>,
    filter: String,
    grouping: Grouping,
    mode: Mode,
    /// Name of the currently-focused tab (== a task slug) for the "current"
    /// badge.
    active_tab: Option<String>,
    /// Names of live tabs (== task slugs) in the session right now, from
    /// TabUpdate. Jump matches a task's slug against these — the reliable key
    /// (slugs don't drift like titles or collide like the reused tab ids).
    live_tabs: Vec<String>,
    /// Number of distinct workspaces across all tasks (footer summary).
    workspace_count: usize,
    /// Transient one-line message (e.g. an error from a mutation).
    message: Option<String>,
    permissions_ok: bool,
    /// True while our pane is visible; we only poll/render when shown.
    visible: bool,
    /// A data refresh is in flight (avoid piling up run_commands).
    loading: bool,
    /// A `set_timeout` is pending (single-chain guard, so overlapping triggers
    /// don't spawn parallel tick loops that double the frame rate).
    ticking: bool,
    /// Whether pane chrome (rename + borderless) has been applied. Deferred
    /// past load() because those commands need a permission granted async.
    applied_chrome: bool,
    /// Display area (cols, rows) the pane was last sized for — resize only when
    /// it actually changes (e.g. a phone rotation), not on every tab switch.
    sized_for: Option<(usize, usize)>,
    /// A render-time re-fit has been issued and we're waiting for the pane to
    /// reach target size — prevents re-issuing every frame (no resize loop).
    refit_pending: bool,
    /// The overlay was just (re)opened: the next render snapshots fresh
    /// activity order and resets the selection. Starts true so the first render
    /// freezes an order.
    reopen_pending: bool,
    /// Frozen display order (task keys) — the rows never re-sort while the
    /// overlay is open; only a (re)open takes a new snapshot (`freeze_order`).
    order: Vec<(String, String)>,
    /// Spinner frame counter (advances each animation tick).
    frame: usize,
    /// Screen row of the first list line, set during draw (click map).
    list_origin: usize,
    /// Display-line scroll offset, set during draw.
    scroll: usize,
    /// Per-rendered-line → filtered position (None for headers/blanks).
    line_map: Vec<Option<usize>>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.tenx_bin = configuration
            .get("tenx_bin")
            .cloned()
            .unwrap_or_else(|| "tenx".into());
        request_permission(&[
            PermissionType::RunCommands,
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[
            EventType::Key,
            EventType::Mouse,
            EventType::RunCommandResult,
            EventType::Timer,
            EventType::Visible,
            EventType::TabUpdate,
            EventType::PermissionRequestResult,
        ]);
        // We're created because we're being shown; assume visible until a
        // Visible(false) says otherwise (zellij doesn't reliably emit an
        // initial Visible(true)). Freeze an order on the first render.
        self.visible = true;
        self.reopen_pending = true;
        // NB: pane chrome (rename + borderless) needs ChangeApplicationState,
        // which is granted ASYNC after load — calling it here gets denied. It's
        // applied in `apply_chrome` once a permission-bearing event arrives.
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(status) => {
                self.permissions_ok = matches!(status, PermissionStatus::Granted);
                if self.permissions_ok {
                    self.apply_chrome();
                    self.refresh();
                    self.ensure_tick();
                }
                true
            }
            Event::Visible(vis) => {
                self.visible = vis;
                if vis {
                    // Reappearing over a new tab (summoned again) — reload now
                    // so the list is fresh, restart the tick loop, and re-apply
                    // our geometry (the re-home resets the pane to default size).
                    self.refresh();
                    self.ensure_tick();
                    self.refit();
                }
                true
            }
            Event::Timer(_) => {
                self.ticking = false; // this scheduled tick has now fired
                if !(self.visible && self.permissions_ok) {
                    return false; // hidden → let the tick loop lapse
                }
                self.frame = self.frame.wrapping_add(1);
                // Animate at ~9fps while any task is working; otherwise fall
                // back to a plain 1.5s data poll (no wasted renders when idle).
                let working = self.tasks.iter().any(|t| t.status == "working");
                if working {
                    if self.frame % POLL_EVERY == 0 {
                        self.refresh();
                    }
                    self.schedule(ANIM_SECS);
                    true // re-render to advance the spinner
                } else {
                    self.refresh();
                    self.schedule(POLL_SECS);
                    false
                }
            }
            Event::RunCommandResult(exit, stdout, stderr, ctx) => {
                match ctx.get(CTX_KIND).map(String::as_str) {
                    Some(KIND_TASKS) => {
                        self.loading = false;
                        if let Ok(dump) = serde_json::from_slice::<TaskDump>(&stdout) {
                            // Always take the fresh data verbatim; the DISPLAY
                            // order is frozen separately (self.order), so a poll
                            // never reshuffles rows under the cursor/finger.
                            self.tasks = dump.tasks;
                            self.apply_filter();
                        }
                        true
                    }
                    Some(KIND_MUTATE) => {
                        // A create/rename/delete finished — surface any error,
                        // then reload so the list reflects the change.
                        if exit != Some(0) {
                            let msg = String::from_utf8_lossy(&stderr);
                            let msg = msg.trim();
                            self.message =
                                Some(if msg.is_empty() { "command failed".into() } else { msg.into() });
                        }
                        self.refresh();
                        true
                    }
                    _ => false,
                }
            }
            Event::TabUpdate(tabs) => {
                // Receiving an app-state event proves permissions are active —
                // a reliable loop-starter even when the grant was cached and
                // PermissionRequestResult never fires.
                self.permissions_ok = true;
                self.apply_chrome();
                self.ensure_tick();
                // Size our floating pane to the active tab's display area
                // (full-bleed on a phone). Also track live tab names (jump
                // open-vs-create) and the focused tab (the "current" badge).
                if let Some(t) = tabs.iter().find(|t| t.active) {
                    self.fit_pane(t.display_area_columns, t.display_area_rows);
                }
                self.live_tabs = tabs.iter().map(|t| t.name.clone()).collect();
                let active = tabs.iter().find(|t| t.active).map(|t| t.name.clone());
                let changed = active != self.active_tab;
                self.active_tab = active;
                changed
            }
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(m) => self.handle_mouse(m),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        // First render after a (re)open: freeze the display order NOW (before
        // the user can act) and select the top, so what they see is stable and
        // no async poll can reshuffle it under a tap.
        if self.reopen_pending && !self.tasks.is_empty() {
            self.reopen_pending = false;
            self.freeze_order();
            self.selected = 0;
            self.selected_key = None;
            self.apply_filter();
        }
        // Self-correct the pane size. On a re-summon, LaunchOrFocusPlugin
        // re-homes the pane and zellij resets it to the default (small) size —
        // and no Visible/changed-TabUpdate event fires to tell us. So whenever
        // we render notably smaller than the responsive target, re-apply the
        // geometry. Converges in a frame; the tolerance avoids border-off-by-one
        // churn at steady state.
        if let Some((dc, dr)) = self.sized_for {
            let (w, h) = geometry_wh(dc, dr);
            let too_small = cols + 3 < w || rows + 3 < h;
            if too_small && !self.refit_pending {
                self.refit_pending = true; // issue once; wait for it to take
                self.apply_geometry(dc, dr);
            } else if !too_small {
                self.refit_pending = false; // reached target — ready for next
            }
        }
        let ansi = self.draw(rows, cols);
        print!("{ansi}");
    }
}

impl State {
    /// Snapshot the current activity order into the frozen display order. Taken
    /// synchronously when the overlay (re)opens, so the rows the user sees stay
    /// put while they choose — background polls refresh data but never reorder.
    fn freeze_order(&mut self) {
        self.order = self
            .tasks
            .iter()
            .map(|t| (t.ws_dir.clone(), t.slug.clone()))
            .collect();
    }

    /// Kick off a task-data reload (unless one is already running).
    fn refresh(&mut self) {
        if self.loading {
            return;
        }
        self.loading = true;
        let mut ctx = BTreeMap::new();
        ctx.insert(CTX_KIND.to_string(), KIND_TASKS.to_string());
        run_command(&[&self.tenx_bin, "overlay", "--json"], ctx);
    }

    /// Apply pane chrome once a permission-bearing event confirms the grant is
    /// active (rename the pane, suppress zellij's own floating frame). Idempotent.
    fn apply_chrome(&mut self) {
        if self.applied_chrome {
            return;
        }
        self.applied_chrome = true;
        let id = get_plugin_ids().plugin_id;
        rename_plugin_pane(id, "tenx");
        set_pane_borderless(PaneId::Plugin(id), true);
    }

    /// Fit our floating pane to the current display area (from TabUpdate).
    /// Only re-applies when the display actually changed (e.g. phone rotation),
    /// so ordinary tab switches don't churn.
    fn fit_pane(&mut self, cols: usize, rows: usize) {
        if cols == 0 || rows == 0 {
            return;
        }
        let changed = self.sized_for != Some((cols, rows));
        self.sized_for = Some((cols, rows));
        if changed {
            self.apply_geometry(cols, rows);
        }
    }

    /// Re-apply the last-known geometry. Needed when the overlay is re-shown:
    /// `LaunchOrFocusPlugin { move_to_focused_tab }` re-homes the pane and
    /// zellij resets it to the default (small) floating size, but the display
    /// area is unchanged — so `fit_pane`'s guard would skip. Without this the
    /// first open is correctly sized but every re-open is default-sized.
    fn refit(&mut self) {
        if let Some((c, r)) = self.sized_for {
            self.apply_geometry(c, r);
        }
    }

    /// Set our floating pane to the responsive geometry for a display area:
    /// full-bleed on a phone, centered-capped on desktop.
    fn apply_geometry(&self, cols: usize, rows: usize) {
        if let Some(coords) = geometry(cols, rows) {
            let id = get_plugin_ids().plugin_id;
            change_floating_panes_coordinates(vec![(PaneId::Plugin(id), coords)]);
        }
    }

    /// Start the tick loop if it isn't already running. Idempotent: multiple
    /// triggers (permission grant, visible, tab update) converge on one chain.
    fn ensure_tick(&mut self) {
        if !self.ticking {
            self.schedule(ANIM_SECS);
        }
    }

    /// Schedule the next tick, marking a timeout pending.
    fn schedule(&mut self, secs: f64) {
        self.ticking = true;
        set_timeout(secs);
    }

    /// Hide the overlay and stop the tick loop (it lapses on the next fire
    /// because `visible` is now false). Arm a re-sort so the next open starts
    /// from fresh activity order at the top.
    fn hide(&mut self) {
        self.visible = false;
        self.reopen_pending = true;
        hide_self();
    }

    /// Recompute `filtered` from `filter` + `grouping`, clamping the selection.
    /// Data arrives already activity-sorted from `tenx overlay --json`; Recent
    /// keeps that order, Workspace re-buckets by workspace (stable, so within a
    /// bucket the activity order is preserved). `filtered` is always in final
    /// display order, so navigation can walk it linearly.
    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        let mut filtered: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                needle.is_empty()
                    || subseq_match(&needle, &format!("{} {}", t.ws, t.title).to_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        // Order by the frozen snapshot (stable while open); tasks not in the
        // snapshot yet (created since the last open) fall to the end.
        let rank = |i: usize, s: &Self| {
            let k = (s.tasks[i].ws_dir.clone(), s.tasks[i].slug.clone());
            s.order.iter().position(|o| *o == k).unwrap_or(usize::MAX)
        };
        filtered.sort_by_key(|&i| rank(i, self));
        if self.grouping == Grouping::Workspace {
            // Stable re-bucket by workspace; frozen order retained within each.
            filtered.sort_by(|&a, &b| self.tasks[a].ws.cmp(&self.tasks[b].ws));
        }
        self.filtered = filtered;

        let mut seen: Vec<&str> = self.tasks.iter().map(|t| t.ws.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        self.workspace_count = seen.len();

        // Keep the highlight on the SAME task across re-sorts/filters. If it's
        // gone (deleted or filtered out), clamp to a valid position.
        self.selected = self
            .selected_key
            .as_ref()
            .and_then(|(wd, sl)| {
                self.filtered
                    .iter()
                    .position(|&i| &self.tasks[i].ws_dir == wd && &self.tasks[i].slug == sl)
            })
            .unwrap_or_else(|| self.selected.min(self.filtered.len().saturating_sub(1)));
        self.sync_key();
    }

    /// Record the selected task's identity so it can be re-found after the list
    /// re-sorts. Call after any change to `selected`.
    fn sync_key(&mut self) {
        self.selected_key = self
            .selected_task()
            .map(|t| (t.ws_dir.clone(), t.slug.clone()));
    }

    /// Build the interleaved display rows (section headers + tasks) for the
    /// current grouping. Recent → a single "RECENT" header; Workspace → one
    /// header per workspace bucket.
    fn display_rows(&self) -> Vec<Disp> {
        let mut out = Vec::new();
        let mut last_ws: Option<&str> = None;
        for (pos, &ti) in self.filtered.iter().enumerate() {
            match self.grouping {
                Grouping::Recent => {
                    if pos == 0 {
                        out.push(Disp::Header("RECENT".into()));
                    }
                }
                Grouping::Workspace => {
                    let ws = self.tasks[ti].ws.as_str();
                    if last_ws != Some(ws) {
                        out.push(Disp::Header(ws.to_uppercase()));
                        last_ws = Some(ws);
                    }
                }
            }
            out.push(Disp::Task(pos));
        }
        out
    }

    fn selected_task(&self) -> Option<&Task> {
        self.filtered.get(self.selected).and_then(|&i| self.tasks.get(i))
    }

    fn move_sel(&mut self, delta: isize) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let cur = self.selected as isize;
        self.selected = (cur + delta).rem_euclid(n as isize) as usize;
        self.sync_key();
    }

    /// Jump to the selected task's tab. If it's already open, use the host API
    /// (correctly attributed to us, the summoning client). If not, create it
    /// via the native binary. Either way, hide afterwards so the overlay gets
    /// out of the way — a re-summon reveals it over the new tab.
    fn jump(&mut self) -> bool {
        let Some(task) = self.selected_task().cloned() else {
            return false;
        };
        // Correlate task → tab by the SLUG (zellij tabs are named by slug).
        // Slugs are immutable and unique, so this never drifts (unlike the
        // title) or collides (unlike the reused numeric tab id). If a live tab
        // matches, switch via the host API (correct per-client attribution, so
        // taps work); otherwise the native binary creates it.
        if self.live_tabs.iter().any(|n| n == &task.slug) {
            go_to_tab_name(&task.slug);
        } else {
            self.run_mutation(&["task", "open", "--ws-dir", &task.ws_dir, &task.slug]);
        }
        self.hide();
        false
    }

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        self.message = None;
        match &self.mode {
            Mode::List => self.handle_list_key(key),
            Mode::Create { .. } => self.handle_create_key(key),
            Mode::Rename { .. } => self.handle_rename_key(key),
            Mode::ConfirmDelete { .. } => self.handle_confirm_key(key),
        }
    }

    /// List mode: plain chars filter, so all actions live on Ctrl. Ctrl-n/p
    /// move (emacs-style, alongside arrows); Ctrl-a/r/d add/rename/delete.
    fn handle_list_key(&mut self, key: KeyWithModifier) -> bool {
        let ctrl = key.has_modifiers(&[KeyModifier::Ctrl]);
        match key.bare_key {
            BareKey::Esc if key.has_no_modifiers() => {
                if self.filter.is_empty() {
                    self.hide();
                } else {
                    self.filter.clear();
                    self.apply_filter();
                }
            }
            BareKey::Enter if key.has_no_modifiers() => return self.jump(),
            BareKey::Down => self.move_sel(1),
            BareKey::Up => self.move_sel(-1),
            BareKey::Char('n') if ctrl => self.move_sel(1),
            BareKey::Char('p') if ctrl => self.move_sel(-1),
            // → / ← toggle grouping (Recent ⇄ Workspace); Tab also cycles.
            BareKey::Right | BareKey::Left | BareKey::Tab if key.has_no_modifiers() => {
                self.grouping = match self.grouping {
                    Grouping::Recent => Grouping::Workspace,
                    Grouping::Workspace => Grouping::Recent,
                };
                self.apply_filter();
            }
            BareKey::Char('a') if ctrl => self.start_create(),
            BareKey::Char('r') if ctrl => self.start_rename(),
            BareKey::Char('d') if ctrl => self.start_delete(),
            BareKey::Backspace if key.has_no_modifiers() => {
                self.filter.pop();
                self.apply_filter();
            }
            BareKey::Char(c) if key.has_no_modifiers() => {
                self.filter.push(c);
                self.apply_filter();
            }
            _ => return false,
        }
        true
    }

    fn handle_create_key(&mut self, key: KeyWithModifier) -> bool {
        let Mode::Create { name, ws_dir, .. } = &mut self.mode else {
            return false;
        };
        match key.bare_key {
            BareKey::Esc => self.mode = Mode::List,
            BareKey::Enter => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    self.mode = Mode::List;
                } else {
                    let ws_dir = ws_dir.clone();
                    self.run_mutation(&["task", "new", "--ws-dir", &ws_dir, &name]);
                    self.mode = Mode::List;
                    // The new task's tab opens itself (new_in with no_open=false),
                    // so get out of the way like a jump does.
                    self.hide();
                }
            }
            BareKey::Backspace => {
                name.pop();
            }
            BareKey::Char(c) if key.has_no_modifiers() => name.push(c),
            _ => return false,
        }
        true
    }

    fn handle_rename_key(&mut self, key: KeyWithModifier) -> bool {
        let Mode::Rename { buffer, slug, ws_dir } = &mut self.mode else {
            return false;
        };
        match key.bare_key {
            BareKey::Esc => self.mode = Mode::List,
            BareKey::Enter => {
                let title = buffer.trim().to_string();
                if !title.is_empty() {
                    let (slug, ws_dir) = (slug.clone(), ws_dir.clone());
                    self.run_mutation(&["task", "rename", "--ws-dir", &ws_dir, &slug, &title]);
                }
                self.mode = Mode::List;
            }
            BareKey::Backspace => {
                buffer.pop();
            }
            BareKey::Char(c) if key.has_no_modifiers() => buffer.push(c),
            _ => return false,
        }
        true
    }

    fn handle_confirm_key(&mut self, key: KeyWithModifier) -> bool {
        let Mode::ConfirmDelete { slug, ws_dir, .. } = &self.mode else {
            return false;
        };
        match key.bare_key {
            BareKey::Char('y') | BareKey::Char('Y') => {
                let (slug, ws_dir) = (slug.clone(), ws_dir.clone());
                self.run_mutation(&["task", "rm", "--force", "--ws-dir", &ws_dir, &slug]);
                self.mode = Mode::List;
            }
            _ => self.mode = Mode::List, // any other key cancels
        }
        true
    }

    fn start_create(&mut self) {
        // Create in the selected task's workspace (the data source only lists
        // workspaces that already have tasks). With an empty list we have no
        // workspace to target.
        match self.selected_task() {
            Some(t) => {
                self.mode = Mode::Create {
                    name: String::new(),
                    ws: t.ws.clone(),
                    ws_dir: t.ws_dir.clone(),
                }
            }
            None => self.message = Some("no workspace — select a task first".into()),
        }
    }

    fn start_rename(&mut self) {
        if let Some(t) = self.selected_task() {
            self.mode = Mode::Rename {
                buffer: t.title.clone(),
                slug: t.slug.clone(),
                ws_dir: t.ws_dir.clone(),
            };
        }
    }

    fn start_delete(&mut self) {
        if let Some(t) = self.selected_task() {
            self.mode = Mode::ConfirmDelete {
                title: t.title.clone(),
                slug: t.slug.clone(),
                ws_dir: t.ws_dir.clone(),
            };
        }
    }

    /// Run a tenx mutation subcommand, marked so its result triggers a reload.
    /// Env is injected so the subprocess (a child of the zellij server, with
    /// no zellij vars) can still drive the session for tab open/rename.
    fn run_mutation(&self, args: &[&str]) {
        let mut env = BTreeMap::new();
        env.insert("ZELLIJ".to_string(), "0".to_string());
        env.insert("ZELLIJ_SESSION_NAME".to_string(), "tenx".to_string());
        let mut ctx = BTreeMap::new();
        ctx.insert(CTX_KIND.to_string(), KIND_MUTATE.to_string());
        let mut full: Vec<&str> = vec![&self.tenx_bin];
        full.extend_from_slice(args);
        run_command_with_env_variables_and_cwd(
            &full,
            env,
            std::path::PathBuf::from("."),
            ctx,
        );
    }

    fn handle_mouse(&mut self, m: Mouse) -> bool {
        // Mouse only drives the list; modal prompts are keyboard-only.
        if !matches!(self.mode, Mode::List) {
            return false;
        }
        match m {
            Mouse::ScrollDown(_) => self.move_sel(1),
            Mouse::ScrollUp(_) => self.move_sel(-1),
            // Tap a row → select it and jump. Mouse events reach *this* client's
            // plugin instance, so the resulting jump is correctly attributed —
            // the phone tap switches the phone's tab, unlike CLI `go-to-tab`.
            Mouse::LeftClick(line, _col) => {
                if let Some(pos) = self.row_at(line) {
                    self.selected = pos;
                    self.sync_key();
                    return self.jump();
                }
                return false;
            }
            _ => return false,
        }
        true
    }

    /// Map a clicked terminal line to a filtered position via the per-line map
    /// captured on the last draw (None over headers/blanks).
    fn row_at(&self, line: isize) -> Option<usize> {
        let line = usize::try_from(line).ok()?;
        let idx = line.checked_sub(self.list_origin)?;
        self.line_map.get(idx).copied().flatten()
    }

    /// Render the overlay into a ratatui `Buffer`, then serialize it to ANSI
    /// for the plugin pane. Layout mirrors a command-palette: header (help +
    /// grouping toggle), column titles, grouped task rows, footer summary.
    fn draw(&mut self, rows: usize, cols: usize) -> String {
        let area = Rect::new(0, 0, cols as u16, rows as u16);
        let mut buf = Buffer::empty(area);
        self.line_map.clear();

        // Our own rounded panel (zellij's native frame is suppressed in load).
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(C_BORDER));
        let inner = block.inner(area);
        block.render(area, &mut buf);
        if inner.width < 8 || inner.height < 6 {
            return buf_to_ansi(&buf);
        }

        let pad = 1u16;
        let cx = inner.x + pad;
        let cw = inner.width - pad * 2;
        let right = cx + cw;

        // ── Responsive column geometry (shared by titles + rows) ──
        // Drop columns as width shrinks: workspace first, then the age header,
        // then the long "done … ago" form → bare age, so a phone still reads.
        let show_ws = cw >= 46;
        let show_age = cw >= 26;
        let age_full = cw >= 62; // "done 30m ago" vs "30m"
        let age_w: u16 = if show_age {
            if age_full { 12 } else { 5 }
        } else {
            0
        };
        let name_x = cx + 2;
        let age_x = right.saturating_sub(age_w);
        let right_edge = if show_age { age_x.saturating_sub(2) } else { right };
        let ws_w: u16 = if show_ws { (cw / 5).clamp(8, 16) } else { 0 };
        let name_w = right_edge
            .saturating_sub(if show_ws { ws_w + 2 } else { 0 })
            .saturating_sub(name_x)
            .max(6);
        let ws_x = name_x + name_w + 2;

        // ── Header row: help/filter (left) + grouping toggle & esc (right) ──
        let esc = "esc";
        put(&mut buf, right - esc.len() as u16, inner.y, esc.len(), esc, Style::default().fg(C_FAINT));
        let mut left_limit = right - esc.len() as u16;
        // Toggle only when there's room for it (phones skip it; → still cycles).
        if matches!(self.mode, Mode::List) && cw >= 56 {
            let (rl, wl) = (" recent ", " workspace ");
            let wx = left_limit - 1 - wl.len() as u16;
            let rx = wx - rl.len() as u16;
            let on = Style::default().fg(C_SEL_TEXT).bg(C_TOGGLE_ON_BG).add_modifier(Modifier::BOLD);
            let off = Style::default().fg(C_FAINT);
            put(&mut buf, rx, inner.y, rl.len(), rl, if self.grouping == Grouping::Recent { on } else { off });
            put(&mut buf, wx, inner.y, wl.len(), wl, if self.grouping == Grouping::Workspace { on } else { off });
            left_limit = rx;
        }
        let (htext, hstyle) = match &self.mode {
            Mode::List if self.filter.is_empty() && cw >= 60 => (
                "⚲ switch task — type to filter, ↑↓ move, ↵ jump".to_string(),
                Style::default().fg(C_DIM),
            ),
            Mode::List if self.filter.is_empty() => {
                ("⚲ switch task".to_string(), Style::default().fg(C_DIM))
            }
            Mode::List => (
                format!("⚲ {}", self.filter),
                Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
            ),
            Mode::Create { name, ws, .. } => (
                format!("＋ new in {ws} — {name}"),
                Style::default().fg(C_WORKING).add_modifier(Modifier::BOLD),
            ),
            Mode::Rename { buffer, .. } => (
                format!("✎ rename — {buffer}"),
                Style::default().fg(C_WORKING).add_modifier(Modifier::BOLD),
            ),
            Mode::ConfirmDelete { title, .. } => (
                format!("🗑 delete “{title}” ?  y / n"),
                Style::default().fg(C_FAILED).add_modifier(Modifier::BOLD),
            ),
        };
        let hw = left_limit.saturating_sub(cx + 1);
        put(&mut buf, cx, inner.y, hw as usize, &htext, hstyle);

        // ── Column titles + faint rule ──
        let hdr = Style::default().fg(C_FAINT);
        put(&mut buf, name_x, inner.y + 2, name_w as usize, "TASK", hdr);
        if show_ws {
            put(&mut buf, ws_x, inner.y + 2, ws_w as usize, "WORKSPACE", hdr);
        }
        if age_full {
            let lc = "LAST CHANGED";
            put(&mut buf, right - lc.len() as u16, inner.y + 2, lc.len(), lc, hdr);
        }
        let rule: String = "─".repeat(cw as usize);
        put(&mut buf, cx, inner.y + 3, cw as usize, &rule, Style::default().fg(C_BORDER));

        // ── Footer: mode hints (left) + summary (right) ── shrinks with width.
        let y_footer = inner.y + inner.height - 1;
        let hints: &[(&str, &str)] = match &self.mode {
            Mode::List if cw >= 72 => &[
                ("↑↓", "navigate"),
                ("↵", "switch"),
                ("→", "group"),
                ("^a", "new"),
                ("^r/^d", "rename/del"),
                ("esc", "close"),
            ],
            Mode::List if cw >= 44 => &[("↵", "switch"), ("→", "group"), ("^a", "new"), ("esc", "close")],
            Mode::List => &[("↵", "switch"), ("^a", "new"), ("esc", "")],
            Mode::Create { .. } => &[("↵", "create"), ("esc", "cancel")],
            Mode::Rename { .. } => &[("↵", "save"), ("esc", "cancel")],
            Mode::ConfirmDelete { .. } => &[("y", "delete"), ("n", "cancel")],
        };
        let mut fx = cx;
        for (key, label) in hints {
            fx = put(&mut buf, fx, y_footer, (right - fx) as usize, key, Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD));
            if !label.is_empty() {
                fx = put(&mut buf, fx + 1, y_footer, (right.saturating_sub(fx + 1)) as usize, label, Style::default().fg(C_FAINT));
            }
            fx += 3;
        }
        // Summary only when it fits without crowding the hints.
        if matches!(self.mode, Mode::List) && cw >= 60 {
            let summary = format!("{} tasks · {} ws", self.filtered.len(), self.workspace_count);
            let sx = right.saturating_sub(summary.chars().count() as u16);
            if sx > fx + 1 {
                put(&mut buf, sx, y_footer, summary.len(), &summary, Style::default().fg(C_FAINT));
            }
        }

        // ── List body ──
        let y_list = inner.y + 4;
        let list_h = y_footer.saturating_sub(y_list) as usize;
        self.list_origin = y_list as usize;
        if list_h == 0 {
            return buf_to_ansi(&buf);
        }
        if let Some(msg) = &self.message {
            put(&mut buf, name_x, y_list, cw as usize, msg, Style::default().fg(C_FAILED));
            return buf_to_ansi(&buf);
        }
        if self.filtered.is_empty() {
            let msg = if self.tasks.is_empty() { "loading…" } else { "no match" };
            put(&mut buf, name_x, y_list, cw as usize, msg, Style::default().fg(C_DIM));
            return buf_to_ansi(&buf);
        }

        // Interleave section headers, then scroll to keep the selection in view.
        let disp = self.display_rows();
        let sel_line = disp
            .iter()
            .position(|d| matches!(d, Disp::Task(p) if *p == self.selected))
            .unwrap_or(0);
        if sel_line < self.scroll {
            self.scroll = sel_line;
        } else if sel_line >= self.scroll + list_h {
            self.scroll = sel_line + 1 - list_h;
        }
        self.scroll = self.scroll.min(disp.len().saturating_sub(list_h));

        for (row, d) in disp.iter().skip(self.scroll).take(list_h).enumerate() {
            let y = y_list + row as u16;
            match d {
                Disp::Header(text) => {
                    self.line_map.push(None);
                    put(&mut buf, cx, y, cw as usize, &format!("▸ {text}"), Style::default().fg(C_SECTION).add_modifier(Modifier::BOLD));
                }
                Disp::Task(pos) => {
                    self.line_map.push(Some(*pos));
                    let t = &self.tasks[self.filtered[*pos]];
                    let selected = *pos == self.selected;
                    if selected {
                        fill_row(&mut buf, inner.x + 1, y, inner.width - 2, C_SEL_BG);
                    }
                    let bg = if selected { Some(C_SEL_BG) } else { None };
                    let base = |fg: Color| {
                        let mut s = Style::default().fg(fg);
                        if let Some(b) = bg {
                            s = s.bg(b);
                        }
                        s
                    };
                    // Status icon — an animated spinner while working.
                    let icon = if t.status == "working" {
                        SPINNER[self.frame % SPINNER.len()]
                    } else {
                        status_icon(&t.status)
                    };
                    put(&mut buf, cx, y, 1, icon, base(status_color(&t.status)));
                    // Name (+ badges), then workspace, then last-changed.
                    let current = self.active_tab.as_deref() == Some(t.slug.as_str());
                    let badge = if t.status == "blocked" {
                        Some(("needs input", C_BLOCKED, C_BADGE_INPUT_BG))
                    } else if current {
                        Some(("current", C_CURRENT_FG, C_CURRENT_BG))
                    } else {
                        None
                    };
                    let badge_w = badge.map(|(t, ..)| t.chars().count() as u16 + 3).unwrap_or(0);
                    let name_avail = name_w.saturating_sub(badge_w);
                    let name_style = base(if selected { C_SEL_TEXT } else { C_TEXT }).add_modifier(Modifier::BOLD);
                    let nx = put(&mut buf, name_x, y, name_avail as usize, &t.title, name_style);
                    if let Some((label, fg, bbg)) = badge {
                        let chip = format!(" {label} ");
                        put(&mut buf, nx + 1, y, chip.chars().count(), &chip, Style::default().fg(fg).bg(bbg).add_modifier(Modifier::BOLD));
                    }
                    if show_ws {
                        put(&mut buf, ws_x, y, ws_w as usize, &t.ws, base(C_DIM));
                    }
                    if show_age {
                        let age = age_str(&t.status, t.age_secs, age_full);
                        let ax = right.saturating_sub(age.chars().count() as u16);
                        put(&mut buf, ax, y, age.chars().count(), &age, base(C_DIM));
                    }
                }
            }
        }

        buf_to_ansi(&buf)
    }
}

const C_SECTION: Color = Color::Rgb(128, 135, 148);

fn status_color(status: &str) -> Color {
    match status {
        "working" => C_WORKING,
        "blocked" => C_BLOCKED,
        "done" => C_DONE,
        "failed" => C_FAILED,
        _ => C_IDLE,
    }
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "working" => "◐",
        "blocked" => "●",
        "done" => "✔",
        "failed" => "✖",
        _ => "·",
    }
}

/// Last-changed text. Wide: "done 30m ago" for finished tasks, bare age
/// otherwise. Compact (narrow panes): always the bare age.
fn age_str(status: &str, secs: Option<u64>, full: bool) -> String {
    match fmt_age(secs) {
        Some(a) if full && status == "done" => format!("done {a} ago"),
        Some(a) => a,
        None => String::new(),
    }
}

/// Write `s` at (x, y) clamped to `maxw` chars; returns the x after the text.
fn put(buf: &mut Buffer, x: u16, y: u16, maxw: usize, s: &str, style: Style) -> u16 {
    if maxw == 0 {
        return x;
    }
    let text: String = s.chars().take(maxw).collect();
    let n = text.chars().count() as u16;
    buf.set_stringn(x, y, text, maxw, style);
    x + n
}

/// Fill a horizontal run with a background colour (for the selection bar).
fn fill_row(buf: &mut Buffer, x: u16, y: u16, w: u16, bg: Color) {
    let blanks = " ".repeat(w as usize);
    buf.set_stringn(x, y, blanks, w as usize, Style::default().bg(bg));
}

/// Serialize a ratatui `Buffer` to a full-frame ANSI string for the plugin
/// pane: clear + home, then each row as styled cells joined by CRLF. Emitting
/// a full frame (not a diff) is fine — zellij calls `render` fresh each tick.
fn buf_to_ansi(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::from("\x1b[2J\x1b[H");
    let mut cur = String::new();
    for y in 0..area.height {
        if y > 0 {
            out.push_str("\r\n");
        }
        for x in 0..area.width {
            let Some(cell) = buf.cell((x, y)) else { continue };
            let sgr = sgr_for(cell.fg, cell.bg, cell.modifier);
            if sgr != cur {
                out.push_str(&sgr);
                cur = sgr;
            }
            let sym = cell.symbol();
            // Skip the empty placeholder cell that follows a wide glyph.
            if !sym.is_empty() {
                out.push_str(sym);
            }
        }
    }
    out.push_str("\x1b[0m");
    out
}

/// Build a self-contained SGR escape (leading reset) for a cell's style.
fn sgr_for(fg: Color, bg: Color, modifier: Modifier) -> String {
    let mut codes = String::from("0");
    if modifier.contains(Modifier::BOLD) {
        codes.push_str(";1");
    }
    if modifier.contains(Modifier::DIM) {
        codes.push_str(";2");
    }
    if modifier.contains(Modifier::ITALIC) {
        codes.push_str(";3");
    }
    if modifier.contains(Modifier::UNDERLINED) {
        codes.push_str(";4");
    }
    if modifier.contains(Modifier::REVERSED) {
        codes.push_str(";7");
    }
    push_color(&mut codes, fg, true);
    push_color(&mut codes, bg, false);
    format!("\x1b[{codes}m")
}

fn push_color(codes: &mut String, color: Color, fg: bool) {
    let base = if fg { 38 } else { 48 };
    match color {
        Color::Reset => {} // leading `0` already reset both
        Color::Rgb(r, g, b) => codes.push_str(&format!(";{base};2;{r};{g};{b}")),
        Color::Indexed(i) => codes.push_str(&format!(";{base};5;{i}")),
        // Named ANSI colors → 30–37 / 40–47 (+60 for bright).
        other => {
            if let Some(n) = ansi_named(other) {
                let off = if fg { 30 } else { 40 };
                codes.push_str(&format!(";{}", off + n));
            }
        }
    }
}

fn ansi_named(c: Color) -> Option<u8> {
    Some(match c {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        _ => return None,
    })
}

fn fmt_age(secs: Option<u64>) -> Option<String> {
    let s = secs?;
    Some(if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else if s < 86400 * 7 {
        format!("{}d", s / 86400)
    } else if s < 86400 * 30 {
        format!("{}w", s / (86400 * 7))
    } else {
        format!("{}mo", s / (86400 * 30))
    })
}

/// Subsequence (fuzzy) match: are all chars of `needle` present in `haystack`
/// in order? Both should already be lowercased.
fn subseq_match(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    for nc in needle.chars() {
        if !hay.any(|hc| hc == nc) {
            return false;
        }
    }
    true
}
