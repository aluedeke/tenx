//! The README demo, generated rather than recorded: a scripted client
//! session — the column beside a task — played through the real client
//! (`Client::draw`, `Client::handle_key`), with the task side a scripted
//! screen (`Script`: a `vt100` parser fed fixture transcripts) and the column
//! offline (no tmux, no registry: a switch or an answer only updates it).
//! Every step is rendered through ratatui's `TestBackend`, and the frames are
//! written out twice — as an animated SVG (one `<g>` per frame, stepped CSS
//! keyframes, no scripting) and as an asciinema v2 cast. Deterministic,
//! touches nothing outside the process, and can't show a real task. `make
//! demo` sets `TENX_DEMO` to write both into `docs/`; the plain test only
//! checks the scene.

use super::screenshot::{fixture_column, hex, plain_text, svg_body, svg_size};
use super::*;
use crate::tui::client::{Client, Focus};
use crate::tui::term::{TaskScreen, encode_key, render_screen};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

/// The demo terminal: a desktop window.
const COLS: u16 = 150;
const ROWS: u16 = 40;

/// The task side: a screen the scene writes to. Keys typed into it echo,
/// so typing shows; nothing else happens.
struct Script {
    parser: Rc<RefCell<vt100::Parser>>,
}

impl TaskScreen for Script {
    fn render(&self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        render_screen(self.parser.borrow().screen(), area, buf)
    }
    fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.borrow_mut().set_size(rows, cols);
    }
    fn write(&mut self, bytes: &[u8]) {
        self.parser.borrow_mut().process(bytes);
    }
    fn key_bytes(&self, key: &KeyEvent) -> Option<Vec<u8>> {
        encode_key(key, false)
    }
    fn mouse_bytes(&self, _: &MouseEvent, _: u16, _: u16) -> Option<Vec<u8>> {
        None
    }
    fn paste(&mut self, text: &str) {
        self.write(text.as_bytes());
    }
    fn alive(&self) -> bool {
        true
    }
    fn take_bell(&self) -> bool {
        false
    }
    fn take_clipboard(&self) -> Vec<String> {
        Vec::new()
    }
    fn kill(&mut self) {}
}

struct Frame {
    buf: Buffer,
    hold_ms: u32,
}

struct Scene {
    client: Client,
    screen: Rc<RefCell<vt100::Parser>>,
    /// The task the screen currently shows, so a switch is noticed.
    shown: Option<String>,
    /// The waiting agent was answered from the column: its screen moves on.
    answered: bool,
    term: Terminal<TestBackend>,
    frames: Vec<Frame>,
}

impl Scene {
    fn new() -> Self {
        let mut column = fixture_column();
        column.offline = true;
        column.current = Some("onboarding-emails".into());
        column.focus_search();
        column.selected = 0;
        let screen = Rc::new(RefCell::new(vt100::Parser::new(ROWS, COLS, 0)));
        let client = Client::new(column, Box::new(Script { parser: screen.clone() }), COLS, ROWS, 0);
        let term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        let mut s = Scene { client, screen, shown: None, answered: false, term, frames: Vec::new() };
        s.sync_screen();
        s
    }

    /// Render the current state and hold it for `hold_ms`.
    fn shot(&mut self, hold_ms: u32) {
        self.sync_screen();
        let Scene { client, term, frames, .. } = self;
        term.draw(|f| client.draw(f)).unwrap();
        frames.push(Frame { buf: term.backend().buffer().clone(), hold_ms });
    }

