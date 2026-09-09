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
        // A workspace for the create form: its name and repos are what the
        // form shows; nothing on disk is touched offline.
        column.workspaces = vec![crate::workspace::Workspace {
            dir: std::path::PathBuf::from("/home/you/ledger"),
            config: crate::workspace::WorkspaceConfig {
                schema_version: crate::workspace::CURRENT_SCHEMA,
                name: "ledger".into(),
                layout: String::new(),
                repos: ["api", "web", "infra"]
                    .iter()
                    .map(|n| crate::workspace::RepoConfig { name: n.to_string(), url: format!("git@github.com:acme/{n}.git") })
                    .collect(),
                age_identity: None,
                agent: String::new(),
                agents: std::collections::HashMap::new(),
            },
        }];
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
        // One row less than the terminal: tmux's status line takes the last.
        let text = pane_text(&slug, row.status, self.answered, rows - 1, cols);
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

/// What a Claude Code session looks like on screen: the header (mark,
/// version and model, working directory), the transcript, the input box
/// near the bottom, and Claude's own status line under it. tmux's status
/// line goes on the last row (`sync_screen`).
struct ClaudeScreen<'a> {
    cwd: &'a str,
    body: Vec<String>,
    /// The line under the input box.
    status: &'a str,
    /// Text already typed into the input box.
    typed: &'a str,
}

fn sgr(c: &palette::Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

const DIM: &str = "\x1b[38;2;120;127;140m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
/// Claude Code's mark, in its salmon.
const MARK: &str = "\x1b[38;2;217;119;87m";

impl ClaudeScreen<'_> {
    fn render(&self, rows: u16, cols: u16) -> String {
        let cols = cols as usize;
        let mut out = String::new();
        let mut lines: Vec<String> = vec![
            format!(" {MARK}▐▛███▜▌{RESET}   {BOLD}Claude Code{RESET} {DIM}v2.1.263{RESET}"),
            format!("{MARK}▝▜█████▛▘{RESET}  Fable 5.1 {DIM}·{RESET} Claude Max"),
            format!("  {MARK}▘▘ ▝▝{RESET}    {DIM}{}{RESET}", self.cwd),
            String::new(),
        ];
        lines.extend(self.body.iter().cloned());
        // Header and transcript from the top; the input box and status
        // pinned to the bottom, above tmux's line.
        for (i, l) in lines.iter().enumerate() {
            let _ = write!(out, "\x1b[{};1H{l}", i + 1);
        }
        let inner = cols.saturating_sub(2);
        let top = rows.saturating_sub(4);
        let _ = write!(out, "\x1b[{top};1H{DIM}╭{}╮{RESET}", "─".repeat(inner));
        let _ = write!(out, "\x1b[{};1H{DIM}│{RESET} {}> {}{}{DIM}│{RESET}", top + 1, sgr(&palette::TEXT), self.typed, " ".repeat(inner.saturating_sub(3 + self.typed.chars().count())));
        let _ = write!(out, "\x1b[{};1H{DIM}╰{}╯{RESET}", top + 2, "─".repeat(inner));
        let _ = write!(out, "\x1b[{};1H  {DIM}{}{RESET}", top + 3, self.status);
        // The cursor in the input box, after what was typed.
        let _ = write!(out, "\x1b[{};{}H", top + 1, 5 + self.typed.chars().count());
        out
    }
}

fn tool(name: &str, arg: &str) -> String {
    format!("{BOLD}⏺{RESET} {BOLD}{name}{RESET}{DIM}({arg}){RESET}")
}

fn result(text: &str) -> String {
    format!("  {DIM}⎿{RESET}  {text}")
}

fn say(text: &str) -> String {
    format!("{BOLD}⏺{RESET} {text}")
}

fn spinner(verb: &str, secs: &str, tokens: &str) -> String {
    format!("{}✻{RESET} {DIM}{verb}… ({secs} · ↑ {tokens} tokens · esc to interrupt){RESET}", sgr(&palette::ACCENT))
}

const STATUS_ACCEPT: &str = "⏵⏵ accept edits on (shift+tab to cycle) · ? for shortcuts";
const STATUS_PLAN: &str = "⏸ plan mode on (shift+tab to cycle) · ? for shortcuts";

