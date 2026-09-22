//! Progress of a long operation, as values: what git says about itself, and
//! what a job made of several such operations has left to do.
//!
//! The binary spawns `git` and feeds every line it writes to
//! [`parse_git_line`]; the column turns the resulting [`Plan`] into rows and a
//! bar. Nothing here runs a process or draws anything, so the whole model —
//! which phase weighs what, when a job counts as finished, what the bar looks
//! like at 68% — is testable without a repo or a terminal.

use std::fmt::Write as _;

/// A phase of a clone or fetch, in the order git reports them.
///
/// Git sends these as `\r`-separated progress lines on stderr. They are not
/// all present in every operation (a fetch with nothing to do reports none,
/// a cached clone skips `Receiving`), so the phase is only ever a label for
/// what is happening *now* — never a step counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `remote: Enumerating objects` — the server is listing what it has.
    Enumerating,
    /// `remote: Counting objects`.
    Counting,
    /// `remote: Compressing objects`.
    Compressing,
    /// `Receiving objects` — the transfer itself, and the only phase that
    /// reports bytes and a rate.
    Receiving,
    /// `Resolving deltas` — local, CPU-bound, after the transfer.
    Resolving,
    /// `Updating files` — writing the worktree out.
    CheckingOut,
}

impl Phase {
    /// The word the column shows for this phase. Present tense, lower case,
    /// short enough for a narrow column.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Enumerating => "enumerating",
            Phase::Counting => "counting",
            Phase::Compressing => "compressing",
            Phase::Receiving => "receiving",
            Phase::Resolving => "resolving",
            Phase::CheckingOut => "checking out",
        }
    }

    /// How much of a clone this phase accounts for, as a fraction of the
    /// whole. Receiving dominates over a network; the rest are rounding on
    /// anything but a tiny repo, but giving them weight keeps the bar moving
    /// during the seconds before the first byte arrives.
    ///
    /// The weights sum to 1.0, and [`Snapshot::fraction`] turns a phase plus
    /// its percent into a position along them.
    fn weight(self) -> f32 {
        match self {
            Phase::Enumerating => 0.04,
            Phase::Counting => 0.04,
            Phase::Compressing => 0.08,
            Phase::Receiving => 0.64,
            Phase::Resolving => 0.14,
            Phase::CheckingOut => 0.06,
        }
    }

    /// The weights of every phase before this one — where its slice starts.
    fn offset(self) -> f32 {
        const ORDER: [Phase; 6] = [
            Phase::Enumerating,
            Phase::Counting,
            Phase::Compressing,
            Phase::Receiving,
            Phase::Resolving,
            Phase::CheckingOut,
        ];
        ORDER.iter().take_while(|p| **p != self).map(|p| p.weight()).sum()
    }
}

/// What git last said about the operation running now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub phase: Phase,
    /// 0–100, when git reported one. `Enumerating` has no total, so it
    /// reports a count and no percent.
    pub percent: Option<u8>,
    /// Bytes transferred so far (`Receiving` only).
    pub bytes: Option<u64>,
    /// Bytes per second (`Receiving` only).
    pub rate: Option<u64>,
}

impl Snapshot {
    /// Where this snapshot sits in the whole operation, 0.0–1.0, by placing
    /// the phase's own percent inside that phase's weighted slice.
    ///
    /// A phase with no percent counts as halfway through its slice: it is
    /// running, so it is not at its start, and claiming it is done would let
    /// the bar go backwards when the next phase opens at 0%.
    pub fn fraction(&self) -> f32 {
        let within = match self.percent {
            Some(p) => f32::from(p) / 100.0,
            None => 0.5,
        };
        (self.phase.offset() + self.phase.weight() * within).clamp(0.0, 1.0)
    }
}

