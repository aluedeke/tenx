//! `tenx internal agent-log <cwd> <pid> [--session <id>] [--agent <kind>]`: a
//! live, compact view of a background agent's transcript, for the pane
//! `tenx watch` opens when a `--bg` session appears under a task. The agent is
//! another process with no terminal of its own, so its pane can't *be* it — but
//! every agent writes its turns to a JSONL transcript, and following that is the
//! next best thing: what it was asked, what it said, which tools it ran.
//! Exits when the agent's pid is gone, so the pane closes with the agent.
//!
//! Transcript location differs by agent (`transcript_path`); the line format
//! differs too, and both are handled by `tenx_core::transcript`.

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(400);
/// How much history to show on open — enough to see what the agent is doing,
/// not the whole conversation.
const TAIL_LINES: usize = 40;

/// How far back from the end of an existing transcript to start rendering
/// history. Transcripts reach tens of MB; the last few hundred KB is plenty for
/// `TAIL_LINES`, and parsing from byte 0 would stall the pane on open.
const HISTORY_BYTES: u64 = 512 * 1024;

pub fn run(cwd: &str, pid: u32, session: Option<&str>, agent: &str) -> Result<()> {
    let mut out = std::io::stdout();
    let name = Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    writeln!(out, "\x1b[1magent · {name}\x1b[0m  \x1b[2m({agent}; pid {pid}; this pane closes when it exits)\x1b[0m")?;

    let mut file: Option<(PathBuf, BufReader<std::fs::File>)> = None;
    // `None` = never scanned; not `Instant::now() - 60s`, which can underflow
    // (and panic) early after boot.
    let mut last_scan: Option<Instant> = None;
    let mut printed_history = false;

    loop {
        // (Re)find the transcript every few seconds: a session that starts after
        // we do shows up here.
        if last_scan.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            last_scan = Some(Instant::now());
            if let Some(newest) = locate_transcript(agent, cwd, session)
                && file.as_ref().is_none_or(|(p, _)| *p != newest)
            {
                let f = std::fs::File::open(&newest).with_context(|| format!("open {}", newest.display()))?;
                let mut reader = BufReader::new(f);
                if !printed_history {
                    let len = reader.get_ref().metadata().map(|m| m.len()).unwrap_or(0);
                    let mut buf = String::new();
                    if len > HISTORY_BYTES {
                        reader.seek(SeekFrom::Start(len - HISTORY_BYTES))?;
                        reader.read_line(&mut buf)?; // discard the partial line
                        buf.clear();
                    }
                    let mut lines: Vec<String> = Vec::new();
                    while reader.read_line(&mut buf)? > 0 {
                        if let Some(l) = render_line(agent, &buf) {
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
                    if let Some(l) = render_line(agent, &buf) {
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

        if !crate::workspace::sessions::pid_alive(pid) {
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

/// Find the agent's transcript file for `cwd`. Each harness stores it
/// differently; within a store, prefer the file matching the session id, else
/// the newest.
fn locate_transcript(agent: &str, cwd: &str, session: Option<&str>) -> Option<PathBuf> {
    match agent {
        "codex" => {
            // ~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl — the id is in
            // the filename, so match on it; else the newest rollout for this cwd.
            let root = home()?.join(".codex/sessions");
            let files = jsonl_files_recursive(&root);
            if let Some(id) = session {
                if let Some(p) = files.iter().find(|p| filename_contains(p, id)) {
                    return Some(p.clone());
                }
            }
            newest_by_mtime(files.into_iter().filter(|p| codex_rollout_cwd_matches(p, cwd)).collect())
        }
        "pi" => {
            // ~/.pi/agent/sessions/--<cwd>--/<ts>_<id>.jsonl
            let dir = home()?.join(".pi/agent/sessions").join(tenx_core::transcript::pi_session_dirname(cwd));
            let files: Vec<PathBuf> = std::fs::read_dir(&dir)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            if let Some(id) = session {
                if let Some(p) = files.iter().find(|p| filename_contains(p, id)) {
                    return Some(p.clone());
                }
            }
            newest_by_mtime(files)
        }
        _ => {
            // Claude: ~/.claude/projects/<encoded cwd>/<session>.jsonl
            let project = crate::workspace::sessions::project_dir(Path::new(cwd))?;
            if let Some(id) = session {
                let p = project.join(format!("{id}.jsonl"));
                if p.is_file() {
                    return Some(p);
                }
            }
            let files: Vec<PathBuf> = std::fs::read_dir(&project)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            newest_by_mtime(files)
        }
    }
}

fn render_line(agent: &str, line: &str) -> Option<String> {
    let e = tenx_core::transcript::parse_line(agent, line.trim())?;
    use tenx_core::transcript::Role;
    let time = if e.hm.is_empty() { "     ".to_string() } else { e.hm.clone() };
    match e.role {
        Role::User if !e.text.is_empty() => {
            Some(format!("\x1b[2m{time}\x1b[0m \x1b[35m›\x1b[0m {}", one_line(&e.text, 160)))
        }
        Role::Assistant => {
            let mut parts = Vec::new();
            if !e.text.is_empty() {
                parts.push(one_line(&e.text, 160));
            }
            for t in &e.tools {
                parts.push(format!("\x1b[2m⚙ {} {}\x1b[0m", t.name, one_line(&t.arg, 100)));
            }
            (!parts.is_empty()).then(|| format!("\x1b[2m{time}\x1b[0m {}", parts.join("  ")))
        }
        _ => None,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn filename_contains(p: &Path, needle: &str) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(needle))
}

fn newest_by_mtime(files: Vec<PathBuf>) -> Option<PathBuf> {
    files.into_iter().max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

fn jsonl_files_recursive(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
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

/// Whether a Codex rollout's `session_meta` cwd equals `cwd` (line 1).
fn codex_rollout_cwd_matches(path: &Path, cwd: &str) -> bool {
    let Ok(f) = std::fs::File::open(path) else { return false };
    let mut first = String::new();
    if BufReader::new(f).read_line(&mut first).is_err() {
        return false;
    }
    tenx_core::codex::session_meta_cwd(&first).as_deref() == Some(cwd)
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
    fn renders_each_agent_format() {
        let claude = r#"{"type":"assistant","timestamp":"2026-09-03T10:16:00Z","message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        let l = render_line("claude", claude).unwrap();
        assert!(l.contains("10:16") && l.contains("On it.") && l.contains("⚙ Bash cargo test"));

        let codex = r#"{"timestamp":"2026-09-04T15:33:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls -la\"}"}}"#;
        assert!(render_line("codex", codex).unwrap().contains("⚙ exec_command ls -la"));

        let pi = r#"{"type":"message","message":{"role":"user","timestamp":1788537339069,"content":[{"type":"text","text":"run the build"}]}}"#;
        assert!(render_line("pi", pi).unwrap().contains("run the build"));
    }

    #[test]
    fn skips_noise() {
        assert!(render_line("codex", r#"{"type":"event_msg","payload":{"type":"task_started"}}"#).is_none());
        assert!(render_line("claude", "not json").is_none());
    }

    #[test]
    fn truncates_long_text() {
        let t = one_line(&"a ".repeat(200), 20);
        assert_eq!(t.chars().count(), 20);
        assert!(t.ends_with('…'));
    }
}
