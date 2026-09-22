//! How a long operation tells someone it is still going.
//!
//! One vocabulary, two surfaces. The work — cloning repos, adding worktrees —
//! lives in `cli::task` and `cli::init` and is called from both the CLI and
//! the column, so it reports through a [`Reporter`] rather than printing:
//!
//! - [`Terminal`] is the CLI's: a live line on stdout, one per step, left
//!   behind as a `✓` when the step finishes. A plain terminal it owns.
//! - `tui::job::Sender` is the column's: every event goes down a channel to
//!   the client's event loop, which draws it with ratatui inside the column.
//!
//! The reason for the split is a bug that was very visible: the old spinner
//! printed `\r  ⠋ cloning…` to stdout unconditionally, and the column runs
//! ratatui on the alternate screen in raw mode. The spinner landed wherever
//! the cursor was, ratatui's diff didn't know those cells had changed so the
//! smear persisted until a resize, and `println!` without a `\r` staircased
//! the final line down the screen. Nothing may print to stdout from inside
//! the client — hence a reporter the caller chooses.

use std::io::Write;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use tenx_core::progress::{bar, transfer_line, Snapshot};

/// Frames for an operation that can't say how far along it is.
pub const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// How often an indeterminate spinner advances.
pub const TICK: Duration = Duration::from_millis(80);

/// Something that happened in a job, addressed to the step it happened to.
///
/// Steps are numbered by the plan the caller built before starting, so a
/// reporter can show the whole shape of the work from the first frame instead
/// of growing a line at a time.
#[derive(Debug, Clone)]
pub enum Event {
    /// The step began. `label` names what it is for (a repo), `verb` is the
    /// present-tense word for what is being done to it ("cloning").
    Start { step: usize, label: String, verb: &'static str },
    /// Git said something about the step running now.
    Update { step: usize, snap: Snapshot },
    /// The step finished. `note` is the outcome in a word ("cloned").
    Done { step: usize, note: String },
    Failed { step: usize, err: String },
}

/// Where a job's events go. `Sync` because the work may report from a worker
/// thread while the UI reads on another.
pub trait Reporter: Send + Sync {
    fn emit(&self, ev: Event);
}

/// Report nothing — for callers with no surface to draw on (tests, the
/// watcher, a `--quiet` run).
pub struct Silent;

impl Reporter for Silent {
    fn emit(&self, _: Event) {}
}

/// A reporter that owns a plain terminal: the CLI's.
///
/// One live line for the running step, rewritten in place, then committed as
/// a permanent `✓` line when the step ends and replaced by the next step's.
/// Single-line on purpose: redrawing several lines needs cursor-up sequences
/// that misbehave the moment anything else writes, and one line per step is
/// what `tenx init` and `tenx task new` already looked like.
pub struct Terminal {
    state: Arc<Mutex<Line>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// The live line's contents, shared with the animation thread.
struct Line {
    /// Label and phase of the step running now; `None` between steps.
    active: Option<(String, &'static str)>,
    snap: Option<Snapshot>,
    frame: usize,
}

impl Line {
    /// The line as it should currently look, without the leading erase.
    ///
    /// Determinate once git has said a percent, indeterminate before that —
    /// the same line either way, so it doesn't jump when the bar appears.
    fn render(&self) -> String {
        let Some((label, verb)) = &self.active else { return String::new() };
        let spin = FRAMES[self.frame % FRAMES.len()];
        let phase = self.snap.map(|s| s.phase.label()).unwrap_or(verb);
        let mut line = format!("  {spin} {phase} {label}");
        if let Some(snap) = self.snap {
            if snap.percent.is_some() {
                // The phase's position in the *whole* operation, not its own
                // percent: the bar has to cross once, not run 0→100 again for
                // counting, compressing, receiving and resolving in turn.
                let frac = snap.fraction();
                line.push_str(&format!("  {} {:>3}%", bar(16, frac), (frac * 100.0).round() as u16));
            }
            let transfer = transfer_line(&snap);
            if !transfer.is_empty() {
                line.push_str(&format!(" · {transfer}"));
            }
        } else {
            line.push('…');
        }
        line
    }
}

impl Terminal {
    /// Start the animation. The thread only ever redraws the live line; every
    /// permanent line is written by `emit`, under the same lock, so the two
    /// can't interleave mid-line.
    pub fn new() -> Terminal {
        let state = Arc::new(Mutex::new(Line { active: None, snap: None, frame: 0 }));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, stop2) = (Arc::clone(&state), Arc::clone(&stop));

        let handle = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                {
                    let mut line = s2.lock().unwrap_or_else(|e| e.into_inner());
                    if line.active.is_some() {
                        line.frame += 1;
                        redraw(&line.render());
                    }
                }
                std::thread::sleep(TICK);
            }
        });

        Terminal { state, stop, handle: Some(handle) }
    }

    /// Stop the animation thread and clear whatever line is still live.
    ///
    /// Called by `Drop`, so an error path that returns early still leaves the
    /// terminal on a clean line rather than mid-spinner.
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
        let mut line = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if line.active.take().is_some() {
            print!("\r\x1b[2K");
            std::io::stdout().flush().ok();
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Terminal::new()
    }
}

