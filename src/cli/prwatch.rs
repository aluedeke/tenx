//! `tenx pr wait`: block until a pull request has news — a review or comment
//! from a person or a bot, or a check that failed — then print it and exit.
//! An agent runs it in the background between turns (the `/pr-watch` skill),
//! so waiting for feedback costs no tokens and its session wakes when the
//! command ends. What counts as news is `tenx_core::prwatch`; this file only
//! asks `gh` and sleeps.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tenx_core::prwatch::{self, Outcome, WaitRecord};

/// `tenx pr wait`. `pr` is a URL or number; without it, the PR of the
/// current directory's branch, or of the one repo in the current task that
/// has a PR.
pub fn wait(pr: Option<&str>, since: Option<&str>, interval: Duration, timeout: Duration) -> Result<()> {
    if !crate::live::gh_available() {
        bail!("`gh` is not on PATH — install the GitHub CLI and run `gh auth login`");
    }
    if let Some(s) = since
        && !prwatch::valid_cursor(s)
    {
        bail!("--since takes a GitHub timestamp like 2026-10-09T11:30:00Z (the one the last wait printed)");
    }
    let url = resolve(pr)?;
    let (repo, number) = prwatch::parse_pr_url(&url).with_context(|| format!("not a pull request URL: {url}"))?;
    let saved_at = cursor_file(&repo, number);
    let saved = saved_at.as_deref().and_then(|p| std::fs::read_to_string(p).ok());
    let (since, save) = prwatch::start_cursor(since, saved.as_deref().map(str::trim));
    let since = since.as_deref();
    if save
        && let (Some(path), Some(at)) = (&saved_at, since)
    {
        // Best-effort: without it a restart only repeats news, never loses it.
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(path)).and_then(|_| std::fs::write(path, at));
    }

    // Seen by the column, `tenx pr list` and sweep for as long as it runs.
    let _registered = register(&url, number, since);

    let start = Instant::now();
    let mut first = true;
    loop {
        match fetch(&url, &repo, number) {
            Ok((view, lines)) => {
                let poll = prwatch::poll(&view, &lines, since).with_context(|| format!("no pull request at {url}"))?;
                if let Some(outcome) = prwatch::outcome(&poll) {
                    print!("{}", prwatch::render(&poll));
                    return finish(outcome, poll.number);
                }
                if start.elapsed() >= timeout {
                    print!("{}", prwatch::render(&poll));
                    return finish(Outcome::TimedOut, poll.number);
                }
            }
            // A first failure is a wrong PR or a missing login: say so. Later
            // ones are the network: keep waiting.
            Err(e) if first => return Err(e),
            Err(e) => eprintln!("tenx: {e:#} — retrying"),
        }
        first = false;
        std::thread::sleep(interval);
    }
}

fn finish(outcome: Outcome, number: u64) -> Result<()> {
    let message = match outcome {
        Outcome::News => return Ok(()),
        Outcome::Merged => format!("PR #{number} was merged"),
        Outcome::Closed => format!("PR #{number} was closed without merging"),
        Outcome::TimedOut => format!("no news on PR #{number} yet — run the `next:` command to keep waiting"),
    };
    Err(crate::cli::secrets::Exit { code: outcome.exit_code(), message }.into())
}

/// Where the point a PR's agent has handled up to is kept between waits:
/// `~/.config/tenx/pr-cursors/<owner>-<repo>-<n>`. Outside the task, since
/// it belongs to the PR, not to a task directory.
fn cursor_file(repo: &str, number: u64) -> Option<std::path::PathBuf> {
    let name = format!("{}-{number}", repo.replace('/', "-"));
    Some(crate::workspace::home_dir().ok()?.join(".config").join("tenx").join("pr-cursors").join(name))
}

/// The PR document and its inline review comments, one JSON object each.
fn fetch(url: &str, repo: &str, number: u64) -> Result<(Value, Vec<Value>)> {
    let view = gh(&["pr", "view", url, "--json", "number,url,state,reviews,comments,statusCheckRollup"], None)?;
    let view: Value = serde_json::from_str(&view).context("parse `gh pr view` output")?;
    // `--jq '.[]'` prints one comment per line, across every page.
    let path = format!("repos/{repo}/pulls/{number}/comments");
    let lines = gh(&["api", &path, "--paginate", "--jq", ".[]"], None)?;
    let lines = lines.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    Ok((view, lines))
}

