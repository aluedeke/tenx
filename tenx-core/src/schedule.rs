//! Scheduled tasks: a workspace's `schedules/<name>.toml` files, and every
//! decision the watcher makes about them — when a schedule is due, what a
//! missed slot becomes, when a run has finished or overrun its timeout, which
//! old runs may be deleted.
//!
//! A run is either a fresh task (`target = "new"`, the default), marked with a
//! `.tenx-run` file holding its `RunRecord` — which is what keeps it off the
//! Tasks list — or a message to an existing task (`target = "<slug>"`), whose
//! record lives in the schedule's state file instead: that task is the user's
//! own and must stay listed.
//!
//! Time is local: a cron slot is a wall-clock time in the zone the caller hands
//! in, so "0 6 * * *" stays at 06:00 across a daylight-saving change. A slot
//! that falls into the spring-forward gap runs at the first time after it, and
//! one in the autumn fold runs once, at its first occurrence.

use crate::status::TaskStatus;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How many runs may be going at once, across every workspace. A due run
/// beyond that waits for a free place (its slot stays due), oldest first.
pub const MAX_PARALLEL: usize = 5;
/// A run's timeout when its schedule names none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3600);
/// How old a missed slot may be and still run once when the watcher is back.
pub const DEFAULT_CATCH_UP: Duration = Duration::from_secs(12 * 3600);
/// Between the interrupt key and closing the window, for a run in its own task.
pub const STOP_GRACE: Duration = Duration::from_secs(30);
/// How long a fresh run may show no session before it counts as failed.
pub const START_GRACE: Duration = Duration::from_secs(3 * 60);
pub const DEFAULT_KEEP_OK: usize = 3;
pub const DEFAULT_KEEP_FAILED: usize = 5;
/// The marker file in a run's own task directory.
pub const RUN_FILE: &str = ".tenx-run";

/// A schedule file as written. `parse` turns it into a checked `Schedule`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScheduleFile {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    pub prompt: String,
    /// "new", or the slug of an existing task.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// Extra agent arguments as shell words, like a task's `.tenx-agent-args`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent_args: String,
    /// "45m"; "0" for none. Empty = `DEFAULT_TIMEOUT`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub timeout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub catch_up: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_ok: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_failed: Option<usize>,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    New,
    Task(String),
}

/// A schedule ready to decide on. `name` is the file's stem.
#[derive(Debug, Clone)]
pub struct Schedule {
    pub name: String,
    pub title: String,
    pub when: When,
    pub prompt: String,
    pub target: Target,
    pub repos: Option<Vec<String>>,
    pub agent: String,
    pub agent_args: String,
    pub timeout: Option<Duration>,
    pub catch_up: Duration,
    pub secrets: Vec<String>,
    pub keep_ok: usize,
    pub keep_failed: usize,
    pub paused: bool,
}

