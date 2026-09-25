//! The README screenshot, generated from the real widgets: the column is
//! rendered with fixture rows into ratatui's `TestBackend` and the buffer is
//! written out cell by cell as an SVG. Nothing here touches disk, tmux or
//! Claude Code's session registry, so the picture never leaks a real task,
//! and it can't drift from what the column actually draws. The plain test
//! only checks the render; `make screenshot` sets `TENX_SCREENSHOT` to write
//! `docs/column.svg`. The fixtures and SVG helpers also serve `demo.rs`.

use super::*;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use std::fmt::Write as _;
use tenx_core::live::{Live, PrInfo};

/// The column's width and height in the screenshot and the demo.
pub(super) const COLS: u16 = 36;
pub(super) const ROWS: u16 = 48;

/// One fixture task. `age` is seconds since its last status change.
struct Fx {
    title: &'static str,
    ws: &'static str,
    status: TaskStatus,
    age: u64,
    open: bool,
    prs: Vec<PrInfo>,
    ports: Vec<u16>,
    waiting_for: Option<&'static str>,
    secrets: Vec<&'static str>,
}

fn fx(title: &'static str, ws: &'static str, status: TaskStatus) -> Fx {
    Fx {
        title,
        ws,
        status,
        age: 0,
        open: false,
        prs: vec![],
        ports: vec![],
        waiting_for: None,
        secrets: vec![],
    }
}

fn pr(number: u64, state: &str, draft: bool, checks: &str) -> PrInfo {
    PrInfo {
        repo: "repo".into(),
        number,
        state: state.into(),
        url: String::new(),
        draft,
        review: String::new(),
        checks: checks.into(),
    }
}

fn row(f: Fx) -> Row {
    let slug = f.title.to_lowercase().replace(' ', "-");
    let changed =
        (f.status != TaskStatus::Idle).then(|| SystemTime::now() - Duration::from_secs(f.age));
    let section = if f.secrets.is_empty() {
        f.status.group()
    } else {
        workspace::TaskGroup::SecretsPending
    };
    Row {
        pending: false,
        ws_idx: 0,
        ws_name: f.ws.into(),
        path: PathBuf::from(format!("/home/you/{}/tasks/{slug}", f.ws)),
        slug,
        title: f.title.into(),
        status: f.status,
        group: f.status,
        changed,
        waiting_for: f.waiting_for.map(str::to_string),
        activity: changed.unwrap_or(SystemTime::UNIX_EPOCH),
        window_id: f.open.then(|| "@1".to_string()),
        pane: (f.open && f.status != TaskStatus::Idle).then(|| "%1".to_string()),
        live: Live {
            ports: f.ports,
            prs: f.prs,
            pr_checked: 0,
        },
        repos: vec![],
        agent: crate::agent::AgentKind::Claude,
        secrets_pending: f.secrets.iter().map(|s| s.to_string()).collect(),
        secrets_pending_set: vec![],
        section,
        subagents: vec![],
    }
}

/// Every section and every kind of chip, on invented tasks. Rows are listed
/// in display order (section, then status rank, then recency), as
/// `rebuild_rows` would sort them.
fn subagent(id: &str, ty: &str, description: &str, status: SubagentStatus, age: u64) -> Subagent {
    let at = SystemTime::now() - Duration::from_secs(age);
    Subagent {
        id: id.into(),
        session_pid: 1,
        agent: "claude".into(),
        agent_type: ty.into(),
        description: Some(description.into()),
        status,
        waiting_for: None,
        started_at: Some(at),
        updated_at: Some(at),
        transcript_path: Some(PathBuf::from(format!("/home/you/.claude/projects/x/s/subagents/agent-{id}.jsonl"))),
        background: false,
    }
}

