//! `tenx internal agent-log <cwd> <pid>`: a live, compact view of a background
//! agent's transcript, for the pane `tenx watch` opens when a `--bg` session
//! appears under a task. The agent is another process with no terminal of its
//! own, so its pane can't *be* it — but Claude Code writes every turn to
//! `~/.claude/projects/<encoded cwd>/<session>.jsonl`, and following that is
//! the next best thing: what it was asked, what it said, which tools it ran.
//! Exits when the agent's pid is gone, so the pane closes with the agent.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(400);
/// How much history to show on open — enough to see what the agent is doing,
/// not the whole conversation.
const TAIL_LINES: usize = 40;

pub fn run(cwd: &str, pid: u32) -> Result<()> {
    let project = crate::workspace::claude::project_dir(Path::new(cwd)).context("no $HOME")?;
    let mut out = std::io::stdout();
    let name = Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    writeln!(out, "\x1b[1magent · {name}\x1b[0m  \x1b[2m(pid {pid}; this pane closes when it exits)\x1b[0m")?;

    let mut file: Option<(PathBuf, BufReader<std::fs::File>)> = None;
    let mut last_scan = Instant::now() - Duration::from_secs(60);
    let mut printed_history = false;

    loop {
        // (Re)find the newest transcript every few seconds: a session that
        // starts after we do, or one that rolls over, shows up here.
        if last_scan.elapsed() > Duration::from_secs(3) {
            last_scan = Instant::now();
            if let Some(newest) = newest_jsonl(&project)
                && file.as_ref().is_none_or(|(p, _)| *p != newest)
            {
                let f = std::fs::File::open(&newest).with_context(|| format!("open {}", newest.display()))?;
                let mut reader = BufReader::new(f);
                if !printed_history {
                    // Render the tail of the existing transcript, then follow.
                    let mut lines: Vec<String> = Vec::new();
                    let mut buf = String::new();
                    while reader.read_line(&mut buf)? > 0 {
                        if let Some(l) = render_line(&buf) {
                            lines.push(l);
                        }
                        buf.clear();
                    }
                    let skip = lines.len().saturating_sub(TAIL_LINES);
                    for l in &lines[skip..] {
                        writeln!(out, "{l}")?;
                    }
                    printed_history = true;
                } else {
                    reader.seek(SeekFrom::End(0))?;
                }
                file = Some((newest, reader));
            }
        }

        let mut got = false;
        if let Some((_, reader)) = file.as_mut() {
            let mut buf = String::new();
            while reader.read_line(&mut buf)? > 0 {
                if buf.ends_with('\n') {
                    if let Some(l) = render_line(&buf) {
                        writeln!(out, "{l}")?;
                        got = true;
                    }
                    buf.clear();
                } else {
                    // A partial line (writer mid-flush): rewind and retry.
                    let back = buf.len() as i64;
                    reader.seek(SeekFrom::Current(-back))?;
                    break;
                }
            }
        }
        out.flush()?;

        if !crate::workspace::claude::pid_alive(pid) {
            writeln!(out, "\x1b[2m— agent exited —\x1b[0m")?;
            out.flush()?;
            std::thread::sleep(Duration::from_secs(2));
            return Ok(());
        }
        if !got {
            std::thread::sleep(POLL);
        }
    }
}

fn newest_jsonl(project: &Path) -> Option<PathBuf> {
    std::fs::read_dir(project)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// One transcript line → one display line, or nothing for records that
/// aren't conversation (progress, summaries, tool results).
fn render_line(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let time = v["timestamp"].as_str().and_then(|t| t.get(11..16)).unwrap_or("     ");
    match v["type"].as_str()? {
        "user" => {
            let text = match &v["message"]["content"] {
                Value::String(s) => s.clone(),
                Value::Array(blocks) => blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => return None,
            };
            let text = text.trim();
            (!text.is_empty()).then(|| format!("\x1b[2m{time}\x1b[0m \x1b[35m›\x1b[0m {}", one_line(text, 160)))
        }
        "assistant" => {
            let blocks = v["message"]["content"].as_array()?;
            let mut parts = Vec::new();
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                            parts.push(one_line(t, 160));
                        }
                    }
                    Some("tool_use") => {
                        let name = b["name"].as_str().unwrap_or("tool");
                        let arg = b["input"]["command"]
                            .as_str()
                            .or_else(|| b["input"]["file_path"].as_str())
                            .or_else(|| b["input"]["pattern"].as_str())
                            .or_else(|| b["input"]["description"].as_str())
                            .unwrap_or("");
                        parts.push(format!("\x1b[2m⚙ {name} {}\x1b[0m", one_line(arg, 100)));
                    }
                    _ => {}
                }
            }
            (!parts.is_empty()).then(|| format!("\x1b[2m{time}\x1b[0m {}", parts.join("  ")))
        }
        _ => None,
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_user_and_assistant_lines() {
        let user = r#"{"type":"user","timestamp":"2026-09-03T10:15:00Z","message":{"content":"fix the   tests\nplease"}}"#;
        let l = render_line(user).unwrap();
        assert!(l.contains("10:15") && l.contains("fix the tests please"));

        let asst = r#"{"type":"assistant","timestamp":"2026-09-03T10:16:00Z","message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        let l = render_line(asst).unwrap();
        assert!(l.contains("On it.") && l.contains("⚙ Bash cargo test"));
    }

    #[test]
    fn skips_noise() {
        assert!(render_line(r#"{"type":"progress","timestamp":"x"}"#).is_none());
        assert!(render_line("not json").is_none());
        assert!(render_line(r#"{"type":"user","message":{"content":"   "}}"#).is_none());
    }

    #[test]
    fn truncates_long_text() {
        let t = one_line(&"a ".repeat(200), 20);
        assert_eq!(t.chars().count(), 20);
        assert!(t.ends_with('…'));
    }
}