/// Check a schedule file. Errors name the field, for `tenx schedule add`
/// and for the watcher's log.
pub fn parse(name: &str, file: &ScheduleFile) -> Result<Schedule, String> {
    let when = match (&file.cron, &file.every) {
        (Some(c), None) => When::Cron(Cron::parse(c)?),
        (None, Some(e)) => {
            let d = crate::time::parse_duration(e)?;
            if d < Duration::from_secs(60) {
                return Err(format!("every = \"{e}\": shorter than a minute"));
            }
            When::Every(d)
        }
        (Some(_), Some(_)) => return Err("set either cron or every, not both".into()),
        (None, None) => return Err("set cron (e.g. \"0 6 * * 1-5\") or every (e.g. \"4h\")".into()),
    };
    if file.prompt.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    let target = match file.target.as_str() {
        "" | "new" => Target::New,
        slug => Target::Task(slug.to_string()),
    };
    if target != Target::New && file.repos.is_some() {
        return Err("repos only applies to target = \"new\"".into());
    }
    let timeout = match file.timeout.as_str() {
        "" => Some(DEFAULT_TIMEOUT),
        "0" => None,
        t => Some(crate::time::parse_duration(t).map_err(|e| format!("timeout: {e}"))?),
    };
    let catch_up = match file.catch_up.as_str() {
        "" => DEFAULT_CATCH_UP,
        "0" => Duration::ZERO,
        c => crate::time::parse_duration(c).map_err(|e| format!("catch_up: {e}"))?,
    };
    Ok(Schedule {
        name: name.to_string(),
        title: if file.title.trim().is_empty() { name.to_string() } else { file.title.trim().to_string() },
        when,
        prompt: file.prompt.clone(),
        target,
        repos: file.repos.clone(),
        agent: file.agent.clone(),
        agent_args: file.agent_args.trim().to_string(),
        timeout,
        catch_up,
        secrets: file.secrets.clone(),
        keep_ok: file.keep_ok.unwrap_or(DEFAULT_KEEP_OK),
        keep_failed: file.keep_failed.unwrap_or(DEFAULT_KEEP_FAILED),
        paused: file.paused,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum When {
    Cron(Cron),
    /// Every so long, counted from the schedule's first sighting.
    Every(Duration),
}

impl When {
    /// The first slot strictly after `after`. `anchor` is where an `Every`
    /// counts from; a cron ignores it. `None`: the schedule never fires again
    /// ("0 0 30 2 *").
    pub fn next_after(&self, after: Timestamp, anchor: Timestamp, tz: &TimeZone) -> Option<Timestamp> {
        match self {
            When::Cron(c) => c.next_after(&after.to_zoned(tz.clone())).map(|z| z.timestamp()),
            When::Every(d) => {
                let every = SignedDuration::try_from(*d).ok()?;
                if after < anchor {
                    return anchor.checked_add(every).ok();
                }
                let n = after.duration_since(anchor).as_nanos() / every.as_nanos() + 1;
                let span = every.checked_mul(i32::try_from(n).ok()?)?;
                anchor.checked_add(span).ok()
            }
        }
    }

    /// The last slot at or before `now`, starting the search at `first` (a
    /// slot known to be ≤ `now`).
    fn latest_until(&self, first: Timestamp, anchor: Timestamp, now: Timestamp, tz: &TimeZone) -> Timestamp {
        if let When::Every(d) = self {
            let Ok(every) = SignedDuration::try_from(*d) else { return first };
            let n = now.duration_since(anchor).as_nanos() / every.as_nanos();
            return i32::try_from(n)
                .ok()
                .and_then(|n| every.checked_mul(n))
                .and_then(|s| anchor.checked_add(s).ok())
                .unwrap_or(first);
        }
        // A cron that fired every minute through a week offline is ~10k steps;
        // the cap only guards against a pathological spec.
        let mut latest = first;
        for _ in 0..200_000 {
            match self.next_after(latest, anchor, tz) {
                Some(n) if n <= now => latest = n,
                _ => break,
            }
        }
        latest
    }
}

/// A five-field cron spec (minute hour day-of-month month day-of-week), or an
/// `@daily`-style alias. Lists, ranges, steps and month/day names, as Vixie
/// cron: when both day fields are restricted, a day matching either fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    dom_any: bool,
    dow_any: bool,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
    pub fn parse(spec: &str) -> Result<Cron, String> {
        let spec = spec.trim();
        let expanded = match spec {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            s if s.starts_with('@') => return Err(format!("cron \"{s}\": unknown alias")),
            s => s,
        };
        let fields: Vec<&str> = expanded.split_whitespace().collect();
        let [mi, h, dom, mo, dow] = fields[..] else {
            return Err(format!("cron \"{spec}\": want 5 fields (minute hour day month weekday)"));
        };
        let err = |e: String| format!("cron \"{spec}\": {e}");
        let mut dow_bits = field(dow, 0, 7, &DAYS).map_err(err)?;
        if dow_bits & (1 << 7) != 0 {
            dow_bits = (dow_bits & !(1 << 7)) | 1; // 7 is Sunday too
        }
        Ok(Cron {
            minute: field(mi, 0, 59, &[]).map_err(err)?,
            hour: field(h, 0, 23, &[]).map_err(err)?,
            dom: field(dom, 1, 31, &[]).map_err(err)?,
            month: field(mo, 1, 12, &MONTHS).map_err(err)?,
            dow: dow_bits,
            dom_any: dom.starts_with('*'),
            dow_any: dow.starts_with('*'),
        })
    }

    fn day_matches(&self, d: Date) -> bool {
        let dom = bit(self.dom, d.day());
        let dow = bit(self.dow, d.weekday().to_sunday_zero_offset());
        // A field starting with `*` ("*", "*/10") still filters, but only two
        // restricted fields widen to either-or.
        if self.dom_any || self.dow_any { dom && dow } else { dom || dow }
    }

    /// The first matching minute strictly after `after`, in `after`'s zone.
    pub fn next_after(&self, after: &Zoned) -> Option<Zoned> {
        let tz = after.time_zone();
        let a = after.datetime();
        let minute = SignedDuration::from_mins(1);
        let mut dt = a.date().at(a.hour(), a.minute(), 0, 0).checked_add(minute).ok()?;
        // Every step jumps to the next month, day, hour or minute that could
        // match, so even a spec matching once a year is a few hundred steps.
        for _ in 0..100_000 {
            if !bit(self.month, dt.month()) {
                dt = dt.date().first_of_month().checked_add(jiff::Span::new().months(1)).ok()?.at(0, 0, 0, 0);
            } else if !self.day_matches(dt.date()) {
                dt = dt.date().tomorrow().ok()?.at(0, 0, 0, 0);
            } else if !bit(self.hour, dt.hour()) {
                dt = DateTime::from(dt.date()).checked_add(SignedDuration::from_hours(i64::from(dt.hour()) + 1)).ok()?;
            } else if !bit(self.minute, dt.minute()) {
                dt = dt.checked_add(minute).ok()?;
            } else {
                let z = tz.to_ambiguous_zoned(dt).compatible().ok()?;
                if z.timestamp() > after.timestamp() {
                    return Some(z);
                }
                dt = dt.checked_add(minute).ok()?;
            }
        }
        None
    }
}

fn bit(set: u64, n: i8) -> bool {
    n >= 0 && set & (1u64 << n) != 0
}

/// One comma-separated cron field as a bit set over `lo..=hi`.
fn field(s: &str, lo: u32, hi: u32, names: &[&str]) -> Result<u64, String> {
    let value = |v: &str| -> Result<u32, String> {
        let lower = v.to_ascii_lowercase();
        let n = match names.iter().position(|n| *n == lower) {
            Some(i) => i as u32 + if lo == 1 { 1 } else { 0 },
            None => v.parse().map_err(|_| format!("'{v}' is not a number"))?,
        };
        if n < lo || n > hi {
            return Err(format!("{n} is outside {lo}-{hi}"));
        }
        Ok(n)
    };
    let mut bits = 0u64;
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => {
                let st: u32 = st.parse().map_err(|_| format!("step '{st}' is not a number"))?;
                if st == 0 {
                    return Err("step 0".into());
                }
                (r, st)
            }
            None => (part, 1),
        };
        let (from, to) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
            // "5/15" means 5, 20, 35, 50: from there to the end.
            (v, if step > 1 { hi } else { v })
        };
        if from > to {
            return Err(format!("range {from}-{to} runs backwards"));
        }
        for n in (from..=to).step_by(step as usize) {
            bits |= 1 << n;
        }
    }
    Ok(bits)
}

