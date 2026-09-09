//! One transcript line → one normalized [`Entry`], for every agent's format.
//!
//! Each harness writes its conversation to a JSONL file in its own shape —
//! Claude Code under `~/.claude/projects`, Codex as a rollout under
//! `~/.codex/sessions`, pi under `~/.pi/agent/sessions`. The agent-log pane and
//! `tenx standup` both need the same few facts out of a line (who spoke, when,
//! what they said, which shell commands ran), so the per-format parsing lives
//! here as pure functions over a line of text, and the two consumers render the
//! `Entry` however they like.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// A session title/name record (Claude's `ai-title`, pi's `session_info`).
    Title,
    /// Anything not worth showing (tool results, meta records).
    Other,
}

/// A tool the assistant invoked. `arg` is a short display string; `command` is
/// the shell command line when this tool ran one (used to spot `git commit`/
/// `git push` in standup), else `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub arg: String,
    pub command: Option<String>,
}

/// A normalized transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `HH:MM` for display, or empty when unknown.
    pub hm: String,
    /// A lexically comparable ISO-8601 (UTC) timestamp for time filtering, when
    /// the format provides one.
    pub iso: Option<String>,
    pub role: Role,
    /// The message text (user prompt or assistant prose), possibly empty.
    pub text: String,
    /// Tools the assistant ran on this line.
    pub tools: Vec<Tool>,
    /// The title, when `role` is `Title`.
    pub title: Option<String>,
}

impl Entry {
    /// Shell command lines this entry ran — for standup's git detection.
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().filter_map(|t| t.command.as_deref())
    }
}

/// Parse one JSONL line of `agent`'s transcript. Returns `None` for a line that
/// carries nothing renderable (blank, non-JSON, or a pure meta record).
pub fn parse_line(agent: &str, line: &str) -> Option<Entry> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    match agent {
        "codex" => parse_codex(&v),
        "pi" => parse_pi(&v),
        _ => parse_claude(&v),
    }
}

/// pi encodes a session directory as `--<cwd>--`, the cwd with its leading
/// separator dropped and every `/`, `\` and `:` turned into `-` (mirrors pi's
/// own `getDefaultSessionDir`).
pub fn pi_session_dirname(cwd: &str) -> String {
    let stripped = cwd.trim_start_matches(['/', '\\']);
    let mapped: String = stripped.chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect();
    format!("--{mapped}--")
}

// ── Claude Code ───────────────────────────────────────────────────────────────

fn parse_claude(v: &Value) -> Option<Entry> {
    let iso = v["timestamp"].as_str().map(str::to_string);
    let hm = hm_from_iso(iso.as_deref());
    match v["type"].as_str()? {
        "ai-title" => v["aiTitle"].as_str().map(|t| title_entry(t, hm, iso)),
        "user" => {
            let text = match &v["message"]["content"] {
                Value::String(s) => s.clone(),
                Value::Array(blocks) => join_text(blocks, &["text"]),
                _ => String::new(),
            };
            Some(Entry { hm, iso, role: Role::User, text: text.trim().to_string(), tools: vec![], title: None })
        }
        "assistant" => {
            let blocks = v["message"]["content"].as_array()?;
            let mut text = String::new();
            let mut tools = Vec::new();
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                            if !text.is_empty() {
                                text.push(' ');
                            }
                            text.push_str(t);
                        }
                    }
                    Some("tool_use") => {
                        let name = b["name"].as_str().unwrap_or("tool").to_string();
                        let command = b["input"]["command"].as_str().map(str::to_string);
                        let arg = command
                            .clone()
                            .or_else(|| b["input"]["file_path"].as_str().map(str::to_string))
                            .or_else(|| b["input"]["pattern"].as_str().map(str::to_string))
                            .or_else(|| b["input"]["description"].as_str().map(str::to_string))
                            .unwrap_or_default();
                        tools.push(Tool { name, arg, command });
                    }
                    _ => {}
                }
            }
            Some(Entry { hm, iso, role: Role::Assistant, text, tools, title: None })
        }
        _ => None,
    }
}

