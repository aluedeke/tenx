//! tenx-statusbar: the one-line status bar at the bottom of every tenx tab.
//!
//! ## What it is for
//!
//! It answers "has something happened in a task I am *not* looking at". The
//! overlay answers that too, but only while it's open; `tenx watch` answers it
//! as a desktop notification, but deliberately only for `Blocked` — a popup
//! after every finished turn is noise you stop reading within a day. `Done` is
//! the case a status bar handles well: worth seeing, not worth interrupting for.
//!
//! ## One event at a time, and the last one stays
//!
//! The bar is a single slot, never a list. Showing every waiting task at once
//! was the first cut and it was wrong: a dozen live agents produce a wall of
//! chips nobody reads, repainting constantly. Events queue instead and take the
//! slot in turn (`EVENT_TICKS` each), and when the queue drains the last one
//! *stays* — highlighted while cycling, dimmed once it is merely standing there.
//!
//! That standing marker is the whole point: it means "something wants you, go
//! open the overlay", which is the surface that can actually show you what. It
//! clears only when nothing is waiting any more — the bar is not a notification
//! you dismiss, it goes quiet when the work is genuinely dealt with.
//!
//! It also absorbs the old per-tab header pane (`tenx tab header`): the left
//! third is this tab's own task and status, which is all that pane did, and
//! doing it here returns a screen line and removes one polling process per tab.
//!
//! ## Zero permissions, and why that is the whole design
//!
//! This plugin never calls `request_permission`. A plugin with a request pending
//! is frozen — measured: no pipes delivered, `render` never called — and it is
//! one line tall, so there is nowhere to show the prompt even if we wanted to.
//! Zero permissions means it paints immediately, in every tab, on a machine that
//! has never run tenx, with nothing written into zellij's permission cache.
//!
//! The price is that `ReadApplicationState` is out of reach, so there is no mode
//! indicator (`NORMAL`/`LOCKED`) and no way to know which tab is focused. Neither
//! turned out to matter: keybind hints can be static because tenx generates the
//! keybinds, and "which tab is focused" is answered by construction — only the
//! visible tab's bar is on screen, so *this* instance's task is the one you're
//! looking at and every other task in the payload is one you aren't.
//!
//! ## Where the data comes from
//!
//! `tenx watch` broadcasts a `tenx::status` pipe whenever the resolved state of
//! the session changes (see `cli::watch` and `zellij::pipe_status`). Payload is
//! `{"tasks":[...]}` in the shared `workspace::task_json` shape, newest activity
//! first, carrying only tasks with a live Claude Code session — `Idle` is
//! encoded by absence, so a task not in the list is idle by definition.
//!
//! Without `run_command` there is no way to ask for a snapshot, so a bar that
//! loads mid-session shows nothing until the next change. That is a deliberate
//! trade for the zero-permission property, and it self-corrects on the first
//! state change anywhere in the session.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use std::collections::{BTreeMap, VecDeque};
use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

/// The tenx palette, included from the native crate rather than copied.
///
/// `tenx-zellij` copied its colours as local consts and they have already
/// drifted from the source (its "working" blue is not `INFO`). A `#[path]`
/// include costs nothing here — `palette.rs` depends only on
/// `ratatui::style::Color`, which this crate already has — and makes drift
/// impossible rather than merely discouraged.
#[path = "../../src/palette.rs"]
#[allow(dead_code)] // the palette serves three consumers; this one uses a subset
mod palette;

/// Pipe name `tenx watch` broadcasts on. A pipe sent without `--plugin` reaches
/// every listening plugin in the session, so the name is the only thing that
/// makes this ours; anything else is ignored in `pipe`.
const STATUS_PIPE: &str = "tenx::status";

/// How long each event holds the slot before the next queued one takes it, in
/// timer ticks (1 s each). Long enough to read a task name in peripheral vision;
/// short enough that a burst of five drains in the time it takes to finish a
/// thought rather than scrolling past like a ticker.
const EVENT_TICKS: u32 = 4;

/// One task with a live Claude Code session, from the `tenx::status` payload.
/// Every field is optional-tolerant: the sender is a different binary that can
/// be older or newer than this wasm, and a missing field should cost one column,
/// not the whole bar.
#[derive(Debug, Clone, Deserialize)]
struct Task {
    #[serde(default)]
    ws_dir: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    waiting_for: Option<String>,
    #[serde(default)]
    age_secs: Option<u64>,
}