/// What the watcher keeps per schedule, in `schedules/.state/<name>.json`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct ScheduleState {
    /// When the watcher first saw the schedule: slots before it never run,
    /// and `every` counts from it.
    pub created: Option<Timestamp>,
    /// The last slot that was decided on — run, skipped or missed.
    pub last_slot: Option<Timestamp>,
    /// The run going on in an existing task (`target = "<slug>"`). A run in a
    /// task of its own keeps its record in that task's `RUN_FILE`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<RunRecord>,
}

/// What a tick does with one schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    /// Nothing to do; the next slot, if it ever comes.
    Wait(Option<Timestamp>),
    Paused,
    /// Start a run for this slot (if there is a free place; otherwise leave
    /// the state alone and it is still due on the next tick).
    Run(Timestamp),
    /// The previous run is still going: skip this slot.
    Busy(Timestamp),
    /// The slot is older than `catch_up`: record it as missed, don't run.
    Missed(Timestamp),
}

/// The decision for one schedule at `now`. Only the latest due slot counts:
/// a week offline is one run (or one miss), not one per slot.
pub fn due(s: &Schedule, state: &ScheduleState, previous_running: bool, now: Timestamp, tz: &TimeZone) -> Due {
    let anchor = state.created.unwrap_or(now);
    let from = state.last_slot.unwrap_or(anchor);
    let first = match s.when.next_after(from, anchor, tz) {
        Some(t) if t <= now => t,
        next => return if s.paused { Due::Paused } else { Due::Wait(next) },
    };
    if s.paused {
        return Due::Paused;
    }
    let slot = s.when.latest_until(first, anchor, now, tz);
    let age = now.duration_since(slot);
    if age > SignedDuration::try_from(s.catch_up).unwrap_or(SignedDuration::MAX) {
        Due::Missed(slot)
    } else if previous_running {
        Due::Busy(slot)
    } else {
        Due::Run(slot)
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Done,
    NothingToReport,
    TimedOut,
    /// Made a normal task (`tenx schedule promote`, `P` in the column)
    /// before it ended; from then on it's the user's, not the schedule's.
    Promoted,
    Skipped { reason: String },
    Failed { reason: String },
}

impl Outcome {
    pub fn ok(&self) -> bool {
        matches!(self, Outcome::Done | Outcome::NothingToReport | Outcome::Promoted)
    }

    pub fn label(&self) -> String {
        match self {
            Outcome::Done => "done".into(),
            Outcome::NothingToReport => "nothing to report".into(),
            Outcome::TimedOut => "timed out".into(),
            Outcome::Promoted => "promoted to a task".into(),
            Outcome::Skipped { reason } => format!("skipped: {reason}"),
            Outcome::Failed { reason } => format!("failed: {reason}"),
        }
    }
}

/// One run: the `RUN_FILE` of a run's own task, the `running` entry of an
/// existing-task run, and a line of the schedule's history once it ends.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RunRecord {
    pub schedule: String,
    /// The task it runs in (its own, or the target).
    pub task: String,
    pub slot: Timestamp,
    pub started: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// The agent said there was nothing for a human (`tenx schedule quiet`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nothing_to_report: bool,
    /// Opened in the Runs tab since it ended.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
}