/// Parse one line of git's `--progress` output, or `None` if it isn't one.
///
/// Git writes progress to stderr as `\r`-terminated redraws of the same line,
/// so a caller splitting on `\n` alone sees many phases stuck together — split
/// on both (see [`split_progress`]) and hand each piece here. Everything that
/// isn't a recognised phase (`remote:` banners, warnings, the final `done.`
/// summaries) returns `None` and should be ignored rather than shown: git's
/// error text arrives on the same stream and is reported through the command's
/// exit status instead.
pub fn parse_git_line(line: &str) -> Option<Snapshot> {
    // `remote: ` prefixes the server-side phases; the local ones have none.
    let line = line.trim().strip_prefix("remote:").unwrap_or(line).trim();
    let (head, rest) = line.split_once(':')?;
    let phase = match head.trim() {
        "Enumerating objects" => Phase::Enumerating,
        "Counting objects" => Phase::Counting,
        "Compressing objects" => Phase::Compressing,
        "Receiving objects" => Phase::Receiving,
        "Resolving deltas" => Phase::Resolving,
        // Older git called it "Checking out files".
        "Updating files" | "Checking out files" => Phase::CheckingOut,
        _ => return None,
    };
    let rest = rest.trim();
    let percent = rest
        .split_once('%')
        .and_then(|(n, _)| n.trim().parse::<u8>().ok())
        .map(|p| p.min(100));
    // "Receiving objects:  42% (5/12), 4.21 MiB | 3.10 MiB/s" — the byte
    // count and the rate are the last two comma/pipe-separated fields.
    let (bytes, rate) = match phase {
        Phase::Receiving => {
            let tail = rest.split(',').next_back().unwrap_or("");
            let (size, speed) = match tail.split_once('|') {
                Some((s, r)) => (s, Some(r)),
                None => (tail, None),
            };
            (parse_size(size), speed.and_then(|r| parse_size(r.trim_end_matches("/s"))))
        }
        _ => (None, None),
    };
    Some(Snapshot { phase, percent, bytes, rate })
}

/// `"4.21 MiB"` → bytes. Git's own units, so the bases are binary.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let digits = s.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let (num, unit) = s.split_at(digits);
    let n: f64 = num.trim().parse().ok()?;
    let mult: f64 = match unit.trim() {
        "B" | "bytes" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult) as u64)
}

/// Split a chunk of git's stderr into the lines it meant to draw.
///
/// Progress redraws are `\r`-separated within one `\n` line, so both are
/// separators here. Empty pieces are dropped.
pub fn split_progress(chunk: &str) -> impl DoubleEndedIterator<Item = &str> {
    chunk
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

// ── The job model ────────────────────────────────────────────────────────────

/// What one step of a job is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum StepState {
    /// Not started. Drawn dim, so the whole shape of the work is visible from
    /// the first frame rather than growing a line at a time.
    Pending,
    /// Started. The snapshot is `None` until git says something — a fetch
    /// with nothing to do never says anything at all.
    Running(Option<Snapshot>),
    /// Finished. The note is the outcome in one word ("fetched", "cloned",
    /// "up to date"), not a sentence.
    Done(String),
    Failed(String),
}

/// One unit of work in a job: a labelled thing that can be pending, running,
/// done or failed.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// What the step is for — a repo name, usually. Shown verbatim.
    pub label: String,
    pub state: StepState,
}

impl Step {
    pub fn pending(label: impl Into<String>) -> Step {
        Step { label: label.into(), state: StepState::Pending }
    }

    /// The word shown beside the label: the live phase while running, the
    /// outcome once finished.
    pub fn note(&self) -> &str {
        match &self.state {
            StepState::Pending => "",
            StepState::Running(Some(s)) => s.phase.label(),
            StepState::Running(None) => "working",
            StepState::Done(note) => note,
            StepState::Failed(_) => "failed",
        }
    }
}

