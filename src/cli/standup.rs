use anyhow::{Context, Result};
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DAILY_LOG_FILE: &str = "daily.local.md";

pub fn run(since: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;

    let from_ts = since
        .map(str::to_string)
        .unwrap_or_else(|| read_last_standup(&ws.dir).unwrap_or_else(start_of_yesterday));

    print_task_files(&ws)?;
    print_activity(&ws.dir, &from_ts)?;
    Ok(())
}

// ── Standup log ───────────────────────────────────────────────────────────────

fn daily_log_path(ws_dir: &Path) -> PathBuf {
    ws_dir.join(DAILY_LOG_FILE)
}

/// Read the timestamp from the first `## YYYY-MM-DD HH:MM` heading in daily.local.md.
fn read_last_standup(ws_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(daily_log_path(ws_dir)).ok()?;
    for line in content.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            return heading_to_iso(heading.trim());
        }
    }
    None
}

/// Parse `YYYY-MM-DD HH:MM` → `YYYY-MM-DDTHH:MM:00Z`
fn heading_to_iso(s: &str) -> Option<String> {
    let (date, time) = s.split_once(' ')?;
    // Validate rough shape
    if date.len() == 10 && time.len() == 5 {
        Some(format!("{date}T{time}:00Z"))
    } else {
        None
    }
}

// ── Time helpers ──────────────────────────────────────────────────────────────

fn start_of_yesterday() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let midnight = (secs.saturating_sub(86400) / 86400) * 86400;
    let date = epoch_to_date(midnight);
    format!("{date}T00:00:00Z")
}

fn epoch_to_date(secs: u64) -> String {
    let days = secs / 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ── Task files ────────────────────────────────────────────────────────────────

fn print_task_files(ws: &crate::workspace::Workspace) -> Result<()> {
    println!("=== TASK FILES ===");
    let tasks_dir = ws.tasks_dir();
    if !tasks_dir.exists() {
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(&tasks_dir)
        .context("read tasks dir")?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let task_md = entry.path().join("TASK.md");
        if !task_md.exists() {
            continue;
        }
        let task_name = entry.file_name().to_string_lossy().into_owned();
        let content = fs::read_to_string(&task_md)
            .with_context(|| format!("read {}", task_md.display()))?;
        println!("\n### TASK: {task_name}");
        print!("{content}");
    }
    Ok(())
}

// ── Activity log ──────────────────────────────────────────────────────────────
//
// One section per agent. Each reads a transcript through the shared parser
// (`tenx_core::transcript`) and reports the user prompts and git commands since
// `from_ts` — so a standup covers Codex and pi work, not only Claude Code.

fn print_activity(ws_dir: &Path, from_ts: &str) -> Result<()> {
    println!("\n=== ACTIVITY LOG (since {from_ts}) ===");
    claude_activity(ws_dir, from_ts)?;
    codex_activity(ws_dir, from_ts);
    pi_activity(ws_dir, from_ts);
    Ok(())
}

fn claude_activity(ws_dir: &Path, from_ts: &str) -> Result<()> {
    let projects_dir = claude_projects_dir()?;
    let slug_prefix = path_to_slug(ws_dir);
    let mut project_dirs: Vec<_> = match fs::read_dir(&projects_dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.contains(&slug_prefix) && e.file_type().map(|t| t.is_dir()).unwrap_or(false)
            })
            .collect(),
        Err(_) => return Ok(()), // no ~/.claude/projects — nothing to report
    };
    project_dirs.sort_by_key(|e| e.file_name());
    for project_dir in project_dirs {
        let label = project_dir
            .file_name()
            .to_string_lossy()
            .replace(&slug_prefix, "")
            .replace('-', "/")
            .trim_matches('/')
            .to_string();
        let label = if label.is_empty() { "root".to_string() } else { label };
        let mut jsonl: Vec<PathBuf> = fs::read_dir(project_dir.path())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        jsonl.sort();
        for path in jsonl {
            emit_entries(&path, "claude", from_ts, &label);
        }
    }
    Ok(())
}