impl RunRecord {
    /// Unread: it ended with something to look at, and nobody has.
    pub fn unread(&self) -> bool {
        !self.read && self.outcome.as_ref().is_some_and(|o| *o != Outcome::NothingToReport)
    }
}

/// Whether a running run has ended, from its task's status. `changed` is when
/// that status began: a `Done` from before the run started (an existing task
/// sitting finished) is not this run's.
pub fn settle(rec: &RunRecord, status: TaskStatus, changed: Option<Timestamp>, now: Timestamp) -> Option<Outcome> {
    match status {
        TaskStatus::Working | TaskStatus::Blocked | TaskStatus::Signaled => None,
        // Interrupted for the timeout and now quiet: that's the timeout, even
        // though the interrupted turn ends as an ordinary `Done`.
        _ if rec.interrupted.is_some() => Some(Outcome::TimedOut),
        TaskStatus::Done if changed.is_some_and(|c| c >= rec.started) => Some(if rec.nothing_to_report {
            Outcome::NothingToReport
        } else {
            Outcome::Done
        }),
        TaskStatus::Done | TaskStatus::Idle => {
            if status == TaskStatus::Idle && changed.is_some_and(|c| c >= rec.started) {
                Some(Outcome::Failed { reason: "the agent exited".into() })
            } else if now.duration_since(rec.started) > SignedDuration::try_from(START_GRACE).unwrap_or_default() {
                Some(Outcome::Failed { reason: "the agent did not start".into() })
            } else {
                None
            }
        }
    }
}

/// The next step against a run's timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Keep,
    /// Send the agent its interrupt key and note the time.
    Interrupt,
    /// Close the run's window (only a run in its own task gets here).
    Close,
}

