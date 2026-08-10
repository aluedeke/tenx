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
//! A plugin *pane* dissolves the problem. Zellij runs at most one instance per
//! (plugin, configuration), so it's a singleton by construction, and every
//! key/mouse event this instance receives is already attributed to the client
//! who summoned it. Jumping via the host API (`go_to_tab_name`) is therefore
//! correct for phone and desktop alike, with no "last-active-client" guessing
//! (the bug that forced Enter-only jumps in the terminal overlay).
//!
//! ## Ctrl+w toggles: why `MessagePlugin`, not `LaunchOrFocusPlugin`
//!
//! The keybind (in the user's `config.kdl`) binds Ctrl+w to `MessagePlugin
//! "tenx" { name "toggle"; floating true; }`, delivered to `pipe()` as
//! `MSG_TOGGLE`. `LaunchOrFocusPlugin` was tried first and doesn't work for a
//! toggle: once we're open and focused on the client's current tab, pressing
//! it again is a pure no-op on zellij's side (already floating, already
//! focused, nothing to move) — no event of any kind reaches the plugin, so
//! there's nothing a key handler could ever act on. `MessagePlugin` instead
//! calls `pipe()` on *every* press, whether it launches a fresh instance or
//! lands on one already running, which is exactly the signal a toggle needs
//! (see `pipe()`). The trade-off: unlike `LaunchOrFocusPlugin`'s
//! `move_to_focused_tab`, a still-open instance left behind on another tab
//! (switched away from by some means other than Ctrl+w/Esc) does not follow
//! you to your current tab — the next Ctrl+w just closes it invisibly, since
//! by then it's already "open" as far as the toggle is concerned.
//!
//! ## Lifetime: one summon, one instance
//!
//! Dismissing the overlay (esc, a jump, or the Ctrl+w toggle) **closes** the
//! pane rather than hiding it, so every summon loads the wasm currently on
//! disk. See `dismiss` for why: a hidden instance outlives reinstalls and
//! there is no reliable way to swap it out from the outside, which made `make
//! install` a silent no-op for the running session. The re-launch cost is a
//! sub-10ms `--json` call.
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
const C_FAILED: Color = Color::Rgb(226, 96, 92); // red — errors, destructive actions
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
    /// Why Claude Code is waiting, in its own words ("input needed", "sandbox
    /// request", the open dialog's label). Only set when `status` is `blocked`,
    /// and only when the session registry is what said so. `default` so an older
    /// native binary still deserializes.
    #[serde(default)]
    waiting_for: Option<String>,
    age_secs: Option<u64>,
    /// Repos this task has worktrees for — what the repo checklist diffs
    /// against. `default` so an older native binary still deserializes.
    #[serde(default)]
    repos: Vec<String>,
}

/// A repo configured in a workspace (not necessarily one this task uses).
#[derive(Debug, Clone, Deserialize)]
struct Repo {
    name: String,
    #[serde(default)]
    cloned: bool,
}

/// A registered workspace and its repos. Listed independently of tasks so the
/// checklists can be built for a workspace that has no tasks yet.
#[derive(Debug, Clone, Deserialize)]
struct Ws {
    name: String,
    dir: String,
    #[serde(default)]
    repos: Vec<Repo>,
}

#[derive(Deserialize)]
struct TaskDump {
    tasks: Vec<Task>,
    #[serde(default)]
    workspaces: Vec<Ws>,
}

/// Context marker on our `run_command` calls so we can tell the data refresh
/// apart from a fire-and-forget mutation in `RunCommandResult`.
const CTX_KIND: &str = "kind";
const KIND_TASKS: &str = "tasks";
/// Any mutation (create/rename/delete/open) uses this marker; on its result we
/// re-read task data so the list reflects the change.
const KIND_MUTATE: &str = "mutate";

/// Name of the pipe message the "Ctrl w" keybind sends (see `pipe()`). The
/// keybind uses `MessagePlugin`, not `LaunchOrFocusPlugin`: the latter is a
/// silent no-op when we're already open and focused on the current tab (no
/// event reaches the plugin either way), which is exactly the case that needs
/// to close us — `MessagePlugin` instead always delivers this message,
/// whether it just launched us or found us already running.
const MSG_TOGGLE: &str = "toggle";

/// Poll interval for re-reading task state (status glyphs, ages, open flags).
/// A poll is cheap; the *render* it may trigger is not, so it only asks for one
/// when `data_fingerprint` says something changed.
const POLL_SECS: f64 = 1.5;
/// Spinner frame interval while a mutation runs — the only animated state.
/// Slower than it looks like it should be on purpose: each frame retransmits
/// the whole pane, so this is a legibility-vs-bandwidth trade, not a frame rate.
const ANIM_SECS: f64 = 0.25;
/// In animated mode, poll data once every N frames (≈ POLL_SECS worth).
const POLL_EVERY: usize = (POLL_SECS / ANIM_SECS) as usize;
/// Braille spinner frames for the busy indicator.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ── Responsive pane geometry ── At/under the phone thresholds the overlay
// fills the whole screen; past them it scales by percentage up to a desktop cap
// and centers. The plugin sizes ITS OWN floating pane (zellij's own default on
// creation is just a fraction of the terminal, which is cramped on a phone).
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

/// One line of a repo checklist: a workspace repo, whether it's ticked, and
/// whether the task already has a worktree for it. `checked != present` is the
/// pending change — added when ticked, detached when unticked. On the create
/// form nothing is `present` yet, so every tick is an addition.
#[derive(Clone)]
struct Pick {
    name: String,
    checked: bool,
    present: bool,
    cloned: bool,
}