pub(super) fn fixture_column() -> Column {
    use TaskStatus::*;
    let m = 60;
    let h = 3600;
    let rows = vec![
        Fx {
            secrets: vec!["STRIPE_WEBHOOK_SECRET"],
            open: true,
            ..fx("stripe webhook signing", "ledger", Idle)
        },
        Fx {
            waiting_for: Some("permission: Bash"),
            open: true,
            age: 4 * m,
            ..fx("add release workflow", "tenx-workspace", Blocked)
        },
        Fx {
            open: true,
            age: 22 * h,
            ..fx("rotate signing keys", "infra", Signaled)
        },
        Fx {
            open: true,
            age: 54 * m,
            ports: vec![8080],
            ..fx("rate limit middleware", "acme-api", Done)
        },
        Fx {
            open: true,
            age: 2 * h,
            ..fx("flaky checkout e2e", "storefront", Done)
        },
        Fx {
            open: true,
            age: 23 * h,
            prs: vec![pr(31, "OPEN", false, "success")],
            ..fx("csv export", "ledger", Done)
        },
        Fx {
            open: true,
            ..fx("column screenshot", "tenx-workspace", Working)
        },
        Fx {
            open: true,
            prs: vec![pr(24, "MERGED", false, "success")],
            ..fx("onboarding emails", "ledger", Working)
        },
        fx("cart abandonment banner", "storefront", Idle),
        fx("terraform drift check", "infra", Idle),
        fx("homebrew tap", "tenx-workspace", Idle),
        Fx {
            prs: vec![
                pr(781, "MERGED", false, "success"),
                pr(404, "MERGED", false, "success"),
            ],
            ..fx("ACME-2244 search ranking", "acme-api", Idle)
        },
        fx("image cdn migration", "storefront", Idle),
        fx("upgrade postgres 17", "infra", Idle),
        Fx {
            prs: vec![pr(14, "OPEN", false, "pending")],
            ..fx("multi-currency totals", "ledger", Idle)
        },
        fx("backup restore drill", "homelab", Idle),
        fx("api reference sweep", "docs", Idle),
        Fx {
            prs: vec![pr(112, "OPEN", false, "failure")],
            ..fx("ACME-2301 webhook retries", "acme-api", Idle)
        },
        Fx {
            prs: vec![pr(113, "OPEN", true, "")],
            ..fx("dark mode", "docs", Idle)
        },
        fx("nightly load test", "infra", Idle),
        fx("reverse proxy", "homelab", Idle),
    ];
    let mut o = Column::empty();
    o.rows = rows.into_iter().map(row).collect();
    // The task you're in has fanned out: one subagent at work, one done.
    if let Some(r) = o.rows.iter_mut().find(|r| r.slug == "column-screenshot") {
        r.subagents = vec![
            subagent("a1", "Explore", "Map the session registry", SubagentStatus::Running, 40),
            subagent("a2", "general-purpose", "Check hook payloads", SubagentStatus::Finished, 20),
        ];
    }
    o.apply_filter();
    o.current = Some("column-screenshot".into());
    o.input_mode = InputMode::Normal;
    o.focus = Focus::List;
    o.selected = 1; // "add release workflow" — the blocked one
    o
}