impl Reporter for Terminal {
    fn emit(&self, ev: Event) {
        let mut line = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match ev {
            Event::Start { label, verb, .. } => {
                line.active = Some((label, verb));
                line.snap = None;
                redraw(&line.render());
            }
            Event::Update { snap, .. } => {
                line.snap = Some(snap);
                redraw(&line.render());
            }
            Event::Done { note, .. } => {
                let label = line.active.take().map(|(l, _)| l).unwrap_or_default();
                line.snap = None;
                commit(&format!("✓ {label}  {note}"));
            }
            Event::Failed { err, .. } => {
                let label = line.active.take().map(|(l, _)| l).unwrap_or_default();
                line.snap = None;
                commit(&format!("✗ {label}  {err}"));
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Rewrite the live line in place: carriage return, erase to end of line.
fn redraw(line: &str) {
    print!("\r\x1b[2K{line}");
    std::io::stdout().flush().ok();
}

/// Replace the live line with a permanent one and move on.
fn commit(line: &str) {
    println!("\r\x1b[2K  {line}");
    std::io::stdout().flush().ok();
}

/// The reporter a command should use: the terminal when this process owns
/// one, silence otherwise.
///
/// The column never takes this path — it builds its own channel reporter —
/// but a `tenx` subcommand run from an agent's shell tool has no terminal to
/// animate, and a spinner there is just noise in a transcript.
pub fn for_cli() -> Box<dyn Reporter> {
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        Box::new(Terminal::new())
    } else {
        Box::new(Silent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tenx_core::progress::Phase;

    fn snap(phase: Phase, percent: Option<u8>) -> Snapshot {
        Snapshot { phase, percent, bytes: None, rate: None }
    }

    #[test]
    fn an_indeterminate_line_names_the_verb() {
        let line = Line { active: Some(("web".into(), "cloning")), snap: None, frame: 0 };
        assert_eq!(line.render(), "  ⠋ cloning web…");
    }

    #[test]
    fn a_determinate_line_carries_a_bar_and_the_phase() {
        let line = Line {
            active: Some(("web".into(), "cloning")),
            snap: Some(snap(Phase::Receiving, Some(50))),
            frame: 1,
        };
        let out = line.render();
        // The phase git reports wins over the caller's opening verb.
        assert!(out.contains("receiving web"), "{out}");
        // Receiving at 50% is ~48% of the whole clone, not 50% of the bar:
        // the phases before it are already behind us.
        assert!(out.contains("48%"), "{out}");
        assert!(out.contains('█') && out.contains('░'), "{out}");
    }

    #[test]
    fn a_phase_with_no_percent_gets_no_bar() {
        let line = Line {
            active: Some(("web".into(), "cloning")),
            snap: Some(snap(Phase::Enumerating, None)),
            frame: 0,
        };
        let out = line.render();
        assert!(out.contains("enumerating web"), "{out}");
        assert!(!out.contains('░'), "no bar without a percent: {out}");
    }

    #[test]
    fn transfer_figures_follow_the_bar() {
        let line = Line {
            active: Some(("web".into(), "cloning")),
            snap: Some(Snapshot {
                phase: Phase::Receiving,
                percent: Some(42),
                bytes: Some(1024 * 1024),
                rate: Some(2 * 1024 * 1024),
            }),
            frame: 0,
        };
        let out = line.render();
        assert!(out.contains("1.00 MiB · 2.00 MiB/s"), "{out}");
    }

    #[test]
    fn the_bar_only_ever_moves_forward_across_phases() {
        let seq = [
            (Phase::Counting, 100u8),
            (Phase::Compressing, 0),
            (Phase::Compressing, 100),
            (Phase::Receiving, 0),
            (Phase::Receiving, 100),
            (Phase::Resolving, 0),
            (Phase::CheckingOut, 100),
        ];
        let pct = |p, n| {
            let line = Line { active: Some(("web".into(), "cloning")), snap: Some(snap(p, Some(n))), frame: 0 };
            let out = line.render();
            let at = out.find('%').unwrap();
            out[..at].trim().rsplit(' ').next().unwrap().parse::<u16>().unwrap()
        };
        let mut last = 0;
        for (phase, n) in seq {
            let now = pct(phase, n);
            assert!(now >= last, "{phase:?} at {n}% went backwards: {last}% → {now}%");
            last = now;
        }
        assert_eq!(last, 100);
    }

    #[test]
    fn nothing_active_renders_nothing() {
        let line = Line { active: None, snap: None, frame: 3 };
        assert_eq!(line.render(), "");
    }

    #[test]
    fn silent_swallows_everything() {
        // Mostly a compile-time check that `Silent` is a usable `Reporter`.
        let r: Box<dyn Reporter> = Box::new(Silent);
        r.emit(Event::Start { step: 0, label: "web".into(), verb: "cloning" });
        r.emit(Event::Done { step: 0, note: "cloned".into() });
    }
}