/// The shared repo checklist widget, used by both the create form and the
/// edit-repos form (the only difference is what `present` starts as).
#[derive(Default, Clone)]
struct RepoPick {
    items: Vec<Pick>,
    cursor: usize,
}

impl RepoPick {
    fn move_cursor(&mut self, delta: isize) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        self.cursor = (self.cursor as isize + delta).rem_euclid(n as isize) as usize;
    }
    fn toggle(&mut self) {
        if let Some(it) = self.items.get_mut(self.cursor) {
            it.checked = !it.checked;
        }
    }
    fn set_all(&mut self, on: bool) {
        self.items.iter_mut().for_each(|i| i.checked = on);
    }
    fn checked(&self) -> Vec<String> {
        self.items.iter().filter(|i| i.checked).map(|i| i.name.clone()).collect()
    }
    fn added(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| i.checked && !i.present)
            .map(|i| i.name.clone())
            .collect()
    }
    fn removed(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| !i.checked && i.present)
            .map(|i| i.name.clone())
            .collect()
    }
}

/// Which field of the create form has the keyboard: the name, or the checklist.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Name,
    Repos,
}

/// What the overlay is doing: the list, or a modal capturing text / a y/n.
#[derive(Default)]
enum Mode {
    #[default]
    List,
    /// Typing a new task name; created in `ws_dir` (of the selected task).
    /// `↵` from the name creates with every repo ticked (the fast path); `⇥`
    /// steps into the checklist to narrow it down first.
    Create { name: String, ws: String, ws_dir: String, phase: Phase, pick: RepoPick },
    /// Repo checklist for an existing task (add/detach worktrees).
    EditRepos { title: String, slug: String, ws_dir: String, pick: RepoPick },
    /// Confirming a repo change that detaches worktrees (destructive).
    ConfirmRepos {
        title: String,
        slug: String,
        ws_dir: String,
        /// The full desired set to hand to `task set-repos`.
        desired: Vec<String>,
        remove: Vec<String>,
    },
    /// Editing the selected task's title; buffer pre-filled with the old one.
    Rename { buffer: String, slug: String, ws_dir: String },
    /// Confirming deletion of the selected task.
    ConfirmDelete { title: String, slug: String, ws_dir: String },
    /// A mutation is running. Cloning a repo can take minutes, so the overlay
    /// stays up and says so instead of vanishing into a blank screen — and an
    /// error lands where the user is still looking.
    Busy { label: String, hide_on_done: bool },
}

/// How the list is organised: sectioned by agent status (the default — what
/// needs you, in the order it needs you), one flat activity-sorted section, or
/// grouped by workspace. Cycled with → (the header's segmented control mirrors
/// it).
#[derive(Default, Clone, Copy, PartialEq)]
enum Grouping {
    #[default]
    Status,
    Recent,
    Workspace,
}

/// Status tokens in intra-section order; unknown tokens sort last. Mirrors
/// `TaskStatus::rank` on the native side — only `blocked` before `done` really
/// matters, so an agent parked on a prompt sits above one that merely finished.
const STATUS_ORDER: [&str; 4] = ["blocked", "working", "done", "idle"];

/// Section labels in display order, mirroring `TaskGroup` on the native side.
/// `blocked` and `done` share one: both are waiting on you, and two adjacent
/// headers saying nearly the same thing helped nobody. The row's icon and
/// Claude's waiting reason carry the difference.
const GROUPS: [&str; 3] = ["WAITING FOR INPUT", "WORKING", "INACTIVE"];

/// Rank of a status token within `STATUS_ORDER`; unknown tokens sort with idle.
fn status_rank(status: &str) -> usize {
    STATUS_ORDER
        .iter()
        .position(|tok| *tok == status)
        .unwrap_or(STATUS_ORDER.len() - 1)
}

/// Section index for a status rank: blocked/done → waiting, working → working,
/// idle → inactive.
fn group_rank(status_rank: usize) -> usize {
    match STATUS_ORDER.get(status_rank) {
        Some(&"working") => 1,
        Some(&"idle") => 2,
        _ => 0,
    }
}

/// A rendered list line: a blank spacer, a section header, or a task
/// (position in `filtered`).
enum Disp {
    Gap,
    Header(String),
    Task(usize),
}