pub(super) fn hex(c: Color, fallback: &palette::Rgb) -> String {
    match c {
        Color::Rgb(r, g, b) => palette::Rgb(r, g, b).hex(),
        _ => fallback.hex(),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The buffer as an SVG: one `<rect>` per run of non-ground background, one
/// `<text>` per run of identically styled cells, each pinned to its cell
/// width with `textLength` so the columns line up in whatever monospace font
/// the viewer has.
pub(super) const CW: f32 = 8.4;
pub(super) const LH: f32 = 18.0;
pub(super) const PAD: f32 = 10.0;

/// Pixel size of a rendered buffer, padding included.
pub(super) fn svg_size(buf: &Buffer) -> (f32, f32) {
    (
        buf.area.width as f32 * CW + 2.0 * PAD,
        buf.area.height as f32 * LH + 2.0 * PAD,
    )
}

/// The buffer as SVG elements (no `<svg>` wrapper, no ground): one `<rect>`
/// per run of non-ground background, one `<text>` per run of identically
/// styled cells, each pinned to its cell width with `textLength` so the
/// columns line up in whatever monospace font the viewer has.
pub(super) fn svg_body(buf: &Buffer) -> String {
    let (w, h) = (buf.area.width, buf.area.height);
    let ground = palette::GROUND.hex();
    let mut out = String::new();
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let first = buf.cell((x, y)).expect("cell in area");
            let (fg, bg, bold) = (first.fg, first.bg, first.modifier.contains(Modifier::BOLD));
            let start = x;
            let mut text = String::new();
            while x < w {
                let c = buf.cell((x, y)).expect("cell in area");
                if c.fg != fg || c.bg != bg || c.modifier.contains(Modifier::BOLD) != bold {
                    break;
                }
                let sym = c.symbol();
                text.push_str(sym);
                // A wide glyph owns the next cell too; skip its placeholder.
                x += sym.width().max(1) as u16;
            }
            let cells = x - start;
            let (px, py) = (PAD + start as f32 * CW, PAD + y as f32 * LH);
            let bg_hex = hex(bg, &palette::GROUND);
            if bg_hex != ground {
                let _ = writeln!(
                    out,
                    r#"<rect x="{px:.1}" y="{py:.1}" width="{:.1}" height="{LH:.1}" fill="{bg_hex}"/>"#,
                    cells as f32 * CW
                );
            }
            if !text.trim().is_empty() {
                let weight = if bold { r#" font-weight="bold""# } else { "" };
                let _ = writeln!(
                    out,
                    r#"<text x="{px:.1}" y="{:.1}" fill="{}" textLength="{:.1}" lengthAdjust="spacingAndGlyphs"{weight} xml:space="preserve">{}</text>"#,
                    py + LH - 5.0,
                    hex(fg, &palette::TEXT),
                    cells as f32 * CW,
                    escape(&text)
                );
            }
        }
    }
    out
}

/// One buffer as a complete SVG document.
fn svg(buf: &Buffer) -> String {
    let (width, height) = svg_size(buf);
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}" font-family="JetBrains Mono, SF Mono, Menlo, Consolas, DejaVu Sans Mono, monospace" font-size="14">"#
    );
    let _ = writeln!(out, r#"<title>The tenx column</title>"#);
    let _ = writeln!(
        out,
        r#"<rect width="{width:.0}" height="{height:.0}" rx="6" fill="{}"/>"#,
        palette::GROUND.hex()
    );
    out.push_str(&svg_body(buf));
    out.push_str("</svg>\n");
    out
}

pub(super) fn plain_text(buf: &Buffer) -> String {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf.cell((x, y)).expect("cell in area").symbol());
        }
        s.push('\n');
    }
    s
}