fn gh(args: &[&str], dir: Option<&Path>) -> Result<String> {
    let mut cmd = Command::new("gh");
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let out = cmd.args(args).stdin(Stdio::null()).output().context("run gh")?;
    if !out.status.success() {
        bail!("gh {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The PR to watch, as a URL.
fn resolve(pr: Option<&str>) -> Result<String> {
    if let Some(pr) = pr {
        if prwatch::parse_pr_url(pr).is_some() {
            return Ok(pr.to_string());
        }
        // A number or branch: `gh` resolves it against the current repo.
        return pr_url(&[pr], None).with_context(|| format!("no pull request '{pr}' here"));
    }
    let cwd = std::env::current_dir()?;
    if let Ok(url) = pr_url(&[], Some(&cwd)) {
        return Ok(url);
    }
    // In a task directory (not inside one of its worktrees): the repos'
    // branches, when exactly one of them has a PR.
    if let Ok(ws) = crate::workspace::find(&cwd)
        && let Ok(cwd) = cwd.canonicalize()
        && let Ok(rel) = cwd.strip_prefix(ws.tasks_dir().canonicalize()?)
        && let Some(slug) = rel.iter().next()
    {
        let task = ws.find_task(&slug.to_string_lossy())?;
        let found: Vec<String> = task.repos.iter().filter_map(|r| pr_url(&[], Some(&task.path.join(r))).ok()).collect();
        match found.as_slice() {
            [one] => return Ok(one.clone()),
            [] => {}
            many => bail!("this task has several PRs — pass one: {}", many.join(", ")),
        }
    }
    bail!("no pull request for this branch — pass its URL or number")
}

fn pr_url(args: &[&str], dir: Option<&Path>) -> Result<String> {
    let mut all = vec!["pr", "view"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--json", "url", "--jq", ".url"]);
    let url = gh(&all, dir)?.trim().to_string();
    if url.is_empty() {
        bail!("no pull request");
    }
    Ok(url)
}

/// `~/.config/tenx/pr-waits/`: one `<pid>.json` per running wait.
fn waits_dir() -> Option<std::path::PathBuf> {
    Some(crate::workspace::home_dir().ok()?.join(".config").join("tenx").join("pr-waits"))
}

/// A wait's registration; removing the file when it drops covers every
/// ordinary exit. A wait killed by a signal leaves it behind for [`waits`]
/// to prune.
struct Registered(std::path::PathBuf);

impl Drop for Registered {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn register(url: &str, number: u64, since: Option<&str>) -> Option<Registered> {
    let dir = waits_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let cwd = std::env::current_dir().ok()?;
    let rec = WaitRecord {
        pid: std::process::id(),
        url: url.to_string(),
        number,
        cwd: cwd.canonicalize().unwrap_or(cwd),
        since: since.map(str::to_string),
        started: crate::live::now_secs(),
    };
    let path = dir.join(format!("{}.json", rec.pid));
    std::fs::write(&path, serde_json::to_vec(&rec).ok()?).ok()?;
    Some(Registered(path))
}

/// Every wait running now. A record whose process is gone — or is no longer
/// a `tenx pr wait`, its pid reused — is deleted on the way.
pub fn waits() -> Vec<WaitRecord> {
    let Some(entries) = waits_dir().and_then(|d| std::fs::read_dir(d).ok()) else { return Vec::new() };
    let mut out = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        let rec: Option<WaitRecord> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        match rec {
            Some(rec) if is_wait(rec.pid) => out.push(rec),
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out.sort_by_key(|r| r.started);
    out
}

fn is_wait(pid: u32) -> bool {
    crate::workspace::sessions::pid_alive(pid)
        && crate::live::run_capture("ps", &["-o", "command=", "-p", &pid.to_string()]).contains(" pr wait")
}

/// The PR numbers waited on from inside `task_dir`.
pub fn watched_in(records: &[WaitRecord], task_dir: &Path) -> Vec<u64> {
    if records.is_empty() {
        return Vec::new();
    }
    let dir = task_dir.canonicalize().unwrap_or_else(|_| task_dir.to_path_buf());
    prwatch::watched_in(records, &dir)
}

/// `tenx pr list`: every running wait, with the task it runs in.
pub fn list(json: bool) -> Result<()> {
    let waits = waits();
    let tasks: Vec<(String, std::path::PathBuf)> = crate::workspace::registered_workspaces()
        .iter()
        .flat_map(|ws| {
            let name = ws.config.name.clone();
            ws.tasks().unwrap_or_default().into_iter().map(move |t| {
                let path = t.path.canonicalize().unwrap_or(t.path);
                (format!("{name}/{}", t.name), path)
            })
        })
        .collect();
    let task_of = |w: &WaitRecord| tasks.iter().find(|(_, p)| w.cwd.starts_with(p)).map(|(n, _)| n.clone());
    if json {
        let rows: Vec<Value> = waits
            .iter()
            .map(|w| {
                serde_json::json!({
                    "pid": w.pid, "url": w.url, "number": w.number, "task": task_of(w),
                    "cwd": w.cwd, "since": w.since, "started": w.started,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if waits.is_empty() {
        println!("no PR waits running");
        return Ok(());
    }
    let now = crate::live::now_secs();
    for w in &waits {
        let age = tenx_core::time::format_duration(Duration::from_secs(now.saturating_sub(w.started)));
        let task = task_of(w).unwrap_or_else(|| w.cwd.display().to_string());
        println!("{}  {task}  waiting {age}  pid {}", w.url, w.pid);
    }
    Ok(())
}