// ── Codex CLI (rollout) ───────────────────────────────────────────────────────

/// Tool names in Codex's rollout that run a shell command.
const CODEX_SHELL_TOOLS: &[&str] = &["exec_command", "shell", "local_shell_call", "bash"];

fn parse_codex(v: &Value) -> Option<Entry> {
    if v["type"].as_str()? != "response_item" {
        return None;
    }
    let iso = v["timestamp"].as_str().map(str::to_string);
    let hm = hm_from_iso(iso.as_deref());
    let p = &v["payload"];
    match p["type"].as_str()? {
        "message" => {
            let role = match p["role"].as_str()? {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => return None, // developer/system: context wrappers, not conversation
            };
            let blocks = p["content"].as_array()?;
            let text = join_text(blocks, &["text", "input_text", "output_text"]).trim().to_string();
            // Skip the XML-ish context blobs Codex prepends as user turns.
            if role == Role::User && text.starts_with('<') {
                return None;
            }
            if text.is_empty() {
                return None;
            }
            Some(Entry { hm, iso, role, text, tools: vec![], title: None })
        }
        "function_call" => {
            let name = p["name"].as_str().unwrap_or("tool").to_string();
            let raw = p["arguments"].as_str().unwrap_or("");
            let command = serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(|a| a["cmd"].as_str().or_else(|| a["command"].as_str()).map(str::to_string));
            let is_shell = CODEX_SHELL_TOOLS.contains(&name.as_str());
            let arg = command.clone().unwrap_or_else(|| raw.to_string());
            Some(Entry {
                hm,
                iso,
                role: Role::Assistant,
                text: String::new(),
                tools: vec![Tool { name, arg, command: if is_shell { command } else { None } }],
                title: None,
            })
        }
        _ => None,
    }
}

// ── pi ────────────────────────────────────────────────────────────────────────

fn parse_pi(v: &Value) -> Option<Entry> {
    match v["type"].as_str()? {
        "session_info" => v["name"].as_str().map(|t| title_entry(t, String::new(), None)),
        "message" => {
            let m = &v["message"];
            let ms = m["timestamp"].as_u64();
            let iso = ms.map(ms_to_iso);
            let hm = iso.as_deref().map(|s| hm_from_iso(Some(s))).unwrap_or_default();
            let role = match m["role"].as_str()? {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => return None, // toolResult, bashExecution: not conversation
            };
            let blocks = m["content"].as_array()?;
            let mut text = String::new();
            let mut tools = Vec::new();
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                            if !text.is_empty() {
                                text.push(' ');
                            }
                            text.push_str(t);
                        }
                    }
                    Some("toolCall") => {
                        let name = b["name"].as_str().unwrap_or("tool").to_string();
                        let command = b["arguments"]["command"].as_str().map(str::to_string);
                        let arg = command
                            .clone()
                            .or_else(|| b["arguments"]["file_path"].as_str().map(str::to_string))
                            .or_else(|| b["arguments"]["pattern"].as_str().map(str::to_string))
                            .unwrap_or_default();
                        let is_shell = name == "bash" || name == "shell";
                        tools.push(Tool { name, arg, command: if is_shell { command } else { None } });
                    }
                    _ => {}
                }
            }
            Some(Entry { hm, iso, role, text: text.trim().to_string(), tools, title: None })
        }
        _ => None,
    }
}

// ── shared helpers ────────────────────────────────────────────────────────────

fn title_entry(t: &str, hm: String, iso: Option<String>) -> Entry {
    Entry { hm, iso, role: Role::Title, text: String::new(), tools: vec![], title: Some(t.to_string()) }
}