/// The time counts from the start, waiting on a human included: a run nobody
/// answers must end, or it would hold back every later slot.
pub fn stop_step(rec: &RunRecord, timeout: Option<Duration>, own_task: bool, now: Timestamp) -> Stop {
    let Some(limit) = timeout.and_then(|t| SignedDuration::try_from(t).ok()) else { return Stop::Keep };
    if now.duration_since(rec.started) < limit {
        return Stop::Keep;
    }
    match rec.interrupted {
        None => Stop::Interrupt,
        Some(at) if own_task && now.duration_since(at) >= SignedDuration::try_from(STOP_GRACE).unwrap_or_default() => {
            Stop::Close
        }
        Some(_) => Stop::Keep,
    }
}

/// A run's display name: the schedule's title and the slot, to the minute.
/// Its slug (the task's directory and branch) is made from it, so two slots
/// never share a task.
pub fn run_title(title: &str, slot: Timestamp, tz: &TimeZone) -> String {
    format!("{title} · {}", slot.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M"))
}

/// What the prompt's placeholders stand for.
pub struct PromptVars<'a> {
    pub slot: Timestamp,
    pub last_run: Option<Timestamp>,
    pub last_run_pr: Option<&'a str>,
}

/// The prompt a run's agent gets: `{date}`, `{last_run}` and `{last_run_pr}`
/// filled in (any other braces are left as they are), and — for a run in its
/// own task — how to say there is nothing to report.
pub fn render_prompt(s: &Schedule, vars: &PromptVars, tz: &TimeZone) -> String {
    let local = |t: Timestamp| t.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M").to_string();
    let mut out = s
        .prompt
        .trim()
        .replace("{date}", &vars.slot.to_zoned(tz.clone()).strftime("%Y-%m-%d").to_string())
        .replace("{last_run}", &vars.last_run.map(local).unwrap_or_else(|| "never".into()))
        .replace("{last_run_pr}", vars.last_run_pr.unwrap_or("none"));
    if s.target == Target::New {
        out.push_str(&format!(
            "\n\n(This is a scheduled run of \"{}\". If you find nothing that needs a human's attention, \
             run `tenx schedule quiet` before you finish.)",
            s.title
        ));
    }
    out
}

/// One finished-or-running run in a task of its own, for `prunable`.
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub started: Timestamp,
    pub outcome: Option<Outcome>,
    /// Nothing to lose: no uncommitted changes, no unpushed commits, no open PR.
    pub clean: bool,
    pub pinned: bool,
}