/// A whole long operation as the column sees it: a title and the steps it is
/// made of, known up front.
///
/// Built before the work starts (creating a task knows its repos), so the
/// panel can show every step from the first frame and the overall bar has a
/// real denominator instead of counting up from nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// What the job is, for the panel's first line: "creating 'checkout flow'".
    pub title: String,
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn new(title: impl Into<String>, labels: impl IntoIterator<Item = String>) -> Plan {
        Plan { title: title.into(), steps: labels.into_iter().map(Step::pending).collect() }
    }

    /// The index of the step running now, if any.
    pub fn active(&self) -> Option<usize> {
        self.steps.iter().position(|s| matches!(s.state, StepState::Running(_)))
    }

    /// The first failure, if the job hit one.
    pub fn failure(&self) -> Option<&str> {
        self.steps.iter().find_map(|s| match &s.state {
            StepState::Failed(e) => Some(e.as_str()),
            _ => None,
        })
    }

    /// Every step settled — nothing pending, nothing running.
    pub fn finished(&self) -> bool {
        self.steps
            .iter()
            .all(|s| matches!(s.state, StepState::Done(_) | StepState::Failed(_)))
    }

    /// How far along the whole job is, 0.0–1.0: finished steps count in full,
    /// the running one contributes its own fraction, and a failed step counts
    /// as finished (the bar stops where it stopped rather than hanging).
    ///
    /// An empty plan is 1.0, so a job with no work to do doesn't render a bar
    /// stuck at zero.
    pub fn fraction(&self) -> f32 {
        if self.steps.is_empty() {
            return 1.0;
        }
        let each = 1.0 / self.steps.len() as f32;
        self.steps
            .iter()
            .map(|s| match &s.state {
                StepState::Pending => 0.0,
                StepState::Running(Some(snap)) => each * snap.fraction(),
                // Running with nothing said yet: don't claim any of the step,
                // or a fast job flickers forward and back.
                StepState::Running(None) => 0.0,
                StepState::Done(_) | StepState::Failed(_) => each,
            })
            .sum::<f32>()
            .clamp(0.0, 1.0)
    }

    /// "2/5" — settled steps over total, for the panel's header.
    pub fn counter(&self) -> String {
        let done = self
            .steps
            .iter()
            .filter(|s| matches!(s.state, StepState::Done(_) | StepState::Failed(_)))
            .count();
        format!("{done}/{}", self.steps.len())
    }
}

// ── Rendering helpers ────────────────────────────────────────────────────────

/// A text progress bar `width` cells wide at `frac` (0.0–1.0), as full and
/// empty blocks.
///
/// Deliberately whole cells only: a partial block (`▏▎▍`) reads as a rendering
/// artifact at the column's width, and half the terminals that run tenx pick a
/// fallback glyph for them anyway.
pub fn bar(width: usize, frac: f32) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let filled = ((width as f32) * frac).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// An indeterminate bar `width` cells wide: a short block sliding back and
/// forth, one cell per `frame`.
///
/// For the stretch before git says a percent — resolving a host, waiting on
/// the server's first byte, a `git worktree remove` that reports nothing at
/// all. A still bar there reads as a hang; this reads as waiting.
///
/// It bounces rather than wrapping, so the eye isn't caught by a block
/// teleporting from the right edge back to the left.
pub fn marquee(width: usize, frame: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let run = (width / 4).clamp(1, width);
    let span = width - run;
    // A full cycle is out and back; `span == 0` (a bar no wider than its run)
    // has nowhere to travel, so it just sits filled.
    let pos = if span == 0 {
        0
    } else {
        let cycle = frame % (span * 2);
        if cycle <= span { cycle } else { span * 2 - cycle }
    };
    let mut s = String::with_capacity(width * 3);
    for i in 0..width {
        s.push(if i >= pos && i < pos + run { '█' } else { '░' });
    }
    s
}

/// Bytes as git writes them — binary units, two significant decimals under
/// 10, one above. `None` renders empty, so a call site can format a snapshot
/// that has no byte count without branching.
pub fn format_bytes(b: Option<u64>) -> String {
    let Some(b) = b else { return String::new() };
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let mut s = String::new();
    if u == 0 {
        let _ = write!(s, "{} B", b);
    } else if v < 10.0 {
        let _ = write!(s, "{v:.2} {}", UNITS[u]);
    } else {
        let _ = write!(s, "{v:.1} {}", UNITS[u]);
    }
    s
}

/// A transfer rate, `format_bytes` plus `/s`.
pub fn format_rate(b: Option<u64>) -> String {
    match b {
        Some(_) => format!("{}/s", format_bytes(b)),
        None => String::new(),
    }
}

