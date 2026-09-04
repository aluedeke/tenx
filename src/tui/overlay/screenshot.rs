//! The README screenshot, generated from the real widgets: the overlay is
//! rendered with fixture rows into ratatui's `TestBackend` and the buffer is
//! written out cell by cell as an SVG. Nothing here touches disk, tmux or
//! Claude Code's session registry, so the picture never leaks a real task,
//! and it can't drift from what the overlay actually draws. The plain test
//! only checks the render; `make screenshot` sets `TENX_SCREENSHOT` to write
//! `docs/overlay.svg`.

use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use std::fmt::Write as _;
use tenx_core::live::{Live, PrInfo};

pub(super) const COLS: u16 = 150;
pub(super) const ROWS: u16 = 36;

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
        secrets_pending: f.secrets.iter().map(|s| s.to_string()).collect(),
        secrets_pending_set: vec![],
        section,
    }
}

/// Every section and every kind of chip, on invented tasks. Rows are listed
/// in display order (section, then status rank, then recency), as
/// `rebuild_rows` would sort them.
pub(super) fn fixture_overlay() -> Overlay {
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
            waiting_for: Some("permission prompt"),
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
            ..fx("overlay screenshot", "tenx-workspace", Working)
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
    let mut o = Overlay::empty(false);
    o.rows = rows.into_iter().map(row).collect();
    o.apply_filter();
    o.current = Some("overlay-screenshot".into());
    o.input_mode = InputMode::Normal;
    o.focus = Focus::List;
    o.selected = 1; // "add release workflow" — the blocked one, so the preview shows its dialog
    o.preview = Preview {
        pane: Some("%1".into()),
        lines: PREVIEW_FIXTURE.lines().map(super::super::ansi::line).collect(),
        gone: false,
    };
    o
}

/// What a Claude Code pane looks like on a permission prompt, as
/// `capture-pane -e` would hand it over (a few SGR sequences included).
pub(super) const PREVIEW_FIXTURE: &str = "\
\x1b[1m⏺\x1b[0m I'll add the release workflow next to the CI one and wire the
  tag push to it.

\x1b[1m Bash command\x1b[0m

   gh workflow run release.yml --ref v0.2.0
   Kick off the release workflow for the tag

 Do you want to proceed?
 \x1b[36m❯ 1. Yes\x1b[0m
   2. No
 \x1b[2mEsc to cancel · Tab to amend\x1b[0m
";

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
    let _ = writeln!(out, r#"<title>The tenx overlay</title>"#);
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

#[test]
fn renders_every_section_and_chip_from_fixtures() {
    let mut overlay = fixture_overlay();
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    term.draw(|f| render(f, &mut overlay)).unwrap();
    let buf = term.backend().buffer();
    let text = plain_text(buf);
    for needle in [
        "SECRETS PENDING",
        "WAITING FOR INPUT",
        "WORKING",
        "INACTIVE",
        "wants STRIPE_WEBHOOK_SECRET",
        "permission prompt",
        "flaky checkout e2e",
        "Do you want to proceed?",
        "1. Yes",
        "y approve",
        " current ",
        "#781 merged",
        "#14 …",
        "#112 ✗",
        "#113 draft",
        ":8080",
        " NORMAL ",
    ] {
        assert!(text.contains(needle), "expected {needle:?} in:\n{text}");
    }
    if std::env::var_os("TENX_SCREENSHOT").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/overlay.svg");
        std::fs::write(&path, svg(buf)).unwrap();
        eprintln!("wrote {}", path.display());
    }
}

#[test]
fn empty_overlay_shows_the_mark() {
    let mut overlay = Overlay::empty(false);
    overlay.apply_filter();
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    term.draw(|f| render(f, &mut overlay)).unwrap();
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
        let path = std::env::temp_dir().join("tenx-overlay-empty.svg");
        std::fs::write(&path, svg(term.backend().buffer())).unwrap();
        eprintln!("wrote {}", path.display());
    }
}