/// Join the `text` field of content blocks whose value lives under any of
/// `keys`, in order, space-separated.
fn join_text(blocks: &[Value], _keys: &[&str]) -> String {
    blocks
        .iter()
        .filter_map(|b| b["text"].as_str())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn hm_from_iso(iso: Option<&str>) -> String {
    iso.and_then(|t| t.get(11..16)).unwrap_or("").to_string()
}

/// Milliseconds since the epoch → `YYYY-MM-DDTHH:MM:SSZ` (UTC), so pi's numeric
/// timestamps sort against the ISO strings the other agents write.
fn ms_to_iso(ms: u64) -> String {
    let secs = ms / 1000;
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_user_assistant_and_title() {
        let u = r#"{"type":"user","timestamp":"2026-09-03T10:15:00Z","message":{"content":"fix the tests"}}"#;
        let e = parse_line("claude", u).unwrap();
        assert_eq!((e.role, e.hm.as_str(), e.text.as_str()), (Role::User, "10:15", "fix the tests"));
        let a = r#"{"type":"assistant","timestamp":"2026-09-03T10:16:00Z","message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"git commit -m x"}}]}}"#;
        let e = parse_line("claude", a).unwrap();
        assert_eq!(e.role, Role::Assistant);
        assert_eq!(e.text, "On it.");
        assert_eq!(e.commands().collect::<Vec<_>>(), vec!["git commit -m x"]);
        let t = r#"{"type":"ai-title","timestamp":"2026-09-03T10:00:00Z","aiTitle":"Fix the test suite"}"#;
        assert_eq!(parse_line("claude", t).unwrap().title.as_deref(), Some("Fix the test suite"));
    }

    #[test]
    fn codex_message_function_and_context_filter() {
        let user = r#"{"timestamp":"2026-09-04T15:32:41Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Run the tests"}]}}"#;
        let e = parse_line("codex", user).unwrap();
        assert_eq!((e.role, e.hm.as_str(), e.text.as_str()), (Role::User, "15:32", "Run the tests"));
        // The <environment_context> / developer wrappers are dropped.
        let ctx = r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/x</cwd>"}]}}"#;
        assert!(parse_line("codex", ctx).is_none());
        let dev = r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"skills"}]}}"#;
        assert!(parse_line("codex", dev).is_none());
        let fc = r#"{"timestamp":"2026-09-04T15:33:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"git push\",\"justification\":\"x\"}"}}"#;
        let e = parse_line("codex", fc).unwrap();
        assert_eq!(e.commands().collect::<Vec<_>>(), vec!["git push"]);
        assert!(parse_line("codex", r#"{"type":"event_msg","payload":{"type":"task_started"}}"#).is_none());
    }

    #[test]
    fn pi_message_toolcall_and_dirname() {
        let u = r#"{"type":"message","message":{"role":"user","timestamp":1788537339069,"content":[{"type":"text","text":"run ls"}]}}"#;
        let e = parse_line("pi", u).unwrap();
        assert_eq!(e.role, Role::User);
        assert_eq!(e.text, "run ls");
        assert!(e.iso.as_deref().unwrap().starts_with("2026-"));
        let a = r#"{"type":"message","message":{"role":"assistant","timestamp":1788537339091,"content":[{"type":"toolCall","name":"bash","arguments":{"command":"git commit -am y"}}]}}"#;
        assert_eq!(parse_line("pi", a).unwrap().commands().collect::<Vec<_>>(), vec!["git commit -am y"]);
        let r = r#"{"type":"message","message":{"role":"toolResult","content":[{"type":"text","text":"out"}]}}"#;
        assert!(parse_line("pi", r).is_none());
        assert_eq!(pi_session_dirname("/ws/tasks/foo"), "--ws-tasks-foo--");
    }

    #[test]
    fn ms_to_iso_is_comparable() {
        // 1788537339069 ms = 2026-09-04T... — must sort after an earlier ISO.
        let iso = ms_to_iso(1788537339069);
        assert!(iso.as_str() > "2026-09-04T00:00:00Z");
        assert!(iso.ends_with('Z') && iso.len() == 20);
    }
}
