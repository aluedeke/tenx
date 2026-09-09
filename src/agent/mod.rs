//! Which coding-agent harness a task runs, and everything that differs by
//! harness: the command launched in the task pane, whether a prior conversation
//! exists to resume, and the process names its sessions run under.
//!
//! tenx supports Claude Code, Codex CLI and pi. The state model is uniform —
//! every agent feeds `tenx`'s session registry through its own hooks/extension
//! (`workspace::sessions`, `tenx_core::session_event`) — so the only things that
//! vary are the launch/resume mechanics collected here. Claude Code is the
//! default and the first fully wired; Codex and pi land in later phases.

use std::path::Path;

/// A coding-agent harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    Codex,
    Pi,
}

impl AgentKind {
    /// Every agent, for `tenx init`/`doctor` and setup loops.
    pub fn all() -> [AgentKind; 3] {
        [AgentKind::Claude, AgentKind::Codex, AgentKind::Pi]
    }

    /// The wire token: what goes in `config.toml`'s `agent`, the `.tenx-agent`
    /// file, a record's `agent` field, and `--agent`.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Pi => "pi",
        }
    }

    /// Parse a token; unknown or empty falls back to the default agent
    /// (`claude`), so an unset workspace behaves exactly as tenx did before
    /// agents were configurable.
    pub fn from_token(token: &str) -> AgentKind {
        match token.trim() {
            "codex" => AgentKind::Codex,
            "pi" => AgentKind::Pi,
            _ => AgentKind::Claude,
        }
    }

    /// Process command substrings a session of this agent runs under — used to
    /// climb from a hook to the agent's pid, and to spot an agent process in a
    /// pane. Matched with `contains`, so partial names are fine.
    pub fn process_names(self) -> &'static [&'static str] {
        match self {
            AgentKind::Claude => &["claude"],
            AgentKind::Codex => &["codex"],
            AgentKind::Pi => &["pi"],
        }
    }

    /// The agent's default binary name.
    pub fn default_bin(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Pi => "pi",
        }
    }

    /// The shell command run in the task's pane, with an explicit binary (a
    /// user's wrapper, or [`default_bin`](Self::default_bin)) and extra arguments
    /// appended (a pinned model, say). `task_dir` is the pane's cwd, used to
    /// decide whether a resume flag is safe. tenx supplies the per-agent
    /// session/resume args around `extra`. See [`launch`] for the config-aware
    /// entry point that fills these in from `[agents.<kind>]`.
    pub fn launch_command_with(self, bin: &str, slug: &str, task_dir: &Path, extra: &[String]) -> String {
        let q = crate::tmux::shell_quote(slug);
        let tail = if extra.is_empty() { String::new() } else { format!(" {}", extra.join(" ")) };
        match self {
            // `--continue` only when a transcript exists; otherwise claude exits
            // 1 and the pane vanishes.
            AgentKind::Claude => {
                let resume = if self.has_conversation(task_dir) { " --continue" } else { "" };
                format!("{bin} --name {q}{resume}{tail}")
            }
            // `codex resume --last` picks the newest thread for this cwd; with
            // none it errors, so only resume when one exists.
            AgentKind::Codex => {
                if self.has_conversation(task_dir) {
                    format!("{bin} resume --last{tail}")
                } else {
                    format!("{bin}{tail}")
                }
            }
            // pi's `-c` continues the most recent session for the cwd or starts
            // fresh — always safe, never exits nonzero.
            AgentKind::Pi => format!("{bin} --name {q} -c{tail}"),
        }
    }

    /// Whether the agent has a stored conversation for `task_dir`, so a resume
    /// flag will continue rather than error.
    pub fn has_conversation(self, task_dir: &Path) -> bool {
        match self {
            AgentKind::Claude => claude_has_conversation(task_dir),
            AgentKind::Codex => codex_has_conversation(task_dir),
            // pi's `-c` starts fresh when there's nothing to continue, so it is
            // always safe and needs no lookup.
            AgentKind::Pi => true,
        }
    }

    /// Prepare anything the agent needs before its window opens. For Codex, that
    /// is trusting the task directory (so its project-local config and this
    /// session's hooks load, and it doesn't stop on the trust prompt). No-op for
    /// the others. Best-effort: a failure here must not block opening the task.
    pub fn prepare(self, task_dir: &Path) {
        if self == AgentKind::Codex {
            let _ = ensure_codex_trust(task_dir);
        }
    }
}

/// Whether Claude Code has stored a conversation for `cwd` (so `--continue` will
/// resume instead of exiting 1). Claude encodes each project dir as its path
/// with `/` → `-` under `~/.claude/projects/`.
fn claude_has_conversation(cwd: &Path) -> bool {
    let Some(project_dir) = crate::workspace::sessions::project_dir(cwd) else {
        return false;
    };
    match std::fs::read_dir(&project_dir) {
        Ok(entries) => entries.flatten().any(|e| e.path().extension().is_some_and(|ext| ext == "jsonl")),
        Err(_) => false,
    }
}