#[derive(Default)]
struct State {
    /// Absolute path to the native tenx binary (from plugin config).
    tenx_bin: String,
    tasks: Vec<Task>,
    /// Every registered workspace and its repos — the source for both repo
    /// checklists, and the create target when no task is selected.
    workspaces: Vec<Ws>,
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
    /// Set once this (single, still-running) instance has processed one
    /// `MSG_TOGGLE` pipe message — see `pipe()` for why this, and not
    /// `visible`, is what decides open-vs-close.
    armed: bool,
    /// Frozen display order — task key plus the status rank it was filed under
    /// (which fixes both its section and its place inside it, see
    /// `frozen_rank`). The rows never re-sort while the overlay is open; only a
    /// (re)open takes a new snapshot (`freeze_order`). The rank is frozen
    /// alongside the position because status keeps arriving from the poll:
    /// without it a task flipping blocked→working mid-view would tear its
    /// section in two and jump out from under the cursor. The row's icon still
    /// updates live.
    order: Vec<((String, String), usize)>,
    /// Spinner frame counter (advances each animation tick).
    frame: usize,
    /// Screen row of the first list line, set during draw (click map).
    list_origin: usize,
    /// Display-line scroll offset, set during draw.
    scroll: usize,
    /// Per-rendered-line → filtered position (None for headers/blanks).
    line_map: Vec<Option<usize>>,
    /// Hash of everything a background poll can change on screen, as of the
    /// last render. A poll whose data hashes the same must not ask to render —
    /// see `buf_to_ansi` for what a render costs.
    fingerprint: u64,
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
                // A running mutation is the one place motion carries
                // information, and it's the only thing on screen while it runs
                // — a clone can take minutes with nothing else to look at. The
                // *list* never animates: a frame there costs the whole pane
                // (see `buf_to_ansi`), and a spinner on every working task made
                // the overlay repaint itself ~9 times a second forever.
                if matches!(self.mode, Mode::Busy { .. }) {
                    self.frame = self.frame.wrapping_add(1);
                    if self.frame % POLL_EVERY == 0 {
                        self.refresh();
                    }
                    self.schedule(ANIM_SECS);
                    return true;
                }
                self.refresh();
                self.schedule(POLL_SECS);
                false // the poll's result renders only if it changed anything
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
                            self.workspaces = dump.workspaces;
                            self.apply_filter();
                        }
                        // Most polls find nothing new — ages are drawn to the
                        // minute and statuses change rarely. Reporting those as
                        // needing a render would repaint the whole pane every
                        // POLL_SECS for no visible difference. (`fingerprint`
                        // is stamped by `render`, so this compares against what
                        // is actually on screen, not against the last poll.)
                        self.data_fingerprint() != self.fingerprint
                    }
                    Some(KIND_MUTATE) => {
                        // A create/rename/delete/repo-change finished — surface
                        // any error, then reload so the list reflects it.
                        let failed = exit != Some(0);
                        if failed {
                            let msg = String::from_utf8_lossy(&stderr);
                            let msg = msg.trim();
                            self.message =
                                Some(if msg.is_empty() { "command failed".into() } else { msg.into() });
                        }
                        // Release a Busy wait: on success a create hides (its
                        // tab already took the screen), everything else drops
                        // back to the refreshed list. On failure we always stay
                        // put so the error is actually seen.
                        if let Mode::Busy { hide_on_done, .. } = self.mode {
                            self.mode = Mode::List;
                            if !failed && hide_on_done {
                                self.dismiss();
                            }
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

    /// Delivery for the "Ctrl w" keybind (`MessagePlugin`, see `MSG_TOGGLE`).
    /// Every press — whether it launches a fresh instance or lands on one
    /// already running — sends exactly one of these, so `armed` alone tells
    /// the two apart: unset means this is the message that (re)opened us,
    /// set means it's a second press on the same still-alive instance and
    /// the toggle should now close it. `MessagePlugin` doesn't focus a
    /// freshly-created pane the way `LaunchOrFocusPlugin` does, so the open
    /// branch claims focus itself.
    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if pipe_message.name != MSG_TOGGLE {
            return false;
        }
        if self.armed {
            self.dismiss();
        } else {
            self.armed = true;
            show_self(true);
        }
        true
    }

    fn render(&mut self, rows: usize, cols: usize) {
        // Being rendered is proof we're on screen, so (re)enable polling here.
        // Polling is gated on `visible`, but `Visible(true)` doesn't reliably
        // fire on re-summon — without this, `hide()` (from the first jump)
        // leaves `visible` stuck false and the re-opened overlay shows stale
        // status forever. A suppressed pane isn't rendered, so this can't keep
        // polling while hidden.
        self.visible = true;
        self.ensure_tick();

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
        // Self-correct the pane size. On every (re)open zellij creates the
        // pane at its own default (small) size, and no Visible/changed-
        // TabUpdate event fires to tell us. So whenever we render notably
        // smaller than the responsive target, re-apply the geometry.
        // Converges in a frame; the tolerance avoids border-off-by-one churn
        // at steady state.
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
        // Record what is now on screen, so the next poll can tell whether it
        // still matches and skip a repaint if it does.
        self.fingerprint = self.data_fingerprint();
    }
}

impl State {
    /// Snapshot the current activity order + status ranks into the frozen
    /// display order. Taken synchronously when the overlay (re)opens, so the
    /// rows the user sees stay put while they choose — background polls refresh
    /// data but never reorder.
    fn freeze_order(&mut self) {
        self.order = self
            .tasks
            .iter()
            .map(|t| ((t.ws_dir.clone(), t.slug.clone()), status_rank(&t.status)))
            .collect();
    }