/// The client's column: the same rows in a 36-column strip, two lines per
/// task. No preview, the current task's title in the "current" colour
/// instead of a chip, chips on the second line, and a footer that fits.
#[test]
fn column_renders_narrow() {
    let mut column = fixture_column();
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    term.draw(|f| render_in(f, &mut column, f.area())).unwrap();
    let buf = term.backend().buffer();
    let text = plain_text(buf);
    for needle in [
        "SECRETS PENDING",
        "WAITING FOR INPUT",
        "WORKING",
        "INACTIVE",
        "column screenshot",
        "     tenx-workspace",              // second line, indented under the title
        "      permission: Bash  · 4m",    // the reason first, then what else fits
        "     acme-api · 54m · :8080",
        "     ledger · 23h · #31 ✓",
        "wants STRIPE_WEBHOOK_SECRET",
        "     ◐ Map the session registry", // a subagent, under its task
        "     ✔ Check hook payloads",
        " NORMAL ",
        "⏎ open",
    ] {
        assert!(text.contains(needle), "expected {needle:?} in:\n{text}");
    }
    // A click on either line of a task selects that task: both screen lines
    // of the second task (filtered position 1, past a spacer and a header)
    // map back to it, and a header line maps to nothing.
    let heights: Vec<u16> = column.item_heights.clone();
    assert!(heights.contains(&2), "task rows are two lines: {heights:?}");
    let list = column.list_area;
    let item = column.line_to_pos.iter().position(|p| *p == Some(1)).unwrap();
    let y = list.y + 1 + heights[..item].iter().sum::<u16>();
    for line in [y, y + 1] {
        let hit = mouse::item_at_heights(list, 1, 0, &heights, list.x + 2, line).unwrap();
        assert_eq!(column.line_to_pos[hit], Some(1), "line {line}");
    }
    let hit = mouse::item_at_heights(list, 1, 0, &heights, list.x + 2, list.y + 1).unwrap();
    assert_eq!(column.line_to_pos[hit], None, "the header line is not a task");
    // The current task's title is drawn in the "current" colour.
    let y = text.lines().position(|l| l.contains("column screenshot")).unwrap() as u16;
    let x = text.lines().nth(y as usize).unwrap().find("column").unwrap() as u16;
    assert_eq!(buf.cell((x, y)).unwrap().fg, palette::CURRENT.color());
    if std::env::var_os("TENX_SCREENSHOT").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/column.svg");
        std::fs::write(&path, svg(buf)).unwrap();
        eprintln!("wrote {}", path.display());
    }
}

#[test]
fn empty_column_shows_the_mark() {
    let mut column = Column::empty();
    column.apply_filter();
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    term.draw(|f| render_in(f, &mut column, f.area())).unwrap();
    let text = plain_text(term.backend().buffer());
    for needle in [
        "━━━━━━━",
        "━━━━ ●",
        "tenx",
        "no tasks yet — :n to create one",
    ] {
        assert!(text.contains(needle), "expected {needle:?} in:\n{text}");
    }
    if std::env::var_os("TENX_SCREENSHOT").is_some() {
        let path = std::env::temp_dir().join("tenx-column-empty.svg");
        std::fs::write(&path, svg(term.backend().buffer())).unwrap();
        eprintln!("wrote {}", path.display());
    }
}



/// The Work tab, rendered through the real widgets at the column's width.
///
/// Its job is to prove the jobs draw *inside* the column — the whole point of
/// the exercise was that the old spinner printed over it — that the tab bar
/// carries the running count, and that the task list keeps its full height.
#[cfg(test)]
mod work_tab {
    use super::*;
    use tenx_core::progress::{Phase, Plan, Snapshot, StepState};