    fn key(&mut self, code: KeyCode, hold_ms: u32) {
        self.client.handle_key(KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
        self.shot(hold_ms);
    }

    fn ctrl_w(&mut self, hold_ms: u32) {
        self.client.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)).unwrap();
        self.shot(hold_ms);
    }

    fn type_str(&mut self, s: &str, per_char_ms: u32) {
        for c in s.chars() {
            self.key(KeyCode::Char(c), per_char_ms);
        }
    }

    fn last_text(&self) -> String {
        plain_text(&self.frames.last().expect("a frame").buf)
    }

    /// What the embedded session would show: the current task's screen,
    /// redrawn whenever the current task (or its status) changed — the same
    /// switch tmux performs under the real client.
    fn sync_screen(&mut self) {
        let column = &self.client.column;
        let Some(slug) = column.current.clone() else { return };
        let row = column.rows.iter().find(|r| r.slug == slug).expect("fixture row");
        let key = format!("{slug}/{:?}/{}", row.status, self.answered);
        if self.shown.as_deref() == Some(key.as_str()) {
            return;
        }
        let (rows, cols) = self.screen.borrow().screen().size();
        // A pty would translate `\n` to `\r\n` on the way out; do it here.
        let text = pane_text(&slug, row.status, self.answered).replace('\n', "\r\n");
        let mut out = format!("\x1b[2J\x1b[H{text}");
        // tmux's status line, as the generated config draws it: glyph in
        // its status colour, the title bold, the workspace muted.
        let sgr = |c: &palette::Rgb| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2);
        let left = format!(
            " {}{} {}\x1b[1m{}\x1b[22m {}{} ",
            sgr(palette::status_color(row.status)),
            row.status.glyph(),
            sgr(&palette::TEXT),
            row.title,
            sgr(&palette::MUTED),
            row.ws_name
        );
        let right = format!("{}● 1 need input{} · {}✔ 3 waiting ", sgr(&palette::WARN), sgr(&palette::MUTED), sgr(&palette::SUCCESS));
        // DECSC/DECRC around the status line, so typing lands back at the
        // prompt.
        let _ = write!(out, "\x1b7\x1b[{rows};1H{left}\x1b[{rows};{}H{right}\x1b[0m\x1b8", cols.saturating_sub(30));
        self.screen.borrow_mut().process(out.as_bytes());
        self.shown = Some(key);
    }

    /// The beat no recording can stage on cue: an agent stops and needs you.
    /// The same edit `refresh_statuses` + `tidy` would make from the session
    /// registry, applied to the fixture row directly — and, as under the
    /// real client, the column re-groups at once with the selection kept.
    fn agent_stops_and_waits(&mut self, slug: &str, reason: &str) {
        let now = SystemTime::now();
        let column = &mut self.client.column;
        let row = column.rows.iter_mut().find(|r| r.slug == slug).expect("fixture row");
        row.status = TaskStatus::Blocked;
        row.group = TaskStatus::Blocked;
        row.section = TaskStatus::Blocked.group();
        row.waiting_for = Some(reason.to_string());
        row.changed = Some(now);
        row.activity = now;
        Self::regroup(column);
    }

    /// Re-group the fixture rows the way `tidy` would, without the registry.
    fn regroup(column: &mut Column) {
        let keep = column.selected_row().map(|r| r.slug.clone());
        for r in column.rows.iter_mut() {
            r.group = r.status;
            if r.secrets_pending.is_empty() && r.secrets_pending_set.is_empty() {
                r.section = r.status.group();
            }
        }
        column.sort_rows();
        column.apply_filter();
        if let Some(slug) = keep
            && let Some(pos) = column.filtered.iter().position(|&i| column.rows[i].slug == slug)
        {
            column.selected = pos;
        }
    }
}