fn codex_activity(ws_dir: &Path, from_ts: &str) {
    let Some(root) = home_dir().map(|h| h.join(".codex/sessions")) else { return };
    let mut files: Vec<PathBuf> = jsonl_files_recursive(&root)
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-")))
        .collect();
    files.sort();
    for path in files {
        let Some(cwd) = first_line(&path).and_then(|l| tenx_core::codex::session_meta_cwd(&l)) else { continue };
        if let Some(label) = rel_label(&cwd, ws_dir) {
            emit_entries(&path, "codex", from_ts, &label);
        }
    }
}

fn pi_activity(ws_dir: &Path, from_ts: &str) {
    let Some(root) = home_dir().map(|h| h.join(".pi/agent/sessions")) else { return };
    let Ok(dirs) = fs::read_dir(&root) else { return };
    let mut files: Vec<PathBuf> = Vec::new();
    for d in dirs.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)) {
        for e in fs::read_dir(d.path()).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "jsonl") {
                files.push(p);
            }
        }
    }
    files.sort();
    for path in files {
        let Some(cwd) = first_line(&path).and_then(|l| pi_header_cwd(&l)) else { continue };
        if let Some(label) = rel_label(&cwd, ws_dir) {
            emit_entries(&path, "pi", from_ts, &label);
        }
    }
}

/// Parse one transcript file for one session's activity and print it, if any.
fn emit_entries(path: &Path, agent: &str, from_ts: &str, label: &str) {
    let Ok(file) = fs::File::open(path) else { return };
    let reader = BufReader::new(file);
    let mut title: Option<String> = None;
    let mut events: Vec<String> = Vec::new();
    for line in reader.lines().map_while(|l| l.ok()) {
        if line.is_empty() {
            continue;
        }
        let Some(e) = tenx_core::transcript::parse_line(agent, &line) else { continue };
        if let Some(t) = e.title {
            title = Some(t);
            continue;
        }
        if e.iso.as_deref().is_some_and(|iso| iso < from_ts) {
            continue;
        }
        let time = &e.hm;
        match e.role {
            tenx_core::transcript::Role::User if e.text.len() > 10 => {
                events.push(format!("[{time}] USER: {}", truncate(&e.text, 200)));
            }
            tenx_core::transcript::Role::Assistant => {
                for cmd in e.commands() {
                    if cmd.contains("git commit") || cmd.contains("git push") {
                        events.push(format!("[{time}] GIT: {}", truncate(cmd, 200)));
                    }
                }
            }
            _ => {}
        }
    }
    if events.is_empty() {
        return;
    }
    let tag = if agent == "claude" { String::new() } else { format!(" [{agent}]") };
    match &title {
        Some(t) => println!("\n--- {label}{tag} | {t} ---"),
        None => println!("\n--- {label}{tag} ---"),
    }
    for e in events {
        println!("{e}");
    }
}

/// A session's cwd made relative to the workspace, for a section label; `None`
/// when the cwd isn't inside this workspace (so other workspaces are skipped).
fn rel_label(cwd: &str, ws_dir: &Path) -> Option<String> {
    let rel = Path::new(cwd).strip_prefix(ws_dir).ok()?;
    let s = rel.to_string_lossy();
    Some(if s.is_empty() { "root".to_string() } else { s.into_owned() })
}

fn first_line(path: &Path) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line).ok()?;
    (!line.is_empty()).then_some(line)
}

/// The `cwd` in a pi session file's header (`{"type":"session","cwd":…}`).
fn pi_header_cwd(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v["type"].as_str() != Some("session") {
        return None;
    }
    v["cwd"].as_str().map(str::to_string)
}

fn jsonl_files_recursive(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn claude_projects_dir() -> Result<PathBuf> {
    let home = env::var("HOME").context("$HOME not set")?;
    Ok(PathBuf::from(home).join(".claude").join("projects"))
}

fn path_to_slug(path: &Path) -> String {
    let home = env::var("HOME").unwrap_or_default();
    path.to_string_lossy().replace(&home, "").replace('/', "-")
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut idx = max;
        while !s.is_char_boundary(idx) {
            idx -= 1;
        }
        &s[..idx]
    }
}