    /// The column's lines as plain strings, ground and colours dropped.
    fn render(column: &mut Column, rows: u16) -> Vec<String> {
        let backend = TestBackend::new(COLS, rows);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| render_in(f, column, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn running_job() -> crate::tui::job::Job {
        let mut plan = Plan::new(
            "creating 'checkout flow'",
            ["tenx".to_string(), "web-frontend".to_string(), "docs".to_string()],
        );
        plan.steps[0].state = StepState::Done("fetched".into());
        plan.steps[1].state = StepState::Running(Some(Snapshot {
            phase: Phase::Receiving,
            percent: Some(68),
            bytes: Some(13_002_342),
            rate: Some(3_250_585),
        }));
        let (job, tx) = crate::tui::job::fixture(plan);
        std::mem::forget(tx); // keep the channel alive for the render
        job
    }

    fn column_with_jobs(n: usize) -> Column {
        let mut c = fixture_column();
        for _ in 0..n {
            c.jobs.push(running_job());
        }
        c
    }

    #[test]
    fn the_tab_bar_carries_the_running_count() {
        let mut c = column_with_jobs(2);
        let bar = render(&mut c, 24).into_iter().next().unwrap();
        assert!(bar.contains("Tasks"), "{bar}");
        assert!(bar.contains("Repos"), "{bar}");
        assert!(bar.contains("Work"), "{bar}");
        assert!(bar.contains("[2]"), "the count of running jobs belongs in the bar: {bar}");
    }

    #[test]
    fn the_count_disappears_when_nothing_runs() {
        let mut c = fixture_column();
        let bar = render(&mut c, 24).into_iter().next().unwrap();
        assert!(bar.contains("Work"), "{bar}");
        assert!(!bar.contains('['), "no brackets when idle: {bar}");
        // A settled job is listed but is not "running", so it adds no count.
        let mut job = running_job();
        job.outcome = Some(Ok("created 'x'".into()));
        c.jobs.push(job);
        let bar = render(&mut c, 24).into_iter().next().unwrap();
        assert!(!bar.contains('['), "a finished job must not be counted: {bar}");
    }

    #[test]
    fn the_task_list_keeps_its_full_height() {
        // The whole reason for moving to a tab: no panel docked under the list.
        let mut idle = fixture_column();
        let before = render(&mut idle, 24);
        let mut busy = column_with_jobs(1);
        let after = render(&mut busy, 24);
        assert_eq!(
            before.len(),
            after.len(),
            "the column's shape must not change when a job starts"
        );
        // Same last task row visible in both: nothing was pushed off.
        let last_row = |v: &Vec<String>| v.iter().rev().find(|l| l.contains('│')).cloned().unwrap_or_default();
        assert_eq!(last_row(&before), last_row(&after), "a job stole rows from the list");
    }

    #[test]
    fn the_work_tab_shows_each_job_with_its_progress() {
        let mut c = column_with_jobs(1);
        c.tab = Tab::Work;
        let screen = render(&mut c, 24).join("\n");
        eprintln!("\n{screen}\n");
        assert!(screen.contains("creating 'checkout flow'"), "{screen}");
        assert!(screen.contains("1/3"), "step counter missing:\n{screen}");
        assert!(screen.contains("✓ tenx"), "{screen}");
        assert!(screen.contains("web-frontend"), "{screen}");
        assert!(screen.contains("receiving"), "{screen}");
        assert!(screen.contains("MiB/s"), "transfer line missing:\n{screen}");
        assert!(screen.contains('█') && screen.contains('░'), "no bar:\n{screen}");
    }

    #[test]
    fn a_failed_job_keeps_its_error_where_it_can_be_read() {
        // The footer message is gone the moment anything else happens; the
        // Work tab is the only record left.
        let mut c = fixture_column();
        let mut job = running_job();
        job.outcome = Some(Err("repository 'x' does not exist".into()));
        c.jobs.push(job);
        c.tab = Tab::Work;
        let screen = render(&mut c, 24).join("\n");
        assert!(screen.contains('✗'), "a failure needs its own glyph:\n{screen}");
        assert!(screen.contains("does not exist"), "the error must survive:\n{screen}");
    }

    #[test]
    fn an_empty_work_tab_says_so() {
        let mut c = fixture_column();
        c.tab = Tab::Work;
        let screen = render(&mut c, 24).join("\n");
        assert!(screen.contains("nothing running"), "{screen}");
    }

    #[test]
    fn an_indeterminate_job_still_shows_movement() {
        // A `git worktree remove` reports no phases at all. It must not sit on
        // a bar frozen at zero, which reads as a hang.
        let mut c = fixture_column();
        let mut plan = Plan::new("deleting 'csv export'", ["csv export".to_string()]);
        plan.steps[0].state = StepState::Running(None);
        let (job, tx) = crate::tui::job::fixture(plan);
        std::mem::forget(tx);
        c.jobs.push(job);
        c.tab = Tab::Work;

        let a = render(&mut c, 24).join("\n");
        c.frame += 3;
        let b = render(&mut c, 24).join("\n");
        assert!(a.contains('█'), "no marquee:\n{a}");
        assert_ne!(a, b, "the indeterminate bar does not move");
        assert!(!a.contains('%'), "nothing to be a percent of yet:\n{a}");
    }

    #[test]
    fn tabs_cycle_both_ways_through_all_three() {
        let mut c = fixture_column();
        assert_eq!(c.tab, Tab::Tasks);
        c.cycle_tab(false);
        assert_eq!(c.tab, Tab::Repos);
        c.cycle_tab(false);
        assert_eq!(c.tab, Tab::Work);
        c.cycle_tab(false);
        assert_eq!(c.tab, Tab::Tasks, "gt wraps");
        c.cycle_tab(true);
        assert_eq!(c.tab, Tab::Work, "gT goes back");
    }

    #[test]
    fn dd_dismisses_a_settled_job_but_not_a_running_one() {
        let mut c = column_with_jobs(1);
        c.tab = Tab::Work;
        c.work_selected = 0;
        c.dismiss_job();
        assert_eq!(c.jobs.len(), 1, "a running job must not be dismissed");
        assert!(c.status_msg.as_deref().unwrap_or("").contains("still running"));

        c.jobs[0].outcome = Some(Ok("created".into()));
        c.dismiss_job();
        assert!(c.jobs.is_empty(), "a settled job clears");
    }

    #[test]
    fn a_click_selects_the_tab_actually_under_it() {
        // The bar is laid out by hand so the spans are exact; a click on
        // "Work" must not land on "Repos" the way a half-split would.
        let mut c = column_with_jobs(1);
        let _ = render(&mut c, 24);
        let spans = c.tab_spans.clone();
        assert_eq!(spans.len(), 3, "three tabs recorded");
        for (i, tab) in Tab::ALL.iter().enumerate() {
            let (a, b) = spans[i];
            assert!(b > a, "tab {i} has no width");
            let mid = c.tabs_area.x + (a + b) / 2;
            let ev = MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: mid,
                row: c.tabs_area.y,
                modifiers: KeyModifiers::NONE,
            };
            c.handle_mouse(ev).unwrap();
            assert_eq!(c.tab, *tab, "click at {mid} should select {tab:?}");
        }
    }
}

/// Quitting while a job runs.
#[cfg(test)]
mod quit_guard {
    use super::*;
    use tenx_core::progress::{Plan, StepState};