/// A plausible screen for the task's Claude pane, by task and status
/// (`answered`: its permission prompt was approved from the column).
fn pane_text(slug: &str, status: TaskStatus, answered: bool) -> String {
    let prompt = "\n\x1b[38;2;120;127;140m╭────────────────────────────────────────────────────────────────────────╮\x1b[0m\n\x1b[38;2;120;127;140m│\x1b[0m > \x1b[38;2;120;127;140m│\x1b[0m\n\x1b[38;2;120;127;140m╰────────────────────────────────────────────────────────────────────────╯\x1b[0m\x1b[2A\x1b[4C";
    match (slug, status) {
        ("onboarding-emails", _) if answered => format!(
            "\x1b[1m⏺\x1b[0m \x1b[2mBash\x1b[0m(pnpm run email:send --template welcome --to sandbox)\n  ⎿  Sent 3 emails to sandbox@acme.test\n\n\
             \x1b[1m⏺\x1b[0m Checking the rendered HTML against the design once more…\n{prompt}"
        ),
        ("onboarding-emails", TaskStatus::Working) => format!(
            "\x1b[1m⏺\x1b[0m Wiring the welcome sequence into the signup flow.\n\n\
             \x1b[1m⏺\x1b[0m \x1b[2mUpdate\x1b[0m(src/email/welcome.ts)\n  ⎿  Updated src/email/welcome.ts with 14 additions\n\n\
             \x1b[1m⏺\x1b[0m \x1b[2mBash\x1b[0m(pnpm test -- email)\n  ⎿  Running…\n{prompt}"
        ),
        ("onboarding-emails", TaskStatus::Blocked) => "\
\x1b[1m⏺\x1b[0m The welcome sequence is wired up and the unit tests pass. I'll send
  the three test emails through the sandbox now.

\x1b[1m Bash command\x1b[0m

   pnpm run email:send --template welcome --to sandbox
   Send the welcome sequence to the sandbox inbox

 Do you want to proceed?
 \x1b[36m❯ 1. Yes\x1b[0m
   2. No
 \x1b[2mEsc to cancel · Tab to amend\x1b[0m
"
        .to_string(),
        ("add-release-workflow", _) => "\
\x1b[1m⏺\x1b[0m I'll add the release workflow next to the CI one and wire the
  tag push to it.

\x1b[1m Bash command\x1b[0m

   gh workflow run release.yml --ref v0.2.0
   Kick off the release workflow for the tag

 Do you want to proceed?
 \x1b[36m❯ 1. Yes\x1b[0m
   2. No
 \x1b[2mEsc to cancel · Tab to amend\x1b[0m
"
        .to_string(),
        ("rotate-signing-keys", _) => "\
\x1b[1m⏺\x1b[0m Rotated the signing keys. The old key stays valid for
  24 hours so in-flight builds still verify.

\x1b[2m$\x1b[0m make verify
  ✓ 12 artifacts verified against the new key
\x1b[2m$\x1b[0m \x1b[2m# bell: verify finished\x1b[0m
\x1b[2m$\x1b[0m "
        .to_string(),
        (_, TaskStatus::Done) => format!(
            "\x1b[1m⏺\x1b[0m Done. The change is in one commit on this branch; the tests pass\n  and I left the PR description in TASK.md.\n{prompt}"
        ),
        (_, TaskStatus::Working) => format!(
            "\x1b[1m⏺\x1b[0m Rendering the fixture through the real widgets and diffing the\n  SVG against the checked-in one.\n\n\
             \x1b[1m⏺\x1b[0m \x1b[2mBash\x1b[0m(make screenshot)\n  ⎿  Running…\n{prompt}"
        ),
        _ => format!("\x1b[1m⏺\x1b[0m Waiting for a task.\n{prompt}"),
    }
}

/// The script. Holds are milliseconds; the whole loop is about 24 seconds.
fn scene() -> Scene {
    let mut s = Scene::new();
    // A task at work, the column beside it.
    s.shot(1800);
    // Ctrl+w: into the column, on the task you are in; ↑ walks the tasks
    // above and the session follows.
    s.ctrl_w(900);
    s.key(KeyCode::Up, 1100);
    s.key(KeyCode::Up, 1100);
    assert!(s.last_text().contains("Done. The change is in one commit"), "csv export's screen\n{}", s.last_text());
    // A filter, and ⏎ opens the match: the keyboard is in the task now.
    s.key(KeyCode::Char('/'), 400);
    s.type_str("rota", 140);
    s.key(KeyCode::Enter, 1200);
    assert_eq!(s.client.focus, Focus::Terminal, "⏎ hands the keyboard to the task");
    // Typing goes to the task.
    s.type_str("git log --oneline -3", 70);
    s.shot(1400);
    // An agent stops and needs you: its row moves up at once.
    s.agent_stops_and_waits("onboarding-emails", tenx_core::dialog::PERMISSION_PROMPT);
    s.shot(2000);
    // Ctrl+w lands on the task you are in; ↑ twice reaches the one waiting.
    s.ctrl_w(700);
    s.key(KeyCode::Up, 1000);
    s.key(KeyCode::Up, 2200);
    assert!(s.last_text().contains("y/N answer"), "answerable from the column\n{}", s.last_text());
    // Answer it from here; the task carries on.
    s.client.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)).unwrap();
    s.answered = true;
    Scene::regroup(&mut s.client.column);
    s.shot(2400);
    // Back to the task.
    s.key(KeyCode::Esc, 3000);
    s
}

/// Stepped CSS keyframes show one frame group at a time; no script, so the
/// file animates inside an `<img>` on GitHub. Reduced-motion viewers get the
/// first frame.
fn animated_svg(frames: &[Frame]) -> String {
    let (width, height) = svg_size(&frames[0].buf);
    let total: u32 = frames.iter().map(|f| f.hold_ms).sum();
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}" font-family="JetBrains Mono, SF Mono, Menlo, Consolas, DejaVu Sans Mono, monospace" font-size="14">"#
    );
    let _ = writeln!(out, "<title>A tenx session, in motion</title>");
    let _ = writeln!(out, "<style>");
    let _ = writeln!(
        out,
        ".f{{opacity:0;animation-duration:{:.2}s;animation-timing-function:step-end;animation-iteration-count:infinite}}",
        total as f32 / 1000.0
    );
    let mut t = 0u32;
    for (i, f) in frames.iter().enumerate() {
        let p0 = t as f32 / total as f32 * 100.0;
        let p1 = (t + f.hold_ms) as f32 / total as f32 * 100.0;
        let _ = writeln!(out, "#f{i}{{animation-name:k{i}}}");
        if i == 0 {
            let _ = writeln!(out, "@keyframes k{i}{{0%{{opacity:1}}{p1:.3}%{{opacity:0}}}}");
        } else {
            let _ = writeln!(out, "@keyframes k{i}{{0%{{opacity:0}}{p0:.3}%{{opacity:1}}{p1:.3}%{{opacity:0}}}}");
        }
        t += f.hold_ms;
    }
    let _ = writeln!(out, "@media (prefers-reduced-motion:reduce){{.f{{animation:none}}#f0{{opacity:1}}}}");
    let _ = writeln!(out, "</style>");
    let _ = writeln!(out, r#"<rect width="{width:.0}" height="{height:.0}" rx="6" fill="{}"/>"#, palette::GROUND.hex());
    for (i, f) in frames.iter().enumerate() {
        let _ = writeln!(out, r#"<g id="f{i}" class="f">"#);
        out.push_str(&svg_body(&f.buf));
        let _ = writeln!(out, "</g>");
    }
    out.push_str("</svg>\n");
    out
}

/// One frame as a full-screen redraw in 24-bit SGR escapes.
fn ansi_frame(buf: &Buffer) -> String {
    let (w, h) = (buf.area.width, buf.area.height);
    let mut out = String::from("\x1b[?25l\x1b[H");
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let first = buf.cell((x, y)).expect("cell in area");
            let (fg, bg, bold) = (first.fg, first.bg, first.modifier.contains(Modifier::BOLD));
            let mut text = String::new();
            while x < w {
                let c = buf.cell((x, y)).expect("cell in area");
                if c.fg != fg || c.bg != bg || c.modifier.contains(Modifier::BOLD) != bold {
                    break;
                }
                let sym = c.symbol();
                text.push_str(sym);
                x += sym.width().max(1) as u16;
            }
            let (fr, fg_, fb) = rgb(hex(fg, &palette::TEXT));
            let (br, bg_, bb) = rgb(hex(bg, &palette::GROUND));
            let _ = write!(out, "\x1b[0m{}\x1b[38;2;{fr};{fg_};{fb}m\x1b[48;2;{br};{bg_};{bb}m{text}", if bold { "\x1b[1m" } else { "" });
        }
        if y + 1 < h {
            out.push_str("\x1b[0m\r\n");
        }
    }
    out.push_str("\x1b[0m");
    out
}

fn rgb(hex: String) -> (u8, u8, u8) {
    let v = u32::from_str_radix(&hex[1..], 16).expect("hex colour");
    ((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// asciinema v2: a JSON header line, then one `[time, "o", data]` event per
/// frame, each a full redraw.
fn cast(frames: &[Frame]) -> String {
    let mut out = serde_json::json!({
        "version": 2, "width": COLS, "height": ROWS, "timestamp": 0,
        "title": "tenx", "env": {"TERM": "xterm-256color", "SHELL": "/bin/sh"}
    })
    .to_string();
    out.push('\n');
    let mut t = 0u32;
    for (i, f) in frames.iter().enumerate() {
        let mut data = ansi_frame(&f.buf);
        if i == 0 {
            data.insert_str(0, "\x1b[2J");
        }
        let ev = serde_json::json!([t as f64 / 1000.0, "o", data]);
        out.push_str(&ev.to_string());
        out.push('\n');
        t += f.hold_ms;
    }
    out
}

#[test]
fn scripted_session_switches_filters_and_answers_from_the_column() {
    let s = scene();
    assert!(s.frames.len() > 25, "{} frames", s.frames.len());
    let first = plain_text(&s.frames[0].buf);
    assert!(first.contains("Wiring the welcome sequence"), "the task beside the column:\n{first}");
    assert!(first.contains("onboarding emails") && first.contains("WORKING"), "{first}");
    let last = s.last_text();
    assert!(last.contains("approved 'onboarding emails'"), "{last}");
    assert!(last.contains("Sent 3 emails"), "the task carried on:\n{last}");

    if std::env::var_os("TENX_DEMO").is_some() {
        let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
        std::fs::write(docs.join("demo.svg"), animated_svg(&s.frames)).unwrap();
        std::fs::write(docs.join("demo.cast"), cast(&s.frames)).unwrap();
        eprintln!("wrote {} frames to {}", s.frames.len(), docs.display());
        // Every frame on its own, for eyeballing.
        let dir = std::env::temp_dir().join("tenx-demo-frames");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, f) in s.frames.iter().enumerate() {
            std::fs::write(dir.join(format!("f{i:02}.svg")), animated_svg(std::slice::from_ref(f))).unwrap();
            std::fs::write(dir.join(format!("f{i:02}.txt")), plain_text(&f.buf)).unwrap();
        }
        eprintln!("frames in {}", dir.display());
    }
}
