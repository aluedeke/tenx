//! `tenx schedule`: a workspace's scheduled tasks. Each schedule is one file,
//! `<workspace>/schedules/<name>.toml` (`tenx_core::schedule::ScheduleFile`);
//! what the watcher remembers about it sits beside it in `.state/<name>.json`
//! (`ScheduleState`), and every run that ended — or a slot that was skipped —
//! is a line of `.state/<name>.jsonl`, which outlives the runs themselves.
//!
//! The watcher calls [`Ticker::tick`] about once a minute: it follows the
//! runs going on (has the agent finished? has the timeout come?), starts the
//! runs that are due — at most `MAX_PARALLEL` at a time, the rest wait for a
//! place — and deletes old runs that have nothing to lose. Every decision is
//! `tenx_core::schedule`; this file reads and writes the files and drives tmux.
//!
//! A run is a task of its own, marked with `.tenx-run` (which keeps it off the
//! Tasks list and on the Runs tab), or a message to an existing task. Starting
//! a run of its own clones and branches like any task, which can take
//! minutes, so it happens on a thread of its own: the tick never waits for git.
//!
//! The timer runs inside the watcher, so a schedule fires while the tenx tmux
//! server is up — it survives closing every terminal, not a reboot. On a
//! headless Mac, `tenx web service install` starts the server and the watcher
//! at login; a slot missed in between runs once afterwards if it is less than
//! the schedule's `catch_up` old.

use anyhow::{bail, Context, Result};
use jiff::tz::TimeZone;
use jiff::Timestamp;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use tenx_core::schedule::{self as rules, Due, Outcome, RunRecord, Schedule, ScheduleFile, ScheduleState, Stop, Target, RUN_FILE};

use crate::workspace::{self, Workspace};

pub const SCHEDULES_DIR: &str = "schedules";
const STATE_DIR: &str = ".state";
/// What ends a turn in all three agents: Claude Code, Codex and pi stop the
/// running turn on Escape and stay open. Ctrl+C would be worse: twice exits
/// Claude Code.
const INTERRUPT_KEY: &str = "Escape";

// ── Files ────────────────────────────────────────────────────────────────────

pub fn dir(ws: &Workspace) -> PathBuf {
    ws.dir.join(SCHEDULES_DIR)
}

fn file_path(ws: &Workspace, name: &str) -> PathBuf {
    dir(ws).join(format!("{name}.toml"))
}

fn state_path(ws: &Workspace, name: &str) -> PathBuf {
    dir(ws).join(STATE_DIR).join(format!("{name}.json"))
}

fn history_path(ws: &Workspace, name: &str) -> PathBuf {
    dir(ws).join(STATE_DIR).join(format!("{name}.jsonl"))
}

/// The workspace's schedule names (file stems), sorted.
pub fn names(ws: &Workspace) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir(ws))
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

pub fn read_file(ws: &Workspace, name: &str) -> Result<ScheduleFile> {
    let path = file_path(ws, name);
    let text = std::fs::read_to_string(&path).with_context(|| format!("no schedule {name:?} ({})", path.display()))?;
    toml::from_str(&text).with_context(|| format!("read {}", path.display()))
}

pub fn load(ws: &Workspace, name: &str) -> Result<Schedule> {
    rules::parse(name, &read_file(ws, name)?).map_err(|e| anyhow::anyhow!("{}: {e}", file_path(ws, name).display()))
}

