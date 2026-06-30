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

fn print_activity(ws_dir: &Path, from_ts: &str) -> Result<()> {
    println!("\n=== ACTIVITY LOG (since {from_ts}) ===");

    let projects_dir = claude_projects_dir()?;
    let slug_prefix = path_to_slug(ws_dir);

    let mut project_dirs: Vec<_> = fs::read_dir(&projects_dir)
        .context("read ~/.claude/projects")?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.contains(&slug_prefix) && e.file_type().map(|t| t.is_dir()).unwrap_or(false)
        })
        .collect();
    project_dirs.sort_by_key(|e| e.file_name());

    for project_dir in project_dirs {
        let mut jsonl_files: Vec<PathBuf> = fs::read_dir(project_dir.path())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
            .map(|e| e.path())
            .collect();
        jsonl_files.sort();

        for jsonl_path in jsonl_files {
            emit_session_activity(
                &jsonl_path,
                from_ts,
                &project_dir.file_name().to_string_lossy(),
                &slug_prefix,
            )?;
        }
    }
    Ok(())
}

fn emit_session_activity(path: &Path, from_ts: &str, slug: &str, slug_prefix: &str) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut session_title: Option<String> = None;
    let mut events: Vec<String> = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        let ts = obj["timestamp"].as_str().unwrap_or("");
        if ts < from_ts {
            continue;
        }
        let time = ts.get(11..16).unwrap_or("");

        match obj["type"].as_str() {
            Some("ai-title") => {
                session_title = obj["aiTitle"].as_str().map(str::to_string);
            }
            Some("user") => {
                if let Some(content) = obj["message"]["content"].as_str() {
                    let trimmed = content.trim();
                    if trimmed.len() > 10 {
                        events.push(format!("[{time}] USER: {}", truncate(trimmed, 200)));
                    }
                }
            }
            Some("assistant") => {
                if let Some(blocks) = obj["message"]["content"].as_array() {
                    for block in blocks {
                        if block["type"] == "tool_use" && block["name"] == "Bash" {
                            let cmd = block["input"]["command"].as_str().unwrap_or("");
                            if cmd.contains("git commit") || cmd.contains("git push") {
                                events.push(format!("[{time}] GIT: {}", truncate(cmd, 200)));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if events.is_empty() {
        return Ok(());
    }

    let label = slug
        .replace(slug_prefix, "")
        .replace('-', "/")
        .trim_matches('/')
        .to_string();
    let label = if label.is_empty() { "root".to_string() } else { label };

    match &session_title {
        Some(t) => println!("\n--- {label} | {t} ---"),
        None => println!("\n--- {label} ---"),
    }
    for e in events {
        println!("{e}");
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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