    fn column_mid_job() -> Column {
        let mut c = fixture_column();
        let mut plan = Plan::new("creating 'checkout flow'", ["tenx".to_string()]);
        plan.steps[0].state = StepState::Running(None);
        let (job, tx) = crate::tui::job::fixture(plan);
        std::mem::forget(tx);
        c.jobs.push(job);
        c
    }

    fn run(c: &mut Column, cmd: &str) {
        c.mode = Mode::Command(cmd.to_string());
        c.handle_key(KeyEvent::from(KeyCode::Enter)).unwrap();
    }

    #[test]
    fn plain_quit_is_refused_while_a_job_runs() {
        let mut c = column_mid_job();
        run(&mut c, "q");
        assert_eq!(c.take_request(), None, "quit must not go through");
        let msg = c.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("creating 'checkout flow'"), "must name the job: {msg}");
        assert!(msg.contains(":q!"), "must say how to quit anyway: {msg}");
    }

    #[test]
    fn bang_quit_goes_anyway() {
        let mut c = column_mid_job();
        run(&mut c, "q!");
        assert_eq!(c.take_request(), Some(ClientRequest::Quit));
    }

    #[test]
    fn quit_is_unaffected_when_nothing_is_running() {
        let mut c = fixture_column();
        run(&mut c, "q");
        assert_eq!(c.take_request(), Some(ClientRequest::Quit));
    }

    #[test]
    fn a_landed_job_does_not_block_quit() {
        // The job finished but its outcome hasn't been folded in yet; there
        // is nothing left to interrupt, so quit must not be held up.
        let mut c = column_mid_job();
        c.jobs[0].outcome = Some(Ok("created".into()));
        run(&mut c, "q");
        assert_eq!(c.take_request(), Some(ClientRequest::Quit));
    }
}