    /// The task's frozen status rank — what it had when the order was taken, or
    /// its current status for a task created since. Sections derive from it via
    /// `group_rank`, so freezing one value pins both the section and the
    /// position within it.
    fn frozen_rank(&self, i: usize) -> usize {
        let t = &self.tasks[i];
        let k = (t.ws_dir.clone(), t.slug.clone());
        self.order
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, g)| *g)
            .unwrap_or_else(|| status_rank(&t.status))
    }

    /// Hash of everything a background poll can change on screen: the drawn
    /// form of each task (note `fmt_age`, not the raw seconds — the list shows
    /// minutes), the resulting filter/selection, the "current" badge's tab, and
    /// the workspace repo state the checklists read. Anything the *user* does
    /// reports its own render, so it doesn't need to be in here.
    fn data_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for t in &self.tasks {
            (&t.ws, &t.slug, &t.title, &t.status, &t.waiting_for, &t.repos).hash(&mut h);
            fmt_age(t.age_secs).hash(&mut h);
        }
        for w in &self.workspaces {
            w.name.hash(&mut h);
            for r in &w.repos {
                (&r.name, r.cloned).hash(&mut h);
            }
        }
        (&self.filtered, self.selected, &self.active_tab).hash(&mut h);
        h.finish()
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
    /// each (re)open is a fresh pane at zellij's default (small) floating
    /// size, but the display area is unchanged from last time — so
    /// `fit_pane`'s guard would skip. Without this the first open is
    /// correctly sized but every re-open is default-sized.
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

    /// Dismiss the overlay: **close** the pane, don't hide it, so the next
    /// Ctrl+w launches a brand-new instance.
    ///
    /// Hiding (`hide_self`) kept the instance alive so a re-summon could paint
    /// the cached task list without a `loading…` frame — but `tenx overlay
    /// --json` returns in under 10ms, so that buys almost nothing, and it costs
    /// a lot: a hidden instance stays welded to the wasm that was on disk when
    /// the session started. Reinstalling the plugin mid-session then had no
    /// effect (`start-or-reload-plugin` spawns an *additional* pane rather than
    /// swapping the running one), so the overlay silently served stale code
    /// until every leftover pane was hunted down and closed by hand.
    ///
    /// Closing makes that impossible by construction: the wasm on disk is the
    /// only thing that can ever be running. Nothing is lost — `reopen_pending`
    /// already discarded the sort order and selection on every re-open, so a
    /// summon was meant to start fresh regardless; now the filter resets too.
    ///
    /// The `visible` gating stays: the pane can still go off-screen without
    /// being dismissed (switching tabs leaves it behind on the old one), and
    /// polling must stop when it does.
    fn dismiss(&mut self) {
        self.visible = false;
        self.reopen_pending = true;
        close_self();
    }

    /// Recompute `filtered` from `filter` + `grouping`, clamping the selection.
    /// Data arrives already activity-sorted from `tenx overlay --json`; Recent
    /// keeps that order, Status and Workspace re-bucket on top of it (stable, so
    /// within a bucket the activity order is preserved). `filtered` is always in
    /// final display order, so navigation can walk it linearly.
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
            s.order
                .iter()
                .position(|(key, _)| *key == k)
                .unwrap_or(usize::MAX)
        };
        filtered.sort_by_key(|&i| rank(i, self));
        match self.grouping {
            // Stable re-buckets; frozen activity order retained within each.
            // Section first, then blocked-above-done inside it. Stable, so the
            // frozen activity order survives within each pair.
            Grouping::Status => {
                filtered.sort_by_key(|&i| {
                    let r = self.frozen_rank(i);
                    (group_rank(r), r)
                })
            }
            Grouping::Workspace => {
                filtered.sort_by(|&a, &b| self.tasks[a].ws.cmp(&self.tasks[b].ws))
            }
            Grouping::Recent => {}
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
    /// current grouping. Status → one header per non-empty status section, with
    /// its count; Recent → a single "RECENT" header; Workspace → one header per
    /// workspace bucket.
    fn display_rows(&self) -> Vec<Disp> {
        let mut out = Vec::new();
        let mut last_ws: Option<&str> = None;
        let mut last_group: Option<usize> = None;
        for (pos, &ti) in self.filtered.iter().enumerate() {
            match self.grouping {
                Grouping::Status => {
                    let g = group_rank(self.frozen_rank(ti));
                    if last_group != Some(g) {
                        if !out.is_empty() {
                            out.push(Disp::Gap);
                        }
                        let n = self
                            .filtered
                            .iter()
                            .filter(|&&i| group_rank(self.frozen_rank(i)) == g)
                            .count();
                        out.push(Disp::Header(format!("{}  {n}", GROUPS[g])));
                        last_group = Some(g);
                    }
                }
                Grouping::Recent => {
                    if pos == 0 {
                        out.push(Disp::Header("RECENT".into()));
                    }
                }
                Grouping::Workspace => {
                    let ws = self.tasks[ti].ws.as_str();
                    if last_ws != Some(ws) {
                        // Blank line before each group (but not the first) so
                        // sections read as distinct blocks.
                        if !out.is_empty() {
                            out.push(Disp::Gap);
                        }
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
        self.dismiss();
        false
    }

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        self.message = None;
        match &self.mode {
            Mode::List => self.handle_list_key(key),
            Mode::Create { .. } => self.handle_create_key(key),
            Mode::EditRepos { .. } => self.handle_editrepos_key(key),
            Mode::ConfirmRepos { .. } => self.handle_confirm_repos_key(key),
            Mode::Rename { .. } => self.handle_rename_key(key),
            Mode::ConfirmDelete { .. } => self.handle_confirm_key(key),
            Mode::Busy { .. } => {
                // The command keeps running; esc just stops staring at it.
                if key.bare_key == BareKey::Esc {
                    self.mode = Mode::List;
                    return true;
                }
                false
            }
        }
    }

    /// List mode: plain chars filter, so all actions live on Ctrl. Ctrl-n/p
    /// move (emacs-style, alongside arrows); Ctrl-a/r/d add/rename/delete.
    fn handle_list_key(&mut self, key: KeyWithModifier) -> bool {
        let ctrl = key.has_modifiers(&[KeyModifier::Ctrl]);
        match key.bare_key {
            BareKey::Esc if key.has_no_modifiers() => {
                if self.filter.is_empty() {
                    self.dismiss();
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
            // → / ⇥ cycle grouping forward (status → recent → workspace), ←
            // back — matching the order of the header's segmented control.
            BareKey::Right | BareKey::Tab if key.has_no_modifiers() => {
                self.grouping = match self.grouping {
                    Grouping::Status => Grouping::Recent,
                    Grouping::Recent => Grouping::Workspace,
                    Grouping::Workspace => Grouping::Status,
                };
                self.apply_filter();
            }
            BareKey::Left if key.has_no_modifiers() => {
                self.grouping = match self.grouping {
                    Grouping::Status => Grouping::Workspace,
                    Grouping::Recent => Grouping::Status,
                    Grouping::Workspace => Grouping::Recent,
                };
                self.apply_filter();
            }
            BareKey::Char('a') if ctrl => self.start_create(),
            BareKey::Char('r') if ctrl => self.start_rename(),
            BareKey::Char('d') if ctrl => self.start_delete(),
            BareKey::Char('e') if ctrl => self.start_edit_repos(),
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

    /// Create form. Two phases share one modal: `Name` is exactly the old
    /// behaviour (type, `↵` creates with every repo), and `⇥`/`↓` steps into the
    /// checklist — where bare characters are free, so `space`/`a`/`n` can be
    /// single-key toggles and taps can hit rows.
    fn handle_create_key(&mut self, key: KeyWithModifier) -> bool {
        let Mode::Create { mut name, ws, ws_dir, mut phase, mut pick } =
            std::mem::replace(&mut self.mode, Mode::List)
        else {
            return false;
        };
        match phase {
            Phase::Name => match key.bare_key {
                BareKey::Esc => return true, // cancelled; mode is already List
                BareKey::Enter => {
                    if self.submit_create(&name, &ws_dir, &pick) {
                        return true;
                    }
                }
                BareKey::Tab | BareKey::Down if !pick.items.is_empty() => phase = Phase::Repos,
                BareKey::Backspace => {
                    name.pop();
                }
                BareKey::Char(c) if key.has_no_modifiers() => name.push(c),
                _ => {}
            },
            Phase::Repos => {
                if !pick_key(&mut pick, &key) {
                    match key.bare_key {
                        BareKey::Esc | BareKey::Tab | BareKey::Left => phase = Phase::Name,
                        BareKey::Enter => {
                            if self.submit_create(&name, &ws_dir, &pick) {
                                return true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        self.mode = Mode::Create { name, ws, ws_dir, phase, pick };
        true
    }

    /// Fire the create. Returns false when the form should stay open (nothing
    /// ticked) — the caller has already taken `self.mode`, so it has to put the
    /// form back rather than leave the user dumped on the list.
    fn submit_create(&mut self, name: &str, ws_dir: &str, pick: &RepoPick) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return true; // nothing typed — treat ↵ as a cancel
        }
        let repos = pick.checked();
        // An empty workspace has nothing to tick; let the native side answer
        // with its "no repos in workspace" guidance rather than inventing one.
        if repos.is_empty() && !pick.items.is_empty() {
            self.message = Some("select at least one repo".into());
            return false;
        }
        let joined = repos.join(",");
        let mut args = vec!["task", "new", "--ws-dir", ws_dir];
        if !joined.is_empty() {
            args.extend_from_slice(&["--repos", &joined]);
        }
        args.push(name);
        // The new task's tab opens itself (new_in with no_open=false), so on
        // success we get out of the way like a jump does.
        self.run_mutation_busy(&args, format!("creating {name}…"), true);
        true
    }

    fn handle_editrepos_key(&mut self, key: KeyWithModifier) -> bool {
        let Mode::EditRepos { title, slug, ws_dir, mut pick } =
            std::mem::replace(&mut self.mode, Mode::List)
        else {
            return false;
        };
        if !pick_key(&mut pick, &key) {
            match key.bare_key {
                BareKey::Esc => return true, // cancelled; mode is already List
                BareKey::Enter => {
                    let (desired, remove) = (pick.checked(), pick.removed());
                    if desired.is_empty() {
                        // Nothing ticked — keep the form up with the reason.
                        self.message = Some("a task must keep at least one repo".into());
                        self.mode = Mode::EditRepos { title, slug, ws_dir, pick };
                    } else if pick.added().is_empty() && remove.is_empty() {
                        // Nothing to do; ↵ reads as a cancel.
                    } else if remove.is_empty() {
                        self.apply_repos(&title, &slug, &ws_dir, &desired);
                    } else {
                        // Detaching drops a worktree and its branch — confirm.
                        self.mode =
                            Mode::ConfirmRepos { title, slug, ws_dir, desired, remove };
                    }
                    return true;
                }
                _ => {}
            }
        }
        self.mode = Mode::EditRepos { title, slug, ws_dir, pick };
        true
    }

    fn handle_confirm_repos_key(&mut self, key: KeyWithModifier) -> bool {
        let Mode::ConfirmRepos { title, slug, ws_dir, desired, .. } =
            std::mem::replace(&mut self.mode, Mode::List)
        else {
            return false;
        };
        match key.bare_key {
            BareKey::Char('y') | BareKey::Char('Y') => {
                self.apply_repos(&title, &slug, &ws_dir, &desired)
            }
            _ => {} // any other key cancels, back to the list
        }
        true
    }

    /// Hand the whole desired set to `task set-repos`, which re-diffs it against
    /// what's actually on disk and adds/detaches in one invocation — one command
    /// and one result, rather than sequencing two async mutations. No `--force`:
    /// git's refusal to drop a dirty worktree is the safety net here.
    fn apply_repos(&mut self, title: &str, slug: &str, ws_dir: &str, desired: &[String]) {
        let mut args: Vec<&str> = vec!["task", "set-repos", "--ws-dir", ws_dir, slug];
        args.extend(desired.iter().map(String::as_str));
        self.run_mutation_busy(&args, format!("updating repos for {title}…"), false);
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
        // Create in the selected task's workspace; with nothing selected (an
        // empty or fully-filtered list) fall back to the first registered
        // workspace, so a workspace with no tasks yet is still reachable.
        let target = self
            .selected_task()
            .map(|t| (t.ws.clone(), t.ws_dir.clone()))
            .or_else(|| self.workspaces.first().map(|w| (w.name.clone(), w.dir.clone())));
        let Some((ws, ws_dir)) = target else {
            self.message = Some("no workspace registered — run `tenx init` in one".into());
            return;
        };
        let pick = self.pick_for(&ws_dir, &[]);
        self.mode = Mode::Create { name: String::new(), ws, ws_dir, phase: Phase::Name, pick };
    }

    /// Open the repo checklist for the selected task, prefilled with the
    /// worktrees it already has.
    fn start_edit_repos(&mut self) {
        let Some(t) = self.selected_task().cloned() else {
            return;
        };
        let pick = self.pick_for(&t.ws_dir, &t.repos);
        if pick.items.is_empty() {
            self.message = Some("no repos in this workspace — add one with `tenx repo add`".into());
            return;
        }
        self.mode = Mode::EditRepos {
            title: t.title.clone(),
            slug: t.slug.clone(),
            ws_dir: t.ws_dir.clone(),
            pick,
        };
    }

    /// Build a checklist for a workspace's repos. `present` is the set the task
    /// already has (empty when creating); those start ticked, and on the create
    /// form — where nothing is present — everything starts ticked so `↵` still
    /// means "all repos".
    fn pick_for(&self, ws_dir: &str, present: &[String]) -> RepoPick {
        let creating = present.is_empty();
        let mut items: Vec<Pick> = self
            .workspaces
            .iter()
            .find(|w| w.dir == ws_dir)
            .map(|w| {
                w.repos
                    .iter()
                    .map(|r| Pick {
                        name: r.name.clone(),
                        checked: creating || present.contains(&r.name),
                        present: present.contains(&r.name),
                        cloned: r.cloned,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A worktree whose repo has since left the workspace config still needs
        // a row, otherwise it could never be detached from here.
        for name in present {
            if !items.iter().any(|i| &i.name == name) {
                items.push(Pick {
                    name: name.clone(),
                    checked: true,
                    present: true,
                    cloned: true,
                });
            }
        }
        RepoPick { items, cursor: 0 }
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

    /// Run a mutation and wait on it visibly. `hide_on_done` dismisses the
    /// overlay once the command *succeeds* (for create, whose new tab has
    /// already taken over the screen); a failure always keeps us up so the
    /// error message lands somewhere the user is looking.
    fn run_mutation_busy(&mut self, args: &[&str], label: String, hide_on_done: bool) {
        self.run_mutation(args);
        self.message = None;
        self.mode = Mode::Busy { label, hide_on_done };
    }

    /// The checklist currently on screen, if any (create's repo phase, the
    /// edit form, or the confirm step which still shows it read-only).
    fn active_pick(&self) -> Option<&RepoPick> {
        match &self.mode {
            Mode::Create { phase: Phase::Repos, pick, .. } => Some(pick),
            Mode::EditRepos { pick, .. } => Some(pick),
            _ => None,
        }
    }

    fn active_pick_mut(&mut self) -> Option<&mut RepoPick> {
        match &mut self.mode {
            Mode::Create { phase: Phase::Repos, pick, .. } => Some(pick),
            Mode::EditRepos { pick, .. } => Some(pick),
            _ => None,
        }
    }

    fn handle_mouse(&mut self, m: Mouse) -> bool {
        // The repo checklist is tappable too — that's the point of rendering it
        // in the body rather than on the header line. Text prompts stay
        // keyboard-only.
        if self.active_pick().is_some() {
            let line = match m {
                Mouse::ScrollDown(_) | Mouse::ScrollUp(_) => {
                    let delta = if matches!(m, Mouse::ScrollDown(_)) { 1 } else { -1 };
                    if let Some(p) = self.active_pick_mut() {
                        p.move_cursor(delta);
                    }
                    return true;
                }
                Mouse::LeftClick(line, _) => line,
                _ => return false,
            };
            let Some(idx) = self.row_at(line) else { return false };
            if let Some(pick) = self.active_pick_mut() {
                pick.cursor = idx;
                pick.toggle();
            }
            return true;
        }
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

    /// Draw the repo checklist into the body area, scrolled to keep the cursor
    /// in view. Each row states what applying would do to it — "+ add",
    /// "− detach", or the settled "worktree" — so the diff is readable without
    /// remembering what was ticked on entry.
    fn draw_picks(
        &mut self,
        buf: &mut Buffer,
        name_x: u16,
        y_list: u16,
        right: u16,
        name_w: u16,
        list_h: usize,
    ) {
        // Cloned so the loop can also push into `self.line_map`; a checklist is
        // a handful of rows, so the copy is free.
        let Some(pick) = self.active_pick().cloned() else { return };
        // Scroll the same way the task list does: keep the cursor on screen.
        let scroll = if pick.cursor >= list_h { pick.cursor + 1 - list_h } else { 0 };
        for (row, item) in pick.items.iter().enumerate().skip(scroll).take(list_h) {
            let y = y_list + (row - scroll) as u16;
            self.line_map.push(Some(row));
            let selected = row == pick.cursor;
            if selected {
                fill_row(buf, name_x - 2, y, right - name_x + 2, C_SEL_BG);
            }
            let bg = if selected { Some(C_SEL_BG) } else { None };
            let style = |fg: Color| {
                let mut s = Style::default().fg(fg);
                if let Some(b) = bg {
                    s = s.bg(b);
                }
                s
            };
            let (note, note_fg) = match (item.checked, item.present) {
                (true, true) => ("worktree", C_DIM),
                (true, false) => ("+ add", C_DONE),
                (false, true) => ("− detach", C_FAILED),
                (false, false) if !item.cloned => ("not cloned", C_FAINT),
                (false, false) => ("", C_FAINT),
            };
            let box_ = if item.checked { "[x]" } else { "[ ]" };
            put(buf, name_x - 2, y, 3, box_, style(if item.checked { C_DONE } else { C_FAINT }));
            let name_fg = if selected {
                C_SEL_TEXT
            } else if item.checked {
                C_TEXT
            } else {
                C_DIM
            };
            // The note is right-aligned, so the name gets whatever is left of
            // it — on a narrow pane that's what truncates, not the status.
            let note_w = note.chars().count() as u16;
            let avail = right
                .saturating_sub(name_x + 2)
                .saturating_sub(if note_w > 0 { note_w + 2 } else { 0 });
            put(buf, name_x + 2, y, name_w.min(avail) as usize, &item.name, style(name_fg).add_modifier(Modifier::BOLD));
            if note_w > 0 {
                put(buf, right - note_w, y, note_w as usize, note, style(note_fg));
            }
        }
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
        // Segmented control, only when there's room (→ still cycles without it).
        // Three chips need a wide pane; on a narrower one just the active chip
        // is shown, so the current grouping is still legible on a phone.
        if matches!(self.mode, Mode::List) && cw >= 48 {
            let chips: [(&str, Grouping); 3] = [
                (" status ", Grouping::Status),
                (" recent ", Grouping::Recent),
                (" workspace ", Grouping::Workspace),
            ];
            let on = Style::default().fg(C_SEL_TEXT).bg(C_TOGGLE_ON_BG).add_modifier(Modifier::BOLD);
            let off = Style::default().fg(C_FAINT);
            let mut x = left_limit - 1;
            for (label, g) in chips.iter().rev() {
                if cw < 64 && self.grouping != *g {
                    continue;
                }
                x -= label.len() as u16;
                put(&mut buf, x, inner.y, label.len(), label, if self.grouping == *g { on } else { off });
            }
            left_limit = x;
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
            // Name phase carries the caret; in the repo phase the name is
            // settled and the checklist below has the focus.
            Mode::Create { name, ws, phase: Phase::Name, .. } => (
                format!("＋ new in {ws} — {name}▏"),
                Style::default().fg(C_WORKING).add_modifier(Modifier::BOLD),
            ),
            Mode::Create { name, ws, .. } => (
                format!("＋ new in {ws} — {name}"),
                Style::default().fg(C_DIM),
            ),
            Mode::EditRepos { title, .. } => (
                format!("⛁ repos — {title}"),
                Style::default().fg(C_WORKING).add_modifier(Modifier::BOLD),
            ),
            Mode::ConfirmRepos { remove, .. } => (
                format!("⛁ detach {} — worktree + branch.  y / n", remove.join(", ")),
                Style::default().fg(C_FAILED).add_modifier(Modifier::BOLD),
            ),
            Mode::Busy { label, .. } => (
                format!("{} {label}", SPINNER[self.frame % SPINNER.len()]),
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

        // ── Column titles + faint rule ── (the checklist relabels them)
        let hdr = Style::default().fg(C_FAINT);
        let picking = self.active_pick().is_some();
        if picking {
            let n = self.active_pick().map(|p| p.items.len()).unwrap_or(0);
            let on = self.active_pick().map(|p| p.checked().len()).unwrap_or(0);
            put(&mut buf, name_x, inner.y + 2, name_w as usize, "REPO", hdr);
            let count = format!("{on} of {n}");
            put(&mut buf, right - count.len() as u16, inner.y + 2, count.len(), &count, hdr);
        } else {
            put(&mut buf, name_x, inner.y + 2, name_w as usize, "TASK", hdr);
            if show_ws {
                put(&mut buf, ws_x, inner.y + 2, ws_w as usize, "WORKSPACE", hdr);
            }
            if age_full {
                let lc = "LAST CHANGED";
                put(&mut buf, right - lc.len() as u16, inner.y + 2, lc.len(), lc, hdr);
            }
        }
        let rule: String = "─".repeat(cw as usize);
        put(&mut buf, cx, inner.y + 3, cw as usize, &rule, Style::default().fg(C_BORDER));

        // ── Footer: mode hints (left) + summary (right) ── shrinks with width.
        let y_footer = inner.y + inner.height - 1;
        let hints: &[(&str, &str)] = match &self.mode {
            Mode::List if cw >= 86 => &[
                ("↑↓", "navigate"),
                ("↵", "switch"),
                ("→", "group"),
                ("^a", "new"),
                ("^e", "repos"),
                ("^r/^d", "rename/del"),
                ("esc", "close"),
            ],
            Mode::List if cw >= 72 => &[
                ("↵", "switch"),
                ("→", "group"),
                ("^a", "new"),
                ("^e", "repos"),
                ("^r/^d", "rename/del"),
                ("esc", "close"),
            ],
            Mode::List if cw >= 44 => {
                &[("↵", "switch"), ("^a", "new"), ("^e", "repos"), ("esc", "close")]
            }
            Mode::List => &[("↵", "switch"), ("^a", "new"), ("^e", "repos")],
            Mode::Create { phase: Phase::Name, .. } if cw >= 56 => {
                &[("↵", "create with all repos"), ("⇥", "pick repos"), ("esc", "cancel")]
            }
            Mode::Create { phase: Phase::Name, .. } => {
                &[("↵", "create"), ("⇥", "repos"), ("esc", "cancel")]
            }
            Mode::Create { .. } | Mode::EditRepos { .. } if cw >= 56 => &[
                ("↵", "apply"),
                ("space", "toggle"),
                ("a/n", "all/none"),
                ("esc", "back"),
            ],
            Mode::Create { .. } | Mode::EditRepos { .. } => {
                &[("↵", "apply"), ("space", "toggle"), ("esc", "back")]
            }
            Mode::ConfirmRepos { .. } => &[("y", "detach"), ("n", "cancel")],
            Mode::Busy { .. } => &[("esc", "run in background")],
            Mode::Rename { .. } => &[("↵", "save"), ("esc", "cancel")],
            Mode::ConfirmDelete { .. } => &[("y", "delete"), ("n", "cancel")],
        };
        // An error raised by a modal belongs next to the modal, not on a list
        // line the modal is covering.
        let modal_msg = match (&self.mode, &self.message) {
            (Mode::List, _) => None,
            (_, Some(msg)) => Some(msg.clone()),
            _ => None,
        };
        if let Some(msg) = modal_msg {
            put(&mut buf, cx, y_footer, cw as usize, &msg, Style::default().fg(C_FAILED));
        } else {
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
        }

        // ── Body ──
        let y_list = inner.y + 4;
        let list_h = y_footer.saturating_sub(y_list) as usize;
        self.list_origin = y_list as usize;
        if list_h == 0 {
            return buf_to_ansi(&buf);
        }
        // A checklist (or a busy wait) takes over the body; `line_map` keeps
        // pointing at whatever is drawn there, so taps land on the right thing.
        if self.active_pick().is_some() {
            self.draw_picks(&mut buf, name_x, y_list, right, name_w, list_h);
            return buf_to_ansi(&buf);
        }
        if let Mode::Busy { label, .. } = &self.mode {
            let spin = format!("{} {label}", SPINNER[self.frame % SPINNER.len()]);
            put(&mut buf, name_x, y_list, cw as usize, &spin, Style::default().fg(C_DIM));
            if list_h > 2 {
                put(
                    &mut buf,
                    name_x,
                    y_list + 2,
                    cw as usize,
                    "a first clone can take a while — esc leaves it running",
                    Style::default().fg(C_FAINT),
                );
            }
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
                Disp::Gap => {
                    self.line_map.push(None); // blank spacer line
                }
                Disp::Header(text) => {
                    self.line_map.push(None);
                    // Section divider: uppercase label + a faint rule to the
                    // edge, so it clearly isn't a task row.
                    let label = format!("{text}  ");
                    let lx = put(
                        &mut buf,
                        cx,
                        y,
                        cw as usize,
                        &label,
                        Style::default().fg(C_SECTION).add_modifier(Modifier::BOLD),
                    );
                    if lx < right {
                        let rule: String = "─".repeat((right - lx) as usize);
                        put(&mut buf, lx, y, (right - lx) as usize, &rule, Style::default().fg(C_BORDER));
                    }
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
                    // Status icon. Deliberately static, including for `working`:
                    // animating it means re-rendering, and a render is the
                    // whole pane (see `buf_to_ansi`). The blue ◐ against the
                    // muted · already says which rows are live.
                    put(&mut buf, cx, y, 1, status_icon(&t.status), base(status_color(&t.status)));
                    // Name (+ badges), then workspace, then last-changed.
                    let current = self.active_tab.as_deref() == Some(t.slug.as_str());
                    // Claude Code's own reason ("input needed", the open
                    // dialog's label) when we have it — it says what a bare
                    // "needs input" can't. Under a NEEDS INPUT header the
                    // generic label adds nothing the section doesn't, so it
                    // shows only in the flat groupings.
                    let badge = if t.status == "blocked" {
                        // Always labelled now: blocked and done share a section,
                        // so the chip is what says which one this row is.
                        Some((
                            t.waiting_for.as_deref().unwrap_or("needs input"),
                            C_BLOCKED,
                            C_BADGE_INPUT_BG,
                        ))
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
        _ => C_IDLE,
    }
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "working" => "◐",
        "blocked" => "●",
        "done" => "✔",
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
/// pane: clear + home, then each row as styled cells joined by CRLF.
///
/// It has to be a full frame, and there is no point trying to make it smaller.
/// Before each `render` zellij *wipes* the pane's grid and marks the whole
/// viewport for re-transmission — `plugin_pane.rs` calls
/// `delete_viewport_and_scroll` + `render_full_viewport` and documents it as
/// "part of the plugin contract". So a diff would paint onto a blanked screen,
/// and shaving bytes here changes nothing on the wire: zellij re-encodes the
/// pane from its own grid regardless.
///
/// What that costs is worth stating, because it drives `POLL_SECS` and the
/// absence of a list spinner: **one render == the whole pane re-sent to every
/// attached terminal** (measured on a 120×44 overlay: ~17 KB, so a 9 fps
/// animation was ~156 KB/s and ~210 terminal writes per second). Over ssh that
/// reads as flicker, and at ~1 KB per write it splits a multi-byte glyph across
/// writes several times a second — which any terminal that doesn't reassemble
/// UTF-8 across reads renders as a question mark. Hence: render only when
/// something actually changed (`data_fingerprint`).
fn buf_to_ansi(buf: &Buffer) -> String {
    let area = buf.area;
    // Hide the cursor — nothing here is a text caret, and left visible it sits
    // wherever the paint ended.
    let mut out = String::from("\x1b[?25l\x1b[2J\x1b[H");
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

/// Keys the repo checklist owns, shared by the create and edit forms. Returns
/// false for anything it doesn't consume, so the caller can add its own
/// bindings (↵, esc) on top. Ctrl-n/p mirror the list view's navigation; bare
/// `n` is "none", which only works because the checklist doesn't capture text.
fn pick_key(pick: &mut RepoPick, key: &KeyWithModifier) -> bool {
    let ctrl = key.has_modifiers(&[KeyModifier::Ctrl]);
    match key.bare_key {
        BareKey::Down => pick.move_cursor(1),
        BareKey::Up => pick.move_cursor(-1),
        BareKey::Char('n') if ctrl => pick.move_cursor(1),
        BareKey::Char('p') if ctrl => pick.move_cursor(-1),
        BareKey::Char(' ') | BareKey::Char('x') if key.has_no_modifiers() => pick.toggle(),
        BareKey::Char('a') if key.has_no_modifiers() => pick.set_all(true),
        BareKey::Char('n') if key.has_no_modifiers() => pick.set_all(false),
        _ => return false,
    }
    true
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