/// The line under a running step's bar: what has arrived and how fast, or
/// empty when git hasn't said (every phase but `Receiving`).
pub fn transfer_line(snap: &Snapshot) -> String {
    match (snap.bytes, snap.rate) {
        (Some(_), Some(_)) => format!("{} · {}", format_bytes(snap.bytes), format_rate(snap.rate)),
        (Some(_), None) => format_bytes(snap.bytes),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_phases_git_actually_writes() {
        let s = parse_git_line("remote: Enumerating objects: 1234, done.").unwrap();
        assert_eq!(s.phase, Phase::Enumerating);
        assert_eq!(s.percent, None, "enumerating has no total, so no percent");

        let s = parse_git_line("remote: Counting objects: 100% (1234/1234), done.").unwrap();
        assert_eq!((s.phase, s.percent), (Phase::Counting, Some(100)));

        let s = parse_git_line("remote: Compressing objects:  57% (123/216)").unwrap();
        assert_eq!((s.phase, s.percent), (Phase::Compressing, Some(57)));

        let s = parse_git_line("Resolving deltas:  88% (7654/8700)").unwrap();
        assert_eq!((s.phase, s.percent), (Phase::Resolving, Some(88)));

        let s = parse_git_line("Updating files:  63% (1234/1954)").unwrap();
        assert_eq!((s.phase, s.percent), (Phase::CheckingOut, Some(63)));

        // Git before 2.23 called the checkout phase something else.
        let s = parse_git_line("Checking out files:  63% (1234/1954)").unwrap();
        assert_eq!(s.phase, Phase::CheckingOut);
    }

    #[test]
    fn receiving_carries_bytes_and_rate() {
        let s = parse_git_line("Receiving objects:  42% (5123/12190), 4.21 MiB | 3.10 MiB/s").unwrap();
        assert_eq!(s.phase, Phase::Receiving);
        assert_eq!(s.percent, Some(42));
        assert_eq!(s.bytes, Some((4.21 * 1024.0 * 1024.0) as u64));
        assert_eq!(s.rate, Some((3.10 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn receiving_without_a_rate_yet() {
        // The first redraw has no `|` — the rate needs two samples.
        let s = parse_git_line("Receiving objects:   1% (122/12190), 132.00 KiB").unwrap();
        assert_eq!(s.bytes, Some(132 * 1024));
        assert_eq!(s.rate, None);
    }

    #[test]
    fn non_progress_output_is_ignored() {
        // Banners, warnings and git's error text all share this stream; only
        // the exit status decides failure, so none of these may parse.
        assert!(parse_git_line("Cloning into bare repository 'foo.git'...").is_none());
        assert!(parse_git_line("warning: redirecting to https://example.com/foo.git/").is_none());
        assert!(parse_git_line("fatal: repository 'x' not found").is_none());
        assert!(parse_git_line("").is_none());
        assert!(parse_git_line("remote:").is_none());
    }

    #[test]
    fn a_redraw_chunk_splits_on_carriage_returns() {
        let chunk = "Receiving objects:  10% (1/10)\rReceiving objects:  50% (5/10)\rReceiving objects: 100% (10/10)\n";
        let got: Vec<_> = split_progress(chunk).filter_map(parse_git_line).map(|s| s.percent).collect();
        assert_eq!(got, vec![Some(10), Some(50), Some(100)]);
    }

    #[test]
    fn a_phase_places_itself_in_the_whole() {
        // Receiving at 0% must already be past everything before it, and
        // Resolving at 0% past all of Receiving — the bar never goes back.
        let recv = Snapshot { phase: Phase::Receiving, percent: Some(0), bytes: None, rate: None };
        let recv_done = Snapshot { phase: Phase::Receiving, percent: Some(100), bytes: None, rate: None };
        let resolve = Snapshot { phase: Phase::Resolving, percent: Some(0), bytes: None, rate: None };
        assert!(recv.fraction() > 0.1);
        assert!(recv_done.fraction() > recv.fraction());
        assert!(resolve.fraction() >= recv_done.fraction());
        let last = Snapshot { phase: Phase::CheckingOut, percent: Some(100), bytes: None, rate: None };
        assert!((last.fraction() - 1.0).abs() < 0.001, "the last phase at 100% is the whole thing");
    }

    #[test]
    fn a_phase_with_no_percent_sits_mid_slice() {
        let s = Snapshot { phase: Phase::Enumerating, percent: None, bytes: None, rate: None };
        let f = s.fraction();
        assert!(f > 0.0 && f < Phase::Enumerating.weight());
    }

    #[test]
    fn plan_tracks_its_steps() {
        let mut plan = Plan::new("creating 'x'", ["tenx".to_string(), "web".to_string()]);
        assert_eq!(plan.counter(), "0/2");
        assert_eq!(plan.fraction(), 0.0);
        assert!(!plan.finished());
        assert_eq!(plan.active(), None);

        plan.steps[0].state = StepState::Done("cloned".into());
        assert_eq!(plan.counter(), "1/2");
        assert!((plan.fraction() - 0.5).abs() < 0.001);

        plan.steps[1].state = StepState::Running(Some(Snapshot {
            phase: Phase::Receiving,
            percent: Some(50),
            bytes: None,
            rate: None,
        }));
        assert_eq!(plan.active(), Some(1));
        assert!(plan.fraction() > 0.5, "the running step adds its own share");
        assert!(plan.fraction() < 1.0);
        assert!(!plan.finished());

        plan.steps[1].state = StepState::Done("cloned".into());
        assert!(plan.finished());
        assert_eq!(plan.fraction(), 1.0);
    }

    #[test]
    fn a_failure_settles_the_job_rather_than_hanging_it() {
        let mut plan = Plan::new("creating 'x'", ["a".to_string(), "b".to_string()]);
        plan.steps[0].state = StepState::Failed("no such repo".into());
        plan.steps[1].state = StepState::Done("cloned".into());
        assert!(plan.finished());
        assert_eq!(plan.failure(), Some("no such repo"));
        assert_eq!(plan.fraction(), 1.0);
    }

    #[test]
    fn an_empty_plan_is_complete() {
        let plan = Plan::new("nothing", []);
        assert!(plan.finished());
        assert_eq!(plan.fraction(), 1.0);
        assert_eq!(plan.counter(), "0/0");
    }

    #[test]
    fn bar_fills_whole_cells_and_never_overruns() {
        assert_eq!(bar(4, 0.0), "░░░░");
        assert_eq!(bar(4, 1.0), "████");
        assert_eq!(bar(4, 0.5), "██░░");
        // Out-of-range input is clamped, not panicked on.
        assert_eq!(bar(4, 2.0).chars().count(), 4);
        assert_eq!(bar(4, -1.0), "░░░░");
        assert_eq!(bar(0, 0.5), "");
    }

    #[test]
    fn a_marquee_slides_and_turns_around() {
        let w = 12;
        let run = w / 4;
        // Same width every frame, and always exactly one run of full cells.
        for frame in 0..40 {
            let m = marquee(w, frame);
            assert_eq!(m.chars().count(), w, "frame {frame}");
            assert_eq!(m.chars().filter(|c| *c == '█').count(), run, "frame {frame}");
        }
        // It starts at the left, reaches the right, and comes back — never
        // jumping from one edge to the other.
        let span = w - run;
        assert!(marquee(w, 0).starts_with('█'));
        assert!(marquee(w, span).ends_with('█'));
        assert!(marquee(w, span * 2).starts_with('█'));
    }

    #[test]
    fn a_marquee_degenerates_without_panicking() {
        assert_eq!(marquee(0, 5), "");
        // Width 1: the run fills it, so there is nowhere to travel — the
        // `span == 0` branch, and the one that would divide by zero without it.
        assert_eq!(marquee(1, 7), "█");
        // Still slides at width 3, just with a one-cell run.
        assert_eq!(marquee(3, 99).chars().filter(|c| *c == '█').count(), 1);
    }

    #[test]
    fn sizes_read_the_way_git_writes_them() {
        assert_eq!(format_bytes(Some(512)), "512 B");
        assert_eq!(format_bytes(Some(1024)), "1.00 KiB");
        assert_eq!(format_bytes(Some(12 * 1024)), "12.0 KiB");
        assert_eq!(format_bytes(Some(5 * 1024 * 1024)), "5.00 MiB");
        assert_eq!(format_bytes(None), "");
        assert_eq!(format_rate(Some(1024)), "1.00 KiB/s");
        assert_eq!(format_rate(None), "");
    }

    #[test]
    fn transfer_line_only_speaks_when_git_did() {
        let recv = Snapshot {
            phase: Phase::Receiving,
            percent: Some(42),
            bytes: Some(1024 * 1024),
            rate: Some(512 * 1024),
        };
        assert_eq!(transfer_line(&recv), "1.00 MiB · 512.0 KiB/s");
        let resolving = Snapshot { phase: Phase::Resolving, percent: Some(42), bytes: None, rate: None };
        assert_eq!(transfer_line(&resolving), "");
    }

    #[test]
    fn a_step_labels_itself_by_state() {
        let mut s = Step::pending("tenx");
        assert_eq!(s.note(), "");
        s.state = StepState::Running(None);
        assert_eq!(s.note(), "working");
        s.state = StepState::Running(Some(Snapshot {
            phase: Phase::Receiving,
            percent: Some(1),
            bytes: None,
            rate: None,
        }));
        assert_eq!(s.note(), "receiving");
        s.state = StepState::Done("fetched".into());
        assert_eq!(s.note(), "fetched");
    }
}