/// Whether Codex has a resumable thread whose cwd is `task_dir`. Scans
/// `~/.codex/sessions/**/rollout-*.jsonl` and reads each file's first
/// (`session_meta`) line. Returns on the first match; a false negative just
/// starts a fresh Codex session, so this errs toward not resuming.
fn codex_has_conversation(task_dir: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let root = std::path::PathBuf::from(home).join(".codex/sessions");
    let want = task_dir.canonicalize().unwrap_or_else(|_| task_dir.to_path_buf());
    codex_rollouts(&root).into_iter().any(|f| {
        // Only plain .jsonl are cheaply readable line-1; a compressed sibling
        // means a session existed here anyway, so treat its cwd as unknown-yes
        // only when we can confirm it. Confirm from the plain file.
        first_line(&f)
            .and_then(|l| tenx_core::codex::session_meta_cwd(&l))
            .map(|c| std::path::Path::new(&c) == want || std::path::PathBuf::from(&c) == want)
            .unwrap_or(false)
    })
}

/// Every plain rollout `.jsonl` under the Codex sessions tree (dated
/// subdirectories), newest directories first so a match is usually found fast.
fn codex_rollouts(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    // sessions/YYYY/MM/DD/rollout-*.jsonl — walk three levels, then files.
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl")
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-"))
            {
                out.push(p);
            }
        }
    }
    out
}

fn first_line(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line).ok()?;
    (!line.is_empty()).then_some(line)
}

/// Trust `task_dir` in `~/.codex/config.toml` so Codex loads its project-local
/// config and hooks and skips the interactive trust prompt. Appends the
/// `[projects."<dir>"]` block only when absent — non-destructive (never rewrites
/// the file, so the user's comments and formatting survive) and idempotent (a
/// duplicate block would make Codex's TOML invalid). A `-c` CLI override does
/// not work for trust; only config.toml does (verified).
fn ensure_codex_trust(task_dir: &Path) -> std::io::Result<()> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(());
    };
    let dir = task_dir.to_string_lossy();
    let path = std::path::PathBuf::from(home).join(".codex/config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let header = format!("[projects.\"{dir}\"]");
    if existing.contains(&header) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n{header}\ntrust_level = \"trusted\"\n"));
    std::fs::write(&path, out)
}

/// Build a task's launch command, honouring `[agents.<kind>]` overrides: the
/// workspace config wins over the global config, and either supplies an
/// alternate binary and/or extra args around tenx's per-agent session flags.
pub fn launch(ws: &crate::workspace::Workspace, kind: AgentKind, slug: &str, task_dir: &Path) -> String {
    let key = kind.as_str();
    let global = crate::workspace::load_global().ok();
    let ws_cfg = ws.config.agents.get(key);
    let global_cfg = global.as_ref().and_then(|g| g.agents.get(key));
    // Prefer the workspace entry as a whole when present, else the global one.
    let cfg = ws_cfg.or(global_cfg);
    let bin = cfg.and_then(|c| c.command.as_deref()).unwrap_or_else(|| kind.default_bin());
    let empty: Vec<String> = Vec::new();
    let args = cfg.map(|c| &c.args).unwrap_or(&empty);
    kind.launch_command_with(bin, slug, task_dir, args)
}

/// The agent a task runs: its own `.tenx-agent` override, else the workspace
/// default (`config.toml`'s `agent`), else the built-in default (`claude`).
pub fn agent_for(ws: &crate::workspace::Workspace, task_dir: &Path) -> AgentKind {
    if let Ok(token) = std::fs::read_to_string(task_dir.join(TENX_AGENT_FILE)) {
        let token = token.trim();
        if !token.is_empty() {
            return AgentKind::from_token(token);
        }
    }
    AgentKind::from_token(&ws.config.agent)
}

/// Per-task agent override: one word (`codex`), same style as `.tenx-pinned`.
/// A user decision, not a cache — so it is never rewritten by tenx's own state.
pub const TENX_AGENT_FILE: &str = ".tenx-agent";

/// Write (or clear) a task's agent override.
pub fn set_task_agent(task_dir: &Path, kind: Option<AgentKind>) -> std::io::Result<()> {
    let path = task_dir.join(TENX_AGENT_FILE);
    match kind {
        Some(k) => std::fs::write(&path, format!("{}\n", k.as_str())),
        None => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip_with_claude_default() {
        for k in AgentKind::all() {
            assert_eq!(AgentKind::from_token(k.as_str()), k);
        }
        assert_eq!(AgentKind::from_token(""), AgentKind::Claude);
        assert_eq!(AgentKind::from_token("nonsense"), AgentKind::Claude);
        assert_eq!(AgentKind::from_token(" codex\n"), AgentKind::Codex);
    }

    #[test]
    fn launch_commands_are_shaped_per_agent() {
        let dir = Path::new("/nonexistent/task");
        let cmd = |k: AgentKind| k.launch_command_with(k.default_bin(), "my-task", dir, &[]);
        assert_eq!(cmd(AgentKind::Claude), "claude --name 'my-task'");
        assert_eq!(cmd(AgentKind::Codex), "codex");
        assert_eq!(cmd(AgentKind::Pi), "pi --name 'my-task' -c");
        // A wrapper binary and extra args are threaded through.
        assert_eq!(
            AgentKind::Codex.launch_command_with("mycodex", "t", dir, &["--model".into(), "o3".into()]),
            "mycodex --model o3"
        );
        assert_eq!(
            AgentKind::Pi.launch_command_with("pi", "t", dir, &["--yolo".into()]),
            "pi --name 't' -c --yolo"
        );
    }
}