/// Indexes of runs that may be deleted: past the newest `keep_ok` good ones
/// or `keep_failed` failed ones, ended, clean and not pinned. A run beyond
/// the limit with something to lose stays — deleting it would delete work.
pub fn prunable(runs: &[RunSummary], keep_ok: usize, keep_failed: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..runs.len()).collect();
    order.sort_by(|a, b| runs[*b].started.cmp(&runs[*a].started));
    let (mut ok, mut failed) = (0, 0);
    let mut out = Vec::new();
    for i in order {
        let Some(outcome) = &runs[i].outcome else { continue };
        let seen = if outcome.ok() {
            ok += 1;
            ok
        } else {
            failed += 1;
            failed
        };
        let keep = if outcome.ok() { keep_ok } else { keep_failed };
        if seen > keep && runs[i].clean && !runs[i].pinned {
            out.push(i);
        }
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin() -> TimeZone {
        // Fixed rules, so the tests need no tzdb: CET/CEST with the EU switch.
        TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    fn at(s: &str) -> Timestamp {
        let dt: DateTime = s.parse().unwrap();
        berlin().to_ambiguous_zoned(dt).compatible().unwrap().timestamp()
    }

    fn local(t: Timestamp) -> String {
        t.to_zoned(berlin()).strftime("%Y-%m-%d %H:%M %a").to_string()
    }

    fn next(spec: &str, after: &str) -> String {
        let c = Cron::parse(spec).unwrap();
        local(c.next_after(&at(after).to_zoned(berlin())).unwrap().timestamp())
    }

    fn sched(toml_cron: Option<&str>, every: Option<&str>) -> Schedule {
        let f = ScheduleFile {
            cron: toml_cron.map(String::from),
            every: every.map(String::from),
            prompt: "check things".into(),
            ..Default::default()
        };
        parse("nightly", &f).unwrap()
    }

    #[test]
    fn cron_finds_the_next_minute() {
        assert_eq!(next("0 6 * * *", "2026-11-08T05:59"), "2026-11-08 06:00 Sun");
        assert_eq!(next("0 6 * * *", "2026-11-08T06:00"), "2026-11-09 06:00 Mon");
        assert_eq!(next("*/15 * * * *", "2026-11-08T10:07:30"), "2026-11-08 10:15 Sun");
        assert_eq!(next("0 6 * * 1-5", "2026-11-07T07:00"), "2026-11-09 06:00 Mon");
        assert_eq!(next("30 9 1 * *", "2026-11-08T00:00"), "2026-12-01 09:30 Tue");
        assert_eq!(next("0 0 1 jan *", "2026-11-08T00:00"), "2027-01-01 00:00 Fri");
        assert_eq!(next("0 8 * * mon,fri", "2026-11-09T08:00"), "2026-11-13 08:00 Fri");
        assert_eq!(next("@weekly", "2026-11-08T00:00"), "2026-11-15 00:00 Sun");
        assert_eq!(next("0 12 * * 7", "2026-11-09T00:00"), "2026-11-15 12:00 Sun");
    }

    #[test]
    fn cron_day_fields_or_when_both_are_set() {
        // The 13th, or any Friday: Vixie cron's rule.
        assert_eq!(next("0 0 13 * 5", "2026-11-08T00:00"), "2026-11-13 00:00 Fri");
        assert_eq!(next("0 0 13 * 5", "2026-11-13T00:00"), "2026-11-20 00:00 Fri");
        assert_eq!(next("0 0 */10 * *", "2026-11-08T00:00"), "2026-11-11 00:00 Wed");
    }

    #[test]
    fn cron_keeps_wall_clock_across_dst() {
        // 06:00 stays 06:00 on both sides of the autumn change (25 Oct 2026).
        assert_eq!(next("0 6 * * *", "2026-10-24T07:00"), "2026-10-25 06:00 Sun");
        // 02:30 doesn't exist on 29 Mar 2026: runs at the first time after the gap.
        assert_eq!(next("30 2 * * *", "2026-03-28T03:00"), "2026-03-29 03:30 Sun");
        // 02:30 happens twice on 25 Oct: runs once.
        let c = Cron::parse("30 2 * * *").unwrap();
        let first = c.next_after(&at("2026-10-24T12:00").to_zoned(berlin())).unwrap();
        let second = c.next_after(&first).unwrap();
        assert_eq!(local(second.timestamp()), "2026-10-26 02:30 Mon");
    }

    #[test]
    fn cron_rejects_garbage_and_never() {
        for bad in ["", "* * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "5-1 * * * *", "*/0 * * * *", "@often", "x * * * *"] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
        let never = Cron::parse("0 0 30 2 *").unwrap();
        assert!(never.next_after(&at("2026-11-08T00:00").to_zoned(berlin())).is_none());
    }

    #[test]
    fn parse_checks_the_file() {
        let base = ScheduleFile { cron: Some("@daily".into()), prompt: "p".into(), ..Default::default() };
        let s = parse("deps", &base).unwrap();
        assert_eq!((s.title.as_str(), s.timeout, s.target.clone()), ("deps", Some(DEFAULT_TIMEOUT), Target::New));
        let no_timeout = ScheduleFile { timeout: "0".into(), ..base.clone() };
        assert_eq!(parse("deps", &no_timeout).unwrap().timeout, None);
        assert!(parse("deps", &ScheduleFile { every: Some("1h".into()), ..base.clone() }).is_err());
        assert!(parse("deps", &ScheduleFile { cron: None, ..base.clone() }).is_err());
        assert!(parse("deps", &ScheduleFile { prompt: " ".into(), ..base.clone() }).is_err());
        assert!(parse("deps", &ScheduleFile { cron: None, every: Some("30s".into()), ..base.clone() }).is_err());
        let task = ScheduleFile { target: "my-task".into(), repos: Some(vec!["a".into()]), ..base };
        assert!(parse("deps", &task).is_err());
    }

    #[test]
    fn due_runs_the_slot_that_just_passed() {
        let s = sched(Some("0 6 * * *"), None);
        let state = ScheduleState { created: Some(at("2026-11-01T00:00")), last_slot: Some(at("2026-11-07T06:00")), running: None };
        assert_eq!(due(&s, &state, false, at("2026-11-08T05:59"), &berlin()), Due::Wait(Some(at("2026-11-08T06:00"))));
        assert_eq!(due(&s, &state, false, at("2026-11-08T06:00:40"), &berlin()), Due::Run(at("2026-11-08T06:00")));
        assert_eq!(due(&s, &state, true, at("2026-11-08T06:00:40"), &berlin()), Due::Busy(at("2026-11-08T06:00")));
    }

    #[test]
    fn due_catches_up_once_within_the_window() {
        let s = sched(Some("0 6 * * *"), None);
        let state = ScheduleState { created: Some(at("2026-11-01T00:00")), last_slot: Some(at("2026-11-03T06:00")), running: None };
        // Days offline: only the latest slot counts, and it's 4h old.
        assert_eq!(due(&s, &state, false, at("2026-11-08T10:00"), &berlin()), Due::Run(at("2026-11-08T06:00")));
        // 13h old: past the 12h window.
        assert_eq!(due(&s, &state, false, at("2026-11-08T19:00"), &berlin()), Due::Missed(at("2026-11-08T06:00")));
    }

    #[test]
    fn due_never_runs_slots_from_before_the_schedule_existed() {
        let s = sched(Some("0 6 * * *"), None);
        let fresh = ScheduleState { created: Some(at("2026-11-08T09:00")), ..Default::default() };
        assert_eq!(due(&s, &fresh, false, at("2026-11-08T09:01"), &berlin()), Due::Wait(Some(at("2026-11-09T06:00"))));
    }

    #[test]
    fn due_pauses_and_every_counts_from_creation() {
        let mut s = sched(None, Some("4h"));
        let state = ScheduleState { created: Some(at("2026-11-08T01:00")), ..Default::default() };
        assert_eq!(due(&s, &state, false, at("2026-11-08T04:59"), &berlin()), Due::Wait(Some(at("2026-11-08T05:00"))));
        assert_eq!(due(&s, &state, false, at("2026-11-08T09:30"), &berlin()), Due::Run(at("2026-11-08T09:00")));
        s.paused = true;
        assert_eq!(due(&s, &state, false, at("2026-11-08T09:30"), &berlin()), Due::Paused);
    }

    fn rec(started: &str) -> RunRecord {
        RunRecord {
            schedule: "nightly".into(),
            task: "nightly-2026-11-08-0600".into(),
            slot: at(started),
            started: at(started),
            interrupted: None,
            ended: None,
            outcome: None,
            nothing_to_report: false,
            read: false,
            pr: None,
        }
    }

    #[test]
    fn settle_reads_the_status() {
        let r = rec("2026-11-08T06:00");
        let now = at("2026-11-08T06:10");
        assert_eq!(settle(&r, TaskStatus::Working, None, now), None);
        assert_eq!(settle(&r, TaskStatus::Blocked, None, now), None);
        assert_eq!(settle(&r, TaskStatus::Done, Some(at("2026-11-08T06:09")), now), Some(Outcome::Done));
        // A Done from before the run started is the existing task's old turn.
        assert_eq!(settle(&r, TaskStatus::Done, Some(at("2026-11-07T20:00")), at("2026-11-08T06:01")), None);
        let quiet = RunRecord { nothing_to_report: true, ..r.clone() };
        assert_eq!(settle(&quiet, TaskStatus::Done, Some(now), now), Some(Outcome::NothingToReport));
        assert_eq!(
            settle(&r, TaskStatus::Idle, Some(at("2026-11-08T06:05")), now),
            Some(Outcome::Failed { reason: "the agent exited".into() })
        );
        assert_eq!(settle(&r, TaskStatus::Idle, None, at("2026-11-08T06:02")), None);
        assert_eq!(
            settle(&r, TaskStatus::Idle, None, at("2026-11-08T06:04")),
            Some(Outcome::Failed { reason: "the agent did not start".into() })
        );
        let stopped = RunRecord { interrupted: Some(at("2026-11-08T07:00")), ..r };
        assert_eq!(settle(&stopped, TaskStatus::Done, Some(at("2026-11-08T07:00:05")), now), Some(Outcome::TimedOut));
        assert_eq!(settle(&stopped, TaskStatus::Working, None, now), None);
    }

    #[test]
    fn stop_interrupts_then_closes_own_tasks_only() {
        let r = rec("2026-11-08T06:00");
        let hour = Some(Duration::from_secs(3600));
        assert_eq!(stop_step(&r, hour, true, at("2026-11-08T06:59")), Stop::Keep);
        assert_eq!(stop_step(&r, hour, true, at("2026-11-08T07:00")), Stop::Interrupt);
        assert_eq!(stop_step(&r, None, true, at("2026-11-09T07:00")), Stop::Keep);
        let r = RunRecord { interrupted: Some(at("2026-11-08T07:00")), ..r };
        assert_eq!(stop_step(&r, hour, true, at("2026-11-08T07:00:20")), Stop::Keep);
        assert_eq!(stop_step(&r, hour, true, at("2026-11-08T07:00:30")), Stop::Close);
        assert_eq!(stop_step(&r, hour, false, at("2026-11-08T08:00")), Stop::Keep);
    }

    #[test]
    fn slug_title_and_prompt() {
        let slot = at("2026-11-08T06:00");
        assert_eq!(crate::slug::slugify(&run_title("Nightly deps", slot, &berlin())), "nightly-deps-2026-11-08-06-00");
        assert_eq!(run_title("Nightly deps", slot, &berlin()), "Nightly deps · 2026-11-08 06:00");
        let mut s = sched(Some("@daily"), None);
        s.prompt = "Since {last_run} ({last_run_pr}) on {date}: {keep}".into();
        let vars = PromptVars { slot, last_run: Some(at("2026-11-07T06:00")), last_run_pr: None };
        let p = render_prompt(&s, &vars, &berlin());
        assert!(p.starts_with("Since 2026-11-07 06:00 (none) on 2026-11-08: {keep}\n\n"), "{p}");
        assert!(p.contains("tenx schedule quiet"));
        s.target = Target::Task("mine".into());
        assert!(!render_prompt(&s, &vars, &berlin()).contains("tenx schedule quiet"));
    }

    #[test]
    fn records_round_trip_and_unread() {
        let mut r = rec("2026-11-08T06:00");
        r.outcome = Some(Outcome::Failed { reason: "x".into() });
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""outcome":{"kind":"failed","reason":"x"}"#), "{json}");
        assert_eq!(serde_json::from_str::<RunRecord>(&json).unwrap(), r);
        assert!(r.unread());
        r.outcome = Some(Outcome::NothingToReport);
        assert!(!r.unread());
    }

    #[test]
    fn prune_keeps_the_newest_and_anything_with_work() {
        let run = |day: u32, outcome: Option<Outcome>, clean: bool| RunSummary {
            started: at(&format!("2026-11-{day:02}T06:00")),
            outcome,
            clean,
            pinned: false,
        };
        let failed = || Some(Outcome::Failed { reason: "x".into() });
        let runs = vec![
            run(1, Some(Outcome::Done), true),            // 0: past the limit, clean: pruned
            run(2, Some(Outcome::Done), false),           // 1: past the limit, has work: kept
            run(3, Some(Outcome::NothingToReport), true), // 2
            run(4, Some(Outcome::Done), true),            // 3
            run(5, failed(), true),                       // 4: newest failed, kept
            run(6, None, true),                           // 5: running, never
            run(7, Some(Outcome::Done), true),            // 6
        ];
        // ok runs, newest first: 6, 3, 2 (kept), 1 (has work), 0 (pruned).
        assert_eq!(prunable(&runs, 3, 1), vec![0]);
        assert_eq!(prunable(&runs, 1, 0), vec![0, 2, 3, 4]);
    }
}