/// The screen for a task's Claude session, by task and status (`answered`:
/// its permission prompt was approved from the column). Every session
/// looks like its own: different work, different point in the turn.
fn pane_text(slug: &str, status: TaskStatus, answered: bool, rows: u16, cols: u16) -> String {
    let permission = |summary: &str, cmd: &str, what: &str| -> Vec<String> {
        vec![
            say(summary),
            String::new(),
            format!("{BOLD} Bash command{RESET}"),
            String::new(),
            format!("   {cmd}"),
            format!("   {DIM}{what}{RESET}"),
            String::new(),
            " Do you want to proceed?".into(),
            format!(" {}❯ 1. Yes{RESET}", sgr(&palette::INFO)),
            "   2. No".into(),
            format!(" {DIM}Esc to cancel · Tab to amend{RESET}"),
        ]
    };
    let (cwd, body, st, typed): (&str, Vec<String>, &str, &str) = match (slug, status) {
        ("onboarding-emails", _) if answered => (
            "~/ledger/tasks/onboarding-emails",
            vec![
                say("The welcome sequence is wired up and the unit tests pass. I'll send the three test emails through the sandbox now."),
                String::new(),
                tool("Bash", "pnpm run email:send --template welcome --to sandbox"),
                result("Sent 3 emails to sandbox@acme.test (welcome, day-2, day-7)"),
                String::new(),
                tool("Read", "src/email/templates/welcome.html"),
                result("Read 86 lines"),
                String::new(),
                spinner("Checking the rendered HTML against the design", "4s", "1.2k"),
            ],
            STATUS_ACCEPT,
            "",
        ),
        ("onboarding-emails", TaskStatus::Working) => (
            "~/ledger/tasks/onboarding-emails",
            vec![
                say("I'll wire the welcome sequence into the signup flow and cover it with a test."),
                String::new(),
                tool("Read", "src/signup/complete.ts"),
                result("Read 142 lines"),
                String::new(),
                tool("Update", "src/email/welcome.ts"),
                result("Updated src/email/welcome.ts with 14 additions and 2 removals"),
                String::new(),
                tool("Bash", "pnpm test -- email"),
                result("Running…"),
                String::new(),
                spinner("Wiring", "38s", "6.4k"),
            ],
            STATUS_ACCEPT,
            "",
        ),
        ("onboarding-emails", TaskStatus::Blocked) => (
            "~/ledger/tasks/onboarding-emails",
            permission(
                "The welcome sequence is wired up and the unit tests pass. I'll send the three test emails through the sandbox now.",
                "pnpm run email:send --template welcome --to sandbox",
                "Send the welcome sequence to the sandbox inbox",
            ),
            STATUS_ACCEPT,
            "",
        ),
        ("add-release-workflow", _) => (
            "~/tenx-workspace/tasks/add-release-workflow",
            permission(
                "I'll add the release workflow next to the CI one and wire the tag push to it.",
                "gh workflow run release.yml --ref v0.2.0",
                "Kick off the release workflow for the tag",
            ),
            STATUS_ACCEPT,
            "",
        ),
        ("rotate-signing-keys", _) => (
            "~/infra/tasks/rotate-signing-keys",
            vec![
                say("Rotated the signing keys. The old key stays valid for 24 hours so in-flight builds still verify."),
                String::new(),
                tool("Bash", "make verify"),
                result("✓ 12 artifacts verified against the new key"),
                String::new(),
                tool("Bash", "printf '\\a'"),
                result(&format!("{DIM}(bell){RESET}")),
                String::new(),
                say("Done — the rotation is complete and verified. Say the word and I'll revoke the old key early."),
            ],
            STATUS_ACCEPT,
            "",
        ),
        ("csv-export", _) => (
            "~/ledger/tasks/csv-export",
            vec![
                say("Done. The export streams rows instead of buffering the whole table, so the 2M-row ledger no longer times out."),
                String::new(),
                format!("  {DIM}Summary of the change:{RESET}"),
                "  1. `exportCsv` writes through a `Transform` stream with a 4 KB buffer.".into(),
                "  2. The integration test covers 250k rows and finishes in 1.8s.".into(),
                "  3. PR #31 is open with the description from TASK.md.".into(),
                String::new(),
                format!("{}✻{RESET} {DIM}Churned for 2m 14s · done{RESET}", sgr(&palette::ACCENT)),
            ],
            STATUS_ACCEPT,
            "",
        ),
        ("column-screenshot", _) => (
            "~/tenx-workspace/tasks/column-screenshot",
            vec![
                say("Rendering the fixture through the real widgets and diffing the SVG against the checked-in one."),
                String::new(),
                tool("Edit", "src/tui/column/screenshot.rs"),
                result("Updated src/tui/column/screenshot.rs with 6 additions and 3 removals"),
                String::new(),
                tool("Bash", "make screenshot"),
                result("Running…"),
                String::new(),
                spinner("Rendering", "12s", "2.9k"),
            ],
            STATUS_PLAN,
            "",
        ),
        ("rate-limit-alerts", _) => (
            "~/ledger/tasks/rate-limit-alerts",
            vec![
                format!("{DIM}╭─────────────────────────────────────────────────────────╮{RESET}"),
                format!("{DIM}│{RESET} {}✻{RESET} Welcome to {BOLD}Claude Code{RESET}!                                    {DIM}│{RESET}", sgr(&palette::ACCENT)),
                format!("{DIM}│{RESET}                                                         {DIM}│{RESET}"),
                format!("{DIM}│{RESET}   /help for help, /status for your current setup        {DIM}│{RESET}"),
                format!("{DIM}│{RESET}                                                         {DIM}│{RESET}"),
                format!("{DIM}│{RESET}   cwd: ~/ledger/tasks/rate-limit-alerts                 {DIM}│{RESET}"),
                format!("{DIM}╰─────────────────────────────────────────────────────────╯{RESET}"),
                String::new(),
                format!(" {DIM}Tip: use /tenx to see the task's notes and links{RESET}"),
            ],
            STATUS_ACCEPT,
            "",
        ),
        (_, TaskStatus::Done) => (
            "~/tasks/task",
            vec![say("Done. The change is in one commit on this branch; the tests pass and I left the PR description in TASK.md.")],
            STATUS_ACCEPT,
            "",
        ),
        _ => ("~/tasks/task", vec![say("Waiting for a task.")], STATUS_ACCEPT, ""),
    };
    ClaudeScreen { cwd, body, status: st, typed }.render(rows, cols)
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
    assert!(s.last_text().contains("Churned for 2m 14s"), "csv export's screen\n{}", s.last_text());
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
    s.key(KeyCode::Esc, 2200);
    // A new task: `n` opens the form in the column — name, repos to check
    // out — and ⏎ creates it: a branch and worktree in each repo, a
    // TASK.md, and a window with Claude Code already started.
    s.ctrl_w(700);
    s.key(KeyCode::Char('n'), 1200);
    assert!(s.last_text().contains(" new task "), "the create form\n{}", s.last_text());
    s.type_str("rate limit alerts", 90);
    s.shot(700);
    s.key(KeyCode::Tab, 500);
    s.key(KeyCode::Tab, 500);
    s.key(KeyCode::Tab, 400);
    s.key(KeyCode::Char(' '), 900); // not infra, this time
    s.key(KeyCode::Enter, 1600);
    assert_eq!(s.client.focus, Focus::Terminal, "the new task takes the keyboard");
    assert!(s.last_text().contains("Welcome to Claude Code"), "a fresh session\n{}", s.last_text());
    s.type_str("alert when a customer hits 80% of their rate limit", 55);
    s.shot(3200);
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
    assert!(s.frames.len() > 60, "{} frames", s.frames.len());
    let first = plain_text(&s.frames[0].buf);
    assert!(first.contains("Claude Code") && first.contains("welcome sequence"), "a Claude session beside the column:\n{first}");
    assert!(first.contains("onboarding emails") && first.contains("WORKING"), "{first}");
    let approved = s.frames.iter().any(|f| plain_text(&f.buf).contains("approved 'onboarding emails'"));
    assert!(approved, "the prompt is answered from the column");
    let last = s.last_text();
    assert!(last.contains("rate limit alerts") && last.contains("80% of their rate limit"), "the new task, with a prompt typed:\n{last}");

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
            std::fs::write(dir.join(format!("f{i:03}.svg")), animated_svg(std::slice::from_ref(f))).unwrap();
            std::fs::write(dir.join(format!("f{i:03}.txt")), plain_text(&f.buf)).unwrap();
        }
        eprintln!("frames in {}", dir.display());
    }
}
