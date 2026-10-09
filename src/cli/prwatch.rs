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

use tenx_core::prwatch::{self, Outcome};

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
