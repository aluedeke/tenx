//! The README demo, generated rather than recorded: a scripted scene is
//! played against the fixture overlay (keystrokes through the real
//! `handle_key`, plus one change to the fixture rows), every step rendered
//! through ratatui's `TestBackend`, and the frames written out twice — as an
//! animated SVG (one `<g>` per frame, stepped CSS keyframes, no scripting) and
//! as an asciinema v2 cast. Deterministic, touches nothing outside the
//! process, and can't show a real task. `make demo` sets `TENX_DEMO` to
//! write both into `docs/`; the plain test only checks the scene.

use super::screenshot::{
    COLS, PREVIEW_FIXTURE, ROWS, fixture_overlay, hex, plain_text, svg_body, svg_size,
};
use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use std::fmt::Write as _;

struct Frame {
    buf: Buffer,
    hold_ms: u32,
}

struct Scene {
    overlay: Overlay,
    term: Terminal<TestBackend>,
    frames: Vec<Frame>,
}

impl Scene {
    fn new(overlay: Overlay) -> Self {
        let term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        Scene {
            overlay,
            term,
            frames: Vec::new(),
        }
    }

    /// Render the current state and hold it for `hold_ms`.
    fn shot(&mut self, hold_ms: u32) {
        self.sync_preview();
        let Scene {
            overlay,
            term,
            frames,
        } = self;
        term.draw(|f| render(f, overlay)).unwrap();
        frames.push(Frame {
            buf: term.backend().buffer().clone(),
            hold_ms,
        });
    }

    fn key(&mut self, code: KeyCode, hold_ms: u32) {
        let closed = self
            .overlay
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE))
            .unwrap();
        assert!(!closed, "the scene must never close the overlay ({code:?})");
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

    /// What `refresh_preview` would capture from the selected task's pane,
    /// without a pane: the same selection rule (list or command mode on the
    /// Tasks tab), and content chosen by the task's status, so the panel
    /// always shows something that could be on that task's screen.
    fn sync_preview(&mut self) {
        let row = match (self.overlay.tab, &self.overlay.mode) {
            (Tab::Tasks, Mode::List | Mode::Command(_)) => self.overlay.selected_row(),
            _ => None,
        };
        let Some((pane, slug, status)) =
            row.and_then(|r| r.pane.clone().map(|p| (p, r.slug.clone(), r.status)))
        else {
            self.overlay.preview = Preview::default();
            return;
        };
        let text = pane_text(&slug, status);
        self.overlay.preview = Preview {
            pane: Some(pane),
            lines: text.lines().map(super::super::ansi::line).collect(),
            gone: false,
        };
    }

    /// The beat no recording can stage on cue: an agent stops and needs you.
    /// Same edit `refresh_statuses` + `rebuild_rows` would make from the
    /// session registry, applied to the fixture row directly.
    fn agent_stops_and_waits(&mut self, slug: &str, reason: &str) {
        let now = SystemTime::now();
        let row = self
            .overlay
            .rows
            .iter_mut()
            .find(|r| r.slug == slug)
            .expect("fixture row");
        row.status = TaskStatus::Blocked;
        row.group = TaskStatus::Blocked;
        row.section = TaskStatus::Blocked.group();
        row.waiting_for = Some(reason.to_string());
        row.changed = Some(now);
        row.activity = now;
        self.overlay.rows.sort_by(|a, b| {
            a.section
                .rank()
                .cmp(&b.section.rank())
                .then(a.group.rank().cmp(&b.group.rank()))
                .then(b.activity.cmp(&a.activity))
        });
        self.overlay.apply_filter();
    }
}