impl Task {
    fn key(&self) -> String {
        format!("{}/{}", self.ws_dir, self.slug)
    }
    /// Both `Blocked` and `Done` mean an agent is sitting there waiting on you —
    /// `TaskGroup::Waiting` on the native side. That's the set the bar surfaces.
    fn wants_you(&self) -> bool {
        self.status == "blocked" || self.status == "done"
    }
}

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    tasks: Vec<Task>,
}

/// A task that just entered a waiting state — one line of news.
#[derive(Clone)]
struct Event_ {
    /// Task key, so a second transition for the same task replaces the first
    /// rather than queueing behind it.
    key: String,
    text: String,
    color: Color,
}

#[derive(Default)]
struct State {
    /// This tab's task, from the layout's plugin config. Empty on the home tab,
    /// which has no task — there the bar shows events only.
    own_slug: String,
    own_title: String,
    own_ws_dir: String,

    /// Latest payload, newest activity first (the sender guarantees the order).
    tasks: Vec<Task>,
    /// Last seen status per task key, for edge detection. Separate from `tasks`
    /// because a task leaving the payload entirely (going idle) is also a change.
    seen: BTreeMap<String, String>,
    /// The first payload seeds `seen` without announcing. Whatever was already
    /// waiting when this tab opened has been waiting since before it existed —
    /// same reasoning as the watcher priming its notified set.
    primed: bool,

    /// Events not yet shown, oldest first. Drained one at a time.
    queue: VecDeque<Event_>,
    /// The event on screen right now.
    current: Option<Event_>,
    /// Ticks left before `current` gives way to the next queued event. At zero
    /// with an empty queue, `current` stops counting and simply stays.
    left: u32,
    /// Seconds since the last payload, added to each `age_secs` so ages keep
    /// counting between pushes. The sender only pushes on state change, and
    /// "blocked for 20 minutes" is precisely a state that isn't changing.
    since_push: u64,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.own_slug = configuration.get("task").cloned().unwrap_or_default();
        self.own_title = configuration.get("task_title").cloned().unwrap_or_default();
        self.own_ws_dir = configuration.get("ws_dir").cloned().unwrap_or_default();
        // NB: no request_permission — see the module docs. Timer needs no grant.
        subscribe(&[EventType::Timer]);
        set_timeout(1.0);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Timer(_) => {
                set_timeout(1.0);
                self.since_push = self.since_push.saturating_add(1);
                let advanced = self.advance();
                // Repaint only when something visible actually moved: the event
                // slot changed, or this tab's own age is on screen to tick.
                advanced || self.own().is_some_and(|t| t.age_secs.is_some())
            }
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name != STATUS_PIPE {
            return false;
        }
        // Every CLI pipe is followed by a close marker with no payload.
        let Some(body) = message.payload.filter(|b| !b.trim().is_empty()) else {
            return false;
        };
        let Ok(payload) = serde_json::from_str::<Payload>(&body) else {
            return false;
        };

        let incoming: BTreeMap<String, String> =
            payload.tasks.iter().map(|t| (t.key(), t.status.clone())).collect();

        if self.primed {
            for task in &payload.tasks {
                let key = task.key();
                // A task absent from `seen` is one that was idle (or unknown)
                // until now, which is as much an edge as a status change.
                if self.seen.get(&key) != Some(&task.status) {
                    self.announce(task);
                }
            }
        }
        self.primed = true;
        self.seen = incoming;
        self.tasks = payload.tasks;
        self.since_push = 0;

        // Nothing is waiting on you any more, so the standing marker has served
        // its purpose. This is the only thing that clears it — the bar is not a
        // list you dismiss, it goes quiet when the work is actually dealt with.
        if !self.tasks.iter().any(|t| t.wants_you()) {
            self.queue.clear();
            self.current = None;
            self.left = 0;
        }
        self.pull();
        true
    }

    fn render(&mut self, rows: usize, cols: usize) {
        if cols == 0 || rows == 0 {
            return;
        }
        let area = Rect::new(0, 0, cols as u16, 1);
        let mut buf = Buffer::empty(area);
        buf.set_style(area, Style::default().bg(palette::GROUND.color()));
        self.draw(&mut buf, cols as u16);
        // Explicit flush, and it is load-bearing: Rust's stdout is line
        // buffered, and a one-row bar emits no newline at all, so without this
        // every frame after the first sits in the guest's buffer and the pane
        // freezes on whatever it painted first. `tenx-zellij` never hit this —
        // it renders many rows joined by CRLF, which flushes as a side effect.
        use std::io::Write as _;
        let frame = buf_to_ansi(&buf);
        let mut out = std::io::stdout();
        let _ = out.write_all(frame.as_bytes());
        let _ = out.flush();
    }
}