fn write_file(ws: &Workspace, name: &str, file: &ScheduleFile) -> Result<()> {
    std::fs::create_dir_all(dir(ws))?;
    write_atomic(&file_path(ws, name), &toml::to_string_pretty(file)?)
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

pub fn read_state(ws: &Workspace, name: &str) -> ScheduleState {
    std::fs::read_to_string(state_path(ws, name)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_state(ws: &Workspace, name: &str, state: &ScheduleState) -> Result<()> {
    write_atomic(&state_path(ws, name), &serde_json::to_string_pretty(state)?)
}

/// Every recorded run and skipped slot, oldest first.
pub fn history(ws: &Workspace, name: &str) -> Vec<RunRecord> {
    std::fs::read_to_string(history_path(ws, name))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn append_history(ws: &Workspace, name: &str, rec: &RunRecord) {
    use std::io::Write;
    let path = history_path(ws, name);
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    let line = serde_json::to_string(rec).unwrap_or_default();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

/// A task's run record, if it is a scheduled run.
pub fn read_run(task_dir: &Path) -> Option<RunRecord> {
    serde_json::from_str(&std::fs::read_to_string(task_dir.join(RUN_FILE)).ok()?).ok()
}

pub fn is_run(task_dir: &Path) -> bool {
    task_dir.join(RUN_FILE).is_file()
}

fn write_run(task_dir: &Path, rec: &RunRecord) -> Result<()> {
    write_atomic(&task_dir.join(RUN_FILE), &serde_json::to_string_pretty(rec)?)
}

/// Mark a run as read — the column's Runs tab, on opening one.
pub fn mark_read(task_dir: &Path) {
    if let Some(mut rec) = read_run(task_dir)
        && rec.outcome.is_some()
        && !rec.read
    {
        rec.read = true;
        let _ = write_run(task_dir, &rec);
    }
}

pub fn tz() -> TimeZone {
    TimeZone::system()
}

fn timestamp(t: SystemTime) -> Option<Timestamp> {
    Timestamp::try_from(t).ok()
}

/// A schedule name as typed: `name` or `workspace/name`, looked up in
/// `--ws-dir`, else the workspace around the current directory, else every
/// registered one (it must be unique there).
fn resolve(ws_dir: Option<&str>, arg: &str) -> Result<(Workspace, String)> {
    if let Some(dir) = ws_dir {
        return Ok((crate::cli::task::load_ws_arg(dir)?, arg.to_string()));
    }
    if let Some((ws_name, name)) = arg.split_once('/') {
        let ws = crate::cli::task::load_ws_arg(ws_name)?;
        return Ok((ws, name.to_string()));
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(ws) = workspace::find(&cwd)
        && file_path(&ws, arg).is_file()
    {
        return Ok((ws, arg.to_string()));
    }
    let found: Vec<Workspace> =
        workspace::registered_workspaces().into_iter().filter(|ws| file_path(ws, arg).is_file()).collect();
    match found.len() {
        1 => Ok((found.into_iter().next().unwrap_or_else(|| unreachable!()), arg.to_string())),
        0 => bail!("no schedule {arg:?} — `tenx schedule list` shows them"),
        _ => bail!(
            "{arg:?} is a schedule in {} — say which, e.g. `{}/{arg}`",
            found.iter().map(|w| w.config.name.as_str()).collect::<Vec<_>>().join(", "),
            found[0].config.name
        ),
    }
}

/// The workspace a new schedule goes into.
fn target_ws(ws_dir: Option<&str>) -> Result<Workspace> {
    match ws_dir {
        Some(d) => crate::cli::task::load_ws_arg(d),
        None => workspace::find(&std::env::current_dir()?),
    }
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// `tenx schedule add`. From an agent (no terminal) the schedule is written
/// paused: a schedule is a standing instruction, so turning it on is a
/// person's call (`resume` needs a terminal too).
pub fn add(ws_dir: Option<&str>, name: &str, mut file: ScheduleFile, replace: bool) -> Result<()> {
    let ws = target_ws(ws_dir)?;
    let slug = workspace::slugify(name);
    if slug.is_empty() || slug != name {
        bail!("schedule names are lowercase letters, digits and dashes — e.g. {:?}", if slug.is_empty() { "nightly-deps" } else { &slug });
    }
    // The adhoc workspace has no repos, so "all of them" is none.
    if ws.is_adhoc() && file.repos.is_none() && file.target.is_empty() {
        file.repos = Some(Vec::new());
    }
    let s = rules::parse(name, &file).map_err(|e| anyhow::anyhow!(e))?;
    if s.when.next_after(Timestamp::now(), Timestamp::now(), &tz()).is_none() {
        bail!("that schedule never fires");
    }
    match &s.target {
        Target::New if !s.secrets.is_empty() => bail!(
            "secrets need target = an existing task: a new task has none released, and releasing one takes a person \
             typing the passphrase into an open window (`tenx secrets fulfill`)"
        ),
        Target::Task(t) => {
            ws.find_task(t).with_context(|| format!("target task {t:?}"))?;
        }
        Target::New => {
            for r in s.repos.iter().flatten() {
                if ws.find_repo(r).is_none() {
                    bail!("no repo {r:?} in workspace {}", ws.config.name);
                }
            }
        }
    }
    if file_path(&ws, name).exists() && !replace {
        bail!("schedule {name:?} exists — `--replace` to overwrite it");
    }
    let by_agent = !crate::cli::secrets::tty_available();
    if by_agent {
        file.paused = true;
    }
    write_file(&ws, name, &file)?;
    let next = s.when.next_after(Timestamp::now(), Timestamp::now(), &tz());
    println!("schedule {}/{name}: next run {}", ws.config.name, fmt_time(next));
    if by_agent {
        println!("paused until a person turns it on: tenx schedule resume {}/{name}", ws.config.name);
    }
    if s.agent_args.is_empty() {
        eprintln!(
            "! no agent_args: an unattended run stops at its first permission prompt. Allow what it needs, e.g. \
             --agent-args \"--permission-mode acceptEdits --allowedTools 'Bash(cargo *)'\""
        );
    }
    Ok(())
}

pub fn rm(ws_dir: Option<&str>, name: &str) -> Result<()> {
    let (ws, name) = resolve(ws_dir, name)?;
    std::fs::remove_file(file_path(&ws, &name))?;
    let _ = std::fs::remove_file(state_path(&ws, &name));
    println!("removed schedule {}/{name} (its runs and history stay)", ws.config.name);
    Ok(())
}

/// `pause` / `resume`, one schedule or `--all`. Resuming needs a terminal.
pub fn set_paused(ws_dir: Option<&str>, name: Option<&str>, paused: bool) -> Result<()> {
    if !paused && !crate::cli::secrets::tty_available() {
        bail!("turning a schedule on is a person's call: run `tenx schedule resume` in a terminal");
    }
    let targets: Vec<(Workspace, String)> = match name {
        Some(n) => vec![resolve(ws_dir, n)?],
        None => workspace::registered_workspaces().into_iter().flat_map(|ws| names(&ws).into_iter().map(move |n| (ws.clone(), n))).collect(),
    };
    for (ws, n) in targets {
        let mut file = read_file(&ws, &n)?;
        if file.paused != paused {
            file.paused = paused;
            write_file(&ws, &n, &file)?;
        }
        println!("{} {}/{n}", if paused { "paused" } else { "resumed" }, ws.config.name);
    }
    Ok(())
}

/// `tenx schedule run`: start a run now, whatever the schedule says (paused
/// included) — for trying a schedule out. Waits for the run to be set up.
pub fn run_now(ws_dir: Option<&str>, name: &str) -> Result<()> {
    let (ws, name) = resolve(ws_dir, name)?;
    let s = load(&ws, &name)?;
    if !crate::tmux::server_running() {
        bail!("the '{}' session isn't running — run 'tenx' to start it first", crate::tmux::SESSION);
    }
    if running_of(&ws, &s.name) {
        bail!("a run of {name:?} is still going");
    }
    let now = Timestamp::now();
    let tz = tz();
    match start(&ws, &s, now, &tz) {
        Ok(Some(task)) => println!("started {}/{task}", ws.config.name),
        Ok(None) => println!("skipped — `tenx schedule runs {name}` says why"),
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Make a run a normal task: drop its marker, so it moves to the Tasks list
/// and no schedule follows it, times it out or deletes it. A run still going
/// is recorded in its schedule's history as promoted.
pub fn promote_dir(task_dir: &Path) -> Result<()> {
    let rec = read_run(task_dir).context("not a scheduled run")?;
    if rec.outcome.is_none()
        && let Some(ws) = task_dir.parent().and_then(Path::parent).and_then(|d| workspace::load(d).ok())
    {
        let mut live = Live { ws, rec, own: Some(task_dir.to_path_buf()) };
        live.finish(Outcome::Promoted, Timestamp::now());
    }
    std::fs::remove_file(task_dir.join(RUN_FILE))?;
    Ok(())
}

/// `tenx schedule promote [task]`: the run named, or the one around the
/// current directory.
pub fn promote(ws_dir: Option<&str>, task: Option<&str>) -> Result<()> {
    let dir = match task {
        Some(t) => {
            let (ws, slug) = crate::cli::task::resolve_task(ws_dir, t)?;
            ws.find_task(&slug)?.path
        }
        None => {
            let cwd = std::env::current_dir()?;
            cwd.ancestors().find(|d| is_run(d)).map(Path::to_path_buf).context("not inside a scheduled run")?
        }
    };
    promote_dir(&dir)?;
    println!("{} is a normal task now", dir.file_name().unwrap_or_default().to_string_lossy());
    Ok(())
}

/// `tenx schedule quiet`: the agent of a run found nothing for a person. The
/// run then ends as "nothing to report" and isn't shown as unread.
pub fn quiet() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(task_dir) = cwd.ancestors().find(|d| is_run(d)) else {
        bail!("not inside a scheduled run");
    };
    let mut rec = read_run(task_dir).context("unreadable run record")?;
    rec.nothing_to_report = true;
    write_run(task_dir, &rec)?;
    println!("noted: nothing to report");
    Ok(())
}

fn fmt_time(t: Option<Timestamp>) -> String {
    match t {
        Some(t) => t.to_zoned(tz()).strftime("%a %Y-%m-%d %H:%M").to_string(),
        None => "never".into(),
    }
}

fn when_label(f: &ScheduleFile) -> String {
    match (&f.cron, &f.every) {
        (Some(c), _) => format!("cron {c}"),
        (_, Some(e)) => format!("every {e}"),
        _ => "?".into(),
    }
}

/// `tenx schedule list`: every schedule in every registered workspace.
pub fn list(json: bool) -> Result<()> {
    let tz = tz();
    let now = Timestamp::now();
    let mut rows = Vec::new();
    for ws in workspace::registered_workspaces() {
        for name in names(&ws) {
            let file = read_file(&ws, &name);
            let parsed = file.as_ref().map_err(|e| e.to_string()).and_then(|f| rules::parse(&name, f));
            let state = read_state(&ws, &name);
            let last = history(&ws, &name).pop();
            let next = parsed.as_ref().ok().and_then(|s| {
                let anchor = state.created.unwrap_or(now);
                s.when.next_after(state.last_slot.unwrap_or(anchor).max(now), anchor, &tz)
            });
            rows.push((ws.config.name.clone(), name, file.ok(), parsed, next, last));
        }
    }
    if json {
        let out: Vec<serde_json::Value> = rows
            .iter()
            .map(|(ws, name, file, parsed, next, last)| {
                serde_json::json!({
                    "workspace": ws, "name": name,
                    "title": parsed.as_ref().map(|s| s.title.clone()).ok(),
                    "when": file.as_ref().map(when_label),
                    "paused": file.as_ref().is_some_and(|f| f.paused),
                    "error": parsed.as_ref().err(),
                    "next": next, "last": last,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no schedules — add one with `tenx schedule add`");
        return Ok(());
    }
    for (ws, name, file, parsed, next, last) in rows {
        let paused = file.as_ref().is_some_and(|f| f.paused);
        let when = file.as_ref().map(when_label).unwrap_or_default();
        match parsed {
            Err(e) => println!("{ws}/{name}  ! {e}"),
            Ok(s) => {
                let next = if paused { "paused".to_string() } else { format!("next {}", fmt_time(next)) };
                let last = last.map(|r| format!("  last {} {}", fmt_time(Some(r.started)), r.outcome.map(|o| o.label()).unwrap_or_else(|| "running".into()))).unwrap_or_default();
                println!("{ws}/{name}  {}  {when}  {next}{last}", s.title);
            }
        }
    }
    Ok(())
}

/// `tenx schedule runs [name]`: the runs on disk (newest first) and, with a
/// name, its history too.
pub fn runs(ws_dir: Option<&str>, name: Option<&str>, json: bool) -> Result<()> {
    let mut recs: Vec<(String, RunRecord)> = Vec::new();
    match name {
        Some(n) => {
            let (ws, n) = resolve(ws_dir, n)?;
            let on_disk: HashSet<String> =
                own_runs(&ws).into_iter().filter(|(_, r)| r.schedule == n).map(|(_, r)| r.task).collect();
            for r in history(&ws, &n) {
                if !on_disk.contains(&r.task) {
                    recs.push((ws.config.name.clone(), r));
                }
            }
            recs.extend(own_runs(&ws).into_iter().filter(|(_, r)| r.schedule == n).map(|(_, r)| (ws.config.name.clone(), r)));
            if let Some(r) = read_state(&ws, &n).running {
                recs.push((ws.config.name.clone(), r));
            }
        }
        None => {
            for ws in workspace::registered_workspaces() {
                recs.extend(own_runs(&ws).into_iter().map(|(_, r)| (ws.config.name.clone(), r)));
            }
        }
    }
    recs.sort_by_key(|r| std::cmp::Reverse(r.1.started));
    if json {
        let out: Vec<serde_json::Value> =
            recs.iter().map(|(ws, r)| serde_json::json!({"workspace": ws, "run": r, "unread": r.unread()})).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if recs.is_empty() {
        println!("no runs");
    }
    for (ws, r) in recs {
        let outcome = r.outcome.as_ref().map(|o| o.label()).unwrap_or_else(|| "running".into());
        let task = if r.task.is_empty() { String::new() } else { format!("  {ws}/{}", r.task) };
        let pr = r.pr.as_deref().map(|p| format!("  {p}")).unwrap_or_default();
        let unread = if r.unread() { "● " } else { "  " };
        println!("{unread}{}  {}  {outcome}{task}{pr}", r.schedule, fmt_time(Some(r.started)));
    }
    Ok(())
}

// ── The tick ─────────────────────────────────────────────────────────────────

/// Runs in tasks of their own in `ws`: (task dir, record).
pub fn own_runs(ws: &Workspace) -> Vec<(PathBuf, RunRecord)> {
    ws.tasks().unwrap_or_default().into_iter().filter_map(|t| read_run(&t.path).map(|r| (t.path, r))).collect()
}

/// Whether a run of this schedule is going on.
fn running_of(ws: &Workspace, name: &str) -> bool {
    read_state(ws, name).running.is_some() || own_runs(ws).iter().any(|(_, r)| r.schedule == name && r.outcome.is_none())
}

/// A run being followed: where its record lives decides how it is saved.
struct Live {
    ws: Workspace,
    rec: RunRecord,
    /// The run's own task directory; `None` for a run in an existing task,
    /// whose record is the schedule state's `running`.
    own: Option<PathBuf>,
}

impl Live {
    fn save(&self) {
        let _ = match &self.own {
            Some(dir) => write_run(dir, &self.rec),
            None => {
                let mut st = read_state(&self.ws, &self.rec.schedule);
                st.running = if self.rec.outcome.is_none() { Some(self.rec.clone()) } else { None };
                write_state(&self.ws, &self.rec.schedule, &st)
            }
        };
    }

    fn task_dir(&self) -> Option<PathBuf> {
        self.own.clone().or_else(|| self.ws.find_task(&self.rec.task).ok().map(|t| t.path))
    }

    fn finish(&mut self, outcome: Outcome, now: Timestamp) {
        self.rec.outcome = Some(outcome);
        self.rec.ended = Some(now);
        if let Some(dir) = self.task_dir() {
            self.rec.pr = crate::live::read(&dir).prs.first().map(|p| p.url.clone());
        }
        self.save();
        append_history(&self.ws, &self.rec.schedule, &self.rec);
    }
}

fn running(workspaces: &[Workspace]) -> Vec<Live> {
    let mut out = Vec::new();
    for ws in workspaces {
        for (dir, rec) in own_runs(ws) {
            if rec.outcome.is_none() {
                out.push(Live { ws: ws.clone(), rec, own: Some(dir) });
            }
        }
        for name in names(ws) {
            if let Some(rec) = read_state(ws, &name).running {
                out.push(Live { ws: ws.clone(), rec, own: None });
            }
        }
    }
    out
}

/// The watcher's scheduler: `tick` about once a minute.
#[derive(Default)]
pub struct Ticker {
    /// Runs being set up on their threads right now (they count against
    /// `MAX_PARALLEL` before their record exists).
    starting: Arc<AtomicUsize>,
    /// Schedule files already reported as broken, so the log says it once.
    reported: HashSet<String>,
}

impl Ticker {
    /// One pass; returns how many runs are going on (for keeping the Mac awake).
    pub fn tick(&mut self) -> usize {
        let tz = tz();
        let now = Timestamp::now();
        let workspaces = workspace::registered_workspaces();
        let sessions = workspace::sessions::sessions();
        let signals = crate::tmux::signals();

        let mut active = 0;
        let mut busy: HashSet<(PathBuf, String)> = HashSet::new();
        for mut run in running(&workspaces) {
            if follow(&mut run, &sessions, &signals, now) {
                active += 1;
                busy.insert((run.ws.dir.clone(), run.rec.schedule.clone()));
            }
        }

        let mut due: Vec<(Timestamp, Workspace, Schedule)> = Vec::new();
        for ws in &workspaces {
            for name in names(ws) {
                let s = match load(ws, &name) {
                    Ok(s) => s,
                    Err(e) => {
                        if self.reported.insert(format!("{}/{name}: {e}", ws.dir.display())) {
                            eprintln!("tenx schedule: {e:#}");
                        }
                        continue;
                    }
                };
                let mut st = read_state(ws, &name);
                if st.created.is_none() {
                    st.created = Some(now);
                    let _ = write_state(ws, &name, &st);
                }
                let previous = busy.contains(&(ws.dir.clone(), name.clone()));
                match rules::due(&s, &st, previous, now, &tz) {
                    Due::Wait(_) | Due::Paused => {}
                    Due::Busy(slot) => skip(ws, &s.name, &mut st, slot, now, "the previous run was still going"),
                    Due::Missed(slot) => skip(ws, &s.name, &mut st, slot, now, "missed while tenx was not running"),
                    Due::Run(slot) => due.push((slot, ws.clone(), s)),
                }
            }
        }

        due.sort_by_key(|(slot, ..)| *slot);
        for (slot, ws, s) in due {
            if active + self.starting.load(Ordering::SeqCst) >= rules::MAX_PARALLEL {
                break; // the rest stay due and wait for a place
            }
            // Recorded before the thread starts, so the next tick can't start
            // the same slot a second time.
            let mut st = read_state(&ws, &s.name);
            st.last_slot = Some(slot);
            if write_state(&ws, &s.name, &st).is_err() {
                continue;
            }
            let starting = self.starting.clone();
            starting.fetch_add(1, Ordering::SeqCst);
            let tz = tz.clone();
            std::thread::spawn(move || {
                if let Err(e) = start(&ws, &s, slot, &tz) {
                    eprintln!("tenx schedule: {}/{}: {e:#}", ws.config.name, s.name);
                }
                starting.fetch_sub(1, Ordering::SeqCst);
            });
        }

        for ws in &workspaces {
            prune(ws);
        }
        active + self.starting.load(Ordering::SeqCst)
    }
}

impl Ticker {
    /// Block until every run this ticker started is set up.
    pub fn wait_for_starts(&self) {
        while self.starting.load(Ordering::SeqCst) > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn skip(ws: &Workspace, name: &str, st: &mut ScheduleState, slot: Timestamp, now: Timestamp, reason: &str) {
    st.last_slot = Some(slot);
    let _ = write_state(ws, name, st);
    append_history(
        ws,
        name,
        &RunRecord {
            schedule: name.to_string(),
            task: String::new(),
            slot,
            started: now,
            interrupted: None,
            ended: Some(now),
            outcome: Some(Outcome::Skipped { reason: reason.into() }),
            nothing_to_report: false,
            read: true,
            pr: None,
        },
    );
}

/// Look at one running run: end it, or step it towards its timeout.
/// Returns whether it is still going.
fn follow(run: &mut Live, sessions: &[workspace::sessions::Session], signals: &workspace::Signals, now: Timestamp) -> bool {
    let Some(dir) = run.task_dir() else {
        run.finish(Outcome::Failed { reason: "its task is gone".into() }, now);
        return false;
    };
    if let Some(own) = &run.own {
        // `tenx schedule quiet` writes the marker while we hold a copy.
        if let Some(fresh) = read_run(own) {
            run.rec.nothing_to_report = fresh.nothing_to_report;
            run.rec.read = fresh.read;
        }
    }
    let state = workspace::resolve_task_state(&dir, sessions, signals);
    if let Some(outcome) = rules::settle(&run.rec, state.status, state.changed.and_then(timestamp), now) {
        run.finish(outcome, now);
        return false;
    }
    let timeout = load(&run.ws, &run.rec.schedule).map(|s| s.timeout).unwrap_or(Some(rules::DEFAULT_TIMEOUT));
    match rules::stop_step(&run.rec, timeout, run.own.is_some(), now) {
        Stop::Keep => true,
        Stop::Interrupt => {
            let window = crate::tmux::find_task_window(&run.rec.task, &dir).ok().flatten();
            let pane = state.pane.or_else(|| window.map(|w| format!("{}.0", w.id)));
            if let Some(pane) = pane {
                let _ = crate::tmux::send_keys(&pane, INTERRUPT_KEY);
            }
            run.rec.interrupted = Some(now);
            run.save();
            true
        }
        Stop::Close => {
            close_window(&run.rec.task, &dir);
            run.finish(Outcome::TimedOut, now);
            false
        }
    }
}

/// Close a run's window the way sweep does: the window, its id cache, and the
/// secrets released to it.
fn close_window(slug: &str, dir: &Path) {
    if let Ok(Some(w)) = crate::tmux::find_task_window(slug, dir)
        && crate::tmux::kill_window(&w.id).is_ok()
    {
        let _ = std::fs::remove_file(dir.join(crate::tmux::WINDOW_ID_FILE));
        let _ = crate::cli::secrets::forget_released(dir);
    }
}

/// Start one run. `Ok(Some(slug))`: the task it runs in; `Ok(None)`: skipped
/// (recorded in the history with the reason).
fn start(ws: &Workspace, s: &Schedule, slot: Timestamp, tz: &TimeZone) -> Result<Option<String>> {
    let last = history(ws, &s.name).into_iter().rev().find(|r| !r.task.is_empty());
    let vars = rules::PromptVars {
        slot,
        last_run: last.as_ref().map(|r| r.started),
        last_run_pr: last.as_ref().and_then(|r| r.pr.as_deref()),
    };
    let prompt = rules::render_prompt(s, &vars, tz);
    let record = |task: &str| RunRecord {
        schedule: s.name.clone(),
        task: task.to_string(),
        slot,
        started: Timestamp::now(),
        interrupted: None,
        ended: None,
        outcome: None,
        nothing_to_report: false,
        read: false,
        pr: None,
    };
    let skipped = |reason: String| {
        let mut st = read_state(ws, &s.name);
        skip(ws, &s.name, &mut st, slot, Timestamp::now(), &reason);
        Ok(None)
    };

    match &s.target {
        Target::New => {
            let title = rules::run_title(&s.title, slot, tz);
            let args = (!s.agent_args.is_empty()).then_some(s.agent_args.as_str());
            let kind = (!s.agent.is_empty()).then(|| crate::agent::AgentKind::from_token(&s.agent));
            let slug = match crate::cli::task::plan_slug(ws, &title) {
                Ok(slug) => slug,
                Err(e) => {
                    let failed = Outcome::Failed { reason: format!("{e:#}") };
                    append_history(ws, &s.name, &RunRecord { outcome: Some(failed), ended: Some(Timestamp::now()), ..record("") });
                    return Err(e);
                }
            };
            // The record is written with the directory, before any clone, so
            // the run never shows on the Tasks list; `started` is set again
            // when the agent starts, so cloning doesn't eat the timeout.
            let run_json = serde_json::to_string_pretty(&record(&slug))?;
            let description = format!("Scheduled run of `{}` ({}).", s.name, s.title);
            let md = crate::cli::task::TaskMd { description: &description, links: &[], prompt: &prompt, run: Some(&run_json) };
            let agent = crate::cli::task::AgentSetup { kind, args, confirm_dev_channels: false };
            let made = crate::cli::task::new_with(ws, &title, s.repos.as_deref(), crate::cli::task::OpenMode::Closed, &md, agent, &crate::progress::Silent);
            let task_dir = ws.tasks_dir().join(&slug);
            if let Err(e) = made {
                if task_dir.join(RUN_FILE).is_file() {
                    let mut live = Live { ws: ws.clone(), rec: record(&slug), own: Some(task_dir) };
                    live.finish(Outcome::Failed { reason: format!("{e:#}") }, Timestamp::now());
                } else {
                    append_history(ws, &s.name, &RunRecord { outcome: Some(Outcome::Failed { reason: format!("{e:#}") }), ended: Some(Timestamp::now()), ..record("") });
                }
                return Err(e);
            }
            write_run(&task_dir, &record(&slug))?;
            if let Err(e) = crate::cli::task::open_window(ws, &slug, true) {
                let mut live = Live { ws: ws.clone(), rec: record(&slug), own: Some(task_dir) };
                live.finish(Outcome::Failed { reason: format!("no window: {e:#}") }, Timestamp::now());
                return Err(e);
            }
            Ok(Some(slug))
        }
        Target::Task(slug) => {
            let task = match ws.find_task(slug) {
                Ok(t) => t,
                Err(_) => return skipped(format!("target task {slug:?} doesn't exist")),
            };
            let missing: Vec<&str> =
                s.secrets.iter().filter(|n| crate::cli::secrets::released_at(&task, n).is_none()).map(String::as_str).collect();
            if !missing.is_empty() {
                return skipped(format!("{} not released in {slug}", missing.join(", ")));
            }
            let rec = record(slug);
            if let Err(e) = crate::cli::drive::send_in(ws, slug, &prompt, false, true) {
                return skipped(format!("{e:#}"));
            }
            let mut st = read_state(ws, &s.name);
            st.running = Some(rec);
            write_state(ws, &s.name, &st)?;
            Ok(Some(slug.clone()))
        }
    }
}

/// Whether a run's task has nothing to lose: no uncommitted changes, no
/// commits that aren't on a remote, no open PR.
fn clean(dir: &Path) -> bool {
    if crate::live::read(dir).prs.iter().any(|p| p.state == "OPEN") {
        return false;
    }
    let task = match workspace::discover_task(dir) {
        Ok(t) => t,
        Err(_) => return false,
    };
    task.repos.iter().all(|repo| {
        let wt = dir.join(repo);
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&wt)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        git(&["status", "--porcelain"]).is_some_and(|s| s.is_empty())
            && git(&["rev-list", "--count", "HEAD", "--not", "--remotes"]).is_some_and(|n| n == "0")
    })
}

/// Delete the old runs of each schedule that `prunable` allows.
fn prune(ws: &Workspace) {
    let runs = own_runs(ws);
    for name in names(ws) {
        let Ok(s) = load(ws, &name) else { continue };
        let mine: Vec<&(PathBuf, RunRecord)> = runs.iter().filter(|(_, r)| r.schedule == name).collect();
        let over = mine.iter().filter(|(_, r)| r.outcome.as_ref().is_some_and(|o| o.ok())).count() > s.keep_ok
            || mine.iter().filter(|(_, r)| r.outcome.as_ref().is_some_and(|o| !o.ok())).count() > s.keep_failed;
        if !over {
            continue; // the git checks below are the expensive part
        }
        let summaries: Vec<rules::RunSummary> = mine
            .iter()
            .map(|(dir, r)| rules::RunSummary {
                started: r.started,
                outcome: r.outcome.clone(),
                clean: r.outcome.is_some() && clean(dir),
                pinned: crate::cli::task::is_pinned(dir),
            })
            .collect();
        for i in rules::prunable(&summaries, s.keep_ok, s.keep_failed) {
            let (dir, rec) = mine[i];
            close_window(&rec.task, dir);
            if let Err(e) = crate::cli::task::rm_in(ws, &rec.task, true) {
                eprintln!("tenx schedule: couldn't delete old run {}: {e:#}", rec.task);
            }
        }
    }
}

/// While runs are going on, keep a Mac from idle sleep (`caffeinate -i`, tied
/// to the watcher's pid so it can't outlive it). Elsewhere a no-op.
#[derive(Default)]
pub struct Awake(Option<std::process::Child>);

impl Awake {
    pub fn set(&mut self, on: bool) {
        if !cfg!(target_os = "macos") {
            return;
        }
        match (on, self.0.as_mut()) {
            (true, None) => {
                self.0 = std::process::Command::new("caffeinate")
                    .args(["-i", "-w", &std::process::id().to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .ok();
            }
            (false, Some(child)) => {
                let _ = child.kill();
                let _ = child.wait();
                self.0 = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, removed when dropped.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let dir = std::env::temp_dir().join(format!("tenx-schedule-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ws_in(dir: &Path) -> Workspace {
        Workspace {
            dir: dir.to_path_buf(),
            config: workspace::WorkspaceConfig { name: "w".into(), ..Default::default() },
        }
    }

    #[test]
    fn files_state_and_history_round_trip() {
        let tmp = Tmp::new("files");
        let ws = ws_in(tmp.path());
        let file = ScheduleFile { cron: Some("0 6 * * *".into()), prompt: "p".into(), title: "T".into(), ..Default::default() };
        write_file(&ws, "nightly", &file).unwrap();
        std::fs::write(dir(&ws).join("notes.txt"), "").unwrap();
        assert_eq!(names(&ws), vec!["nightly"]);
        assert_eq!(load(&ws, "nightly").unwrap().title, "T");

        let st = ScheduleState { created: Some(Timestamp::UNIX_EPOCH), ..Default::default() };
        write_state(&ws, "nightly", &st).unwrap();
        assert_eq!(read_state(&ws, "nightly"), st);
        assert_eq!(names(&ws), vec!["nightly"], "the state dir is no schedule");

        let rec = RunRecord {
            schedule: "nightly".into(),
            task: "t".into(),
            slot: Timestamp::UNIX_EPOCH,
            started: Timestamp::UNIX_EPOCH,
            interrupted: None,
            ended: None,
            outcome: Some(Outcome::Done),
            nothing_to_report: false,
            read: false,
            pr: None,
        };
        append_history(&ws, "nightly", &rec);
        append_history(&ws, "nightly", &rec);
        assert_eq!(history(&ws, "nightly").len(), 2);
    }

    /// The README's example schedule file is one `tenx schedule` reads.
    #[test]
    fn the_readme_example_parses() {
        let readme = include_str!("../../README.md");
        let section = &readme[readme.find("## Scheduled tasks").expect("README section")..];
        let start = section.find("```toml\n").expect("a toml example") + "```toml\n".len();
        let example = &section[start..start + section[start..].find("```").unwrap()];
        let file: ScheduleFile = toml::from_str(example).unwrap();
        let s = rules::parse("nightly-deps", &file).unwrap();
        assert_eq!((s.keep_ok, s.keep_failed, s.timeout), (3, 5, Some(rules::DEFAULT_TIMEOUT)));
    }

    #[test]
    fn run_marker_and_read_flag() {
        let tmp = Tmp::new("marker");
        assert!(!is_run(tmp.path()));
        let mut rec = RunRecord {
            schedule: "s".into(),
            task: "t".into(),
            slot: Timestamp::UNIX_EPOCH,
            started: Timestamp::UNIX_EPOCH,
            interrupted: None,
            ended: None,
            outcome: None,
            nothing_to_report: false,
            read: false,
            pr: None,
        };
        write_run(tmp.path(), &rec).unwrap();
        assert!(is_run(tmp.path()));
        mark_read(tmp.path());
        assert!(!read_run(tmp.path()).unwrap().read, "a running run isn't read yet");
        rec.outcome = Some(Outcome::Done);
        write_run(tmp.path(), &rec).unwrap();
        mark_read(tmp.path());
        assert!(read_run(tmp.path()).unwrap().read);
    }

    #[test]
    fn clean_needs_no_changes_and_nothing_unpushed() {
        let tmp = Tmp::new("clean");
        let task = tmp.path().join("task");
        let origin = tmp.path().join("origin.git");
        let git = |dir: &Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(ok.status.success(), "{args:?}: {}", String::from_utf8_lossy(&ok.stderr));
        };
        std::fs::create_dir_all(&task).unwrap();
        git(tmp.path(), &["init", "-q", "--bare", origin.to_str().unwrap()]);
        git(tmp.path(), &["clone", "-q", origin.to_str().unwrap(), task.join("repo").to_str().unwrap()]);
        let wt = task.join("repo");
        // `discover_task` counts a repo by its `.git` *file* (a worktree);
        // make the clone look like one.
        let gitdir = tmp.path().join("repo-gitdir");
        std::fs::rename(wt.join(".git"), &gitdir).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(wt.join("a"), "1").unwrap();
        assert!(!clean(&task), "uncommitted file");
        git(&wt, &["add", "a"]);
        git(&wt, &["commit", "-qm", "a"]);
        assert!(!clean(&task), "commit not on a remote");
        git(&wt, &["push", "-q", "origin", "HEAD:main"]);
        assert!(clean(&task));
    }
}