/// A plausible tail of the task's Claude pane for its status. Two blocked
/// tasks have their own permission dialogs; the rest share a shape.
fn pane_text(slug: &str, status: TaskStatus) -> String {
    match (slug, status) {
        ("add-release-workflow", _) => PREVIEW_FIXTURE.to_string(),
        ("onboarding-emails", TaskStatus::Blocked) => "\
\x1b[1m⏺\x1b[0m The welcome sequence is wired up. I'll send the three
  test emails through the sandbox now.

\x1b[1m Bash command\x1b[0m

   pnpm run email:send --template welcome --to sandbox
   Send the welcome sequence to the sandbox inbox

 Do you want to proceed?
 \x1b[36m❯ 1. Yes\x1b[0m
   2. No
 \x1b[2mEsc to cancel · Tab to amend\x1b[0m
"
        .to_string(),
        (_, TaskStatus::Working) => "\
\x1b[1m⏺\x1b[0m Wiring the welcome sequence into the signup flow.

\x1b[1m⏺\x1b[0m \x1b[2mUpdate\x1b[0m(src/email/welcome.ts)
  ⎿  Updated src/email/welcome.ts with 14 additions

\x1b[1m⏺\x1b[0m \x1b[2mBash\x1b[0m(pnpm test -- email)
  ⎿  Running…
"
        .to_string(),
        (_, TaskStatus::Signaled) => "\
\x1b[1m⏺\x1b[0m Rotated the signing keys. The old key stays valid for
  24 hours so in-flight builds still verify.

\x1b[2m$\x1b[0m make verify
  ✓ 12 artifacts verified against the new key
\x1b[2m$\x1b[0m \x1b[2m# bell: verify finished\x1b[0m
"
        .to_string(),
        (_, TaskStatus::Done) => "\
\x1b[1m⏺\x1b[0m Done. The change is in one commit on this branch; the
  tests pass and I left the PR description in TASK.md.

\x1b[2m❯\x1b[0m
"
        .to_string(),
        _ => String::new(),
    }
}

/// The script. Holds are milliseconds; the whole loop is about 20 seconds.
fn scene() -> Scene {
    let mut s = Scene::new(fixture_overlay());
    s.shot(1600);
    // Move down the list.
    s.key(KeyCode::Char('j'), 350);
    s.key(KeyCode::Char('j'), 350);
    s.key(KeyCode::Char('j'), 600);
    // Filter: `/` to the search field, type, Esc back to the list.
    s.key(KeyCode::Char('/'), 500);
    s.type_str("check", 140);
    s.shot(900);
    s.key(KeyCode::Esc, 600);
    // `dd` asks before deleting; `n` backs out.
    s.key(KeyCode::Char('d'), 200);
    s.key(KeyCode::Char('d'), 1300);
    assert!(s.last_text().contains("delete '"), "confirm prompt");
    s.key(KeyCode::Char('n'), 500);
    // Clear the filter.
    s.key(KeyCode::Char('/'), 250);
    for _ in 0..5 {
        s.key(KeyCode::Backspace, 90);
    }
    s.key(KeyCode::Esc, 500);
    // The `:` command line and its hints.
    s.key(KeyCode::Char(':'), 1200);
    assert!(
        s.last_text().contains("new · open · delete"),
        "command hints"
    );
    s.key(KeyCode::Esc, 700);
    // An agent stops and needs you: the row moves up, the chip appears.
    s.agent_stops_and_waits("onboarding-emails", tenx_core::dialog::PERMISSION_PROMPT);
    s.shot(2400);
    // Find it, land on it, and see that it can be answered from here.
    s.key(KeyCode::Char('/'), 400);
    s.type_str("onb", 160);
    s.key(KeyCode::Esc, 500);
    s.key(KeyCode::Char('k'), 3000);
    assert!(
        s.last_text().contains("y approve"),
        "approve hint\n{}",
        s.last_text()
    );
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
    let _ = writeln!(out, "<title>The tenx overlay, in motion</title>");
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
            let _ = writeln!(
                out,
                "@keyframes k{i}{{0%{{opacity:1}}{p1:.3}%{{opacity:0}}}}"
            );
        } else {
            let _ = writeln!(
                out,
                "@keyframes k{i}{{0%{{opacity:0}}{p0:.3}%{{opacity:1}}{p1:.3}%{{opacity:0}}}}"
            );
        }
        t += f.hold_ms;
    }
    let _ = writeln!(
        out,
        "@media (prefers-reduced-motion:reduce){{.f{{animation:none}}#f0{{opacity:1}}}}"
    );
    let _ = writeln!(out, "</style>");
    let _ = writeln!(
        out,
        r#"<rect width="{width:.0}" height="{height:.0}" rx="6" fill="{}"/>"#,
        palette::GROUND.hex()
    );
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
            let _ = write!(
                out,
                "\x1b[0m{}\x1b[38;2;{fr};{fg_};{fb}m\x1b[48;2;{br};{bg_};{bb}m{text}",
                if bold { "\x1b[1m" } else { "" }
            );
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
        "title": "tenx overlay", "env": {"TERM": "xterm-256color", "SHELL": "/bin/sh"}
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
fn scripted_scene_plays_through_and_lands_on_the_waiting_agent() {
    let s = scene();
    assert!(s.frames.len() > 20, "{} frames", s.frames.len());
    let last = s.last_text();
    assert!(last.contains("onboarding emails"), "{last}");
    assert!(
        last.contains(tenx_core::dialog::PERMISSION_PROMPT),
        "{last}"
    );
    assert!(last.contains("WAITING FOR INPUT"), "{last}");
    // The preview follows the selection: the waiting agent's own dialog.
    assert!(last.contains("pnpm run email:send"), "{last}");
    // Only the filtered row is listed.
    assert!(!last.contains("flaky checkout"), "{last}");

    if std::env::var_os("TENX_DEMO").is_some() {
        let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
        std::fs::write(docs.join("overlay-demo.svg"), animated_svg(&s.frames)).unwrap();
        std::fs::write(docs.join("overlay-demo.cast"), cast(&s.frames)).unwrap();
        eprintln!("wrote {} frames to {}", s.frames.len(), docs.display());
        // Every frame on its own, for eyeballing.
        let dir = std::env::temp_dir().join("tenx-demo-frames");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, f) in s.frames.iter().enumerate() {
            std::fs::write(
                dir.join(format!("f{i:02}.svg")),
                animated_svg(std::slice::from_ref(f)),
            )
            .unwrap();
        }
        eprintln!("frames in {}", dir.display());
    }
}