impl State {
    fn own_key(&self) -> String {
        format!("{}/{}", self.own_ws_dir, self.own_slug)
    }

    /// Queue a transition for a task that isn't this tab's. Only into a waiting
    /// state: `working` is the normal course of events and announcing it would
    /// make the bar flicker through every turn of every agent.
    fn announce(&mut self, task: &Task) {
        if !task.wants_you() || task.key() == self.own_key() {
            return;
        }
        let key = task.key();
        let name = if task.title.is_empty() { &task.slug } else { &task.title };
        let (text, color) = if task.status == "blocked" {
            let why = task.waiting_for.as_deref().unwrap_or("needs input");
            (format!("💬 {name} · {why}"), palette::WARN.color())
        } else {
            (format!("✅ {name} finished"), palette::SUCCESS.color())
        };
        // One entry per task. A task that flips blocked→done→blocked while the
        // queue is draining should update its own line, not take three turns.
        self.queue.retain(|e| e.key != key);
        self.queue.push_back(Event_ { key, text, color });
    }

    /// Move the currently shown event to the next queued one if its time is up.
    /// Returns whether the visible slot changed.
    fn advance(&mut self) -> bool {
        if self.left > 0 {
            self.left -= 1;
            if self.left == 0 {
                // Expired. Either the next event takes the slot, or this one
                // stays put as the standing "there is something to look at"
                // marker — `pull` decides, and rendering dims it either way.
                return self.pull() || self.current.is_some();
            }
        }
        self.pull()
    }

    /// Promote the next queued event into the visible slot, if the slot is free
    /// or its dwell time has elapsed. Returns whether anything changed.
    fn pull(&mut self) -> bool {
        if self.left > 0 {
            return false;
        }
        match self.queue.pop_front() {
            Some(next) => {
                self.current = Some(next);
                self.left = EVENT_TICKS;
                true
            }
            None => false,
        }
    }

    /// This tab's own task, if it has a live session.
    fn own(&self) -> Option<&Task> {
        let key = self.own_key();
        self.tasks.iter().find(|t| t.key() == key)
    }

    fn age(&self, task: &Task) -> Option<String> {
        task.age_secs.map(|s| fmt_age(s + self.since_push))
    }

    fn draw(&self, buf: &mut Buffer, width: u16) {
        let mut x = 1u16;

        // ── Left: this tab's task ────────────────────────────────────────────
        if !self.own_slug.is_empty() {
            let title = if self.own_title.is_empty() { &self.own_slug } else { &self.own_title };
            x = put(buf, x, width, title, Style::default().fg(palette::BRIGHT.color()).add_modifier(Modifier::BOLD));
            x = put(buf, x, width, "  ", Style::default());

            let (glyph, label, color) = match self.own().map(|t| t.status.as_str()) {
                Some("blocked") => ("💬", "needs input", palette::WARN.color()),
                Some("done") => ("✅", "waiting for you", palette::SUCCESS.color()),
                Some("working") => ("▷", "working", palette::INFO.color()),
                // No live session in this task's tree — the payload omits idle
                // tasks entirely, so absence is the signal.
                _ => ("·", "idle", palette::MUTED.color()),
            };
            x = put(buf, x, width, &format!("{glyph} {label}"), Style::default().fg(color));
            if let Some(age) = self.own().and_then(|t| self.age(t)) {
                x = put(buf, x, width, &format!(" {age}"), Style::default().fg(palette::MUTED.color()));
            }
        }

        // ── Right: hint, then the single event slot ──────────────────────────
        let hint = "^w tasks";
        let hint_x = width.saturating_sub(hint.len() as u16 + 1);
        put(buf, hint_x, width, hint, Style::default().fg(palette::MUTED.color()));

        // One event at a time, never a list. With a dozen agents running, a list
        // is a wall of chips nobody reads and it repaints constantly; the bar's
        // job is to say "something happened, go look", and the overlay's job is
        // to say what. Queued events take this slot in turn, and once the queue
        // drains the last one *stays* — a standing marker that there is
        // something to check, rather than news that quietly disappears.
        let right_edge = hint_x.saturating_sub(2);
        let Some(event) = &self.current else { return };

        // Highlighted while it's still cycling, dimmed once it's just the
        // standing marker — so a genuinely new event is visibly different from
        // one that has been sitting there, without either shouting.
        let live = self.left > 0;
        let style = if live {
            Style::default().fg(palette::GROUND.color()).bg(event.color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(event.color)
        };
        let text = if live { format!(" {} ", event.text) } else { event.text.clone() };
        // How many more are still queued behind this one. Not a task count —
        // that would be the list again — just an honest "more coming".
        let more = self.queue.len();
        let tail = if more > 0 { format!(" +{more}") } else { String::new() };

        let w = display_width(&text) as u16 + display_width(&tail) as u16;
        let start = right_edge.saturating_sub(w).max(x + 2);
        let after = put(buf, start, right_edge, &text, style);
        if !tail.is_empty() {
            put(buf, after, right_edge, &tail, Style::default().fg(palette::MUTED.color()));
        }
    }
}

/// Write `text` at `x`, clipped at `limit`, returning the next free column.
fn put(buf: &mut Buffer, x: u16, limit: u16, text: &str, style: Style) -> u16 {
    if x >= limit {
        return x;
    }
    let room = (limit - x) as usize;
    buf.set_stringn(x, 0, text, room, style);
    x + display_width(text).min(room) as u16
}

/// Columns a string occupies, from the same table ratatui uses to lay cells out.
/// These two must agree exactly: `put` advances the cursor by this, `set_stringn`
/// consumes cells by ratatui's, and one column of disagreement per glyph pushes
/// the line past the pane width, where it wraps and scrolls the only row away.
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn fmt_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Serialize a one-row `Buffer` to ANSI. No clear-screen: this pane is a single
/// line that zellij repaints in full, and `\x1b[2J` in a 1-row pane makes some
/// terminals blink the whole row.
fn buf_to_ansi(buf: &Buffer) -> String {
    // `\x1b[?7l` disables auto-wrap for this pane. In a one-row grid a single
    // column of overshoot wraps, scrolls the only line out of existence, and
    // leaves a blank bar — the failure looks like "the plugin stopped
    // rendering", not like an off-by-one. Belt to the braces below.
    let mut out = String::from("\x1b[?7l\x1b[2J\x1b[H");
    let mut cur = String::new();
    let mut x = 0u16;
    while x < buf.area.width {
        let Some(cell) = buf.cell((x, 0)) else {
            x += 1;
            continue;
        };
        let sym = cell.symbol();
        // A wide glyph occupies two cells: the symbol in the first, a filler in
        // the second. Emitting both writes three columns for a two-column glyph,
        // so advance past the filler by the symbol's own width rather than
        // stepping one cell at a time.
        if sym.is_empty() {
            x += 1;
            continue;
        }
        let sgr = sgr_for(cell.fg, cell.bg, cell.modifier);
        if sgr != cur {
            out.push_str(&sgr);
            cur = sgr;
        }
        out.push_str(sym);
        x += display_width(sym).max(1) as u16;
    }
    out.push_str("\x1b[0m");
    out
}

fn sgr_for(fg: Color, bg: Color, modifier: Modifier) -> String {
    let mut codes = String::from("0");
    if modifier.contains(Modifier::BOLD) {
        codes.push_str(";1");
    }
    if modifier.contains(Modifier::DIM) {
        codes.push_str(";2");
    }
    push_color(&mut codes, fg, true);
    push_color(&mut codes, bg, false);
    format!("\x1b[{codes}m")
}

fn push_color(codes: &mut String, color: Color, fg: bool) {
    let base = if fg { 38 } else { 48 };
    match color {
        Color::Reset => {}
        Color::Rgb(r, g, b) => codes.push_str(&format!(";{base};2;{r};{g};{b}")),
        Color::Indexed(i) => codes.push_str(&format!(";{base};5;{i}")),
        _ => {}
    }
}
