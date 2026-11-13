//! Claude Code agents as a task's main session: `claude --agent <name>` runs
//! the session *as* that agent — its prompt, tools and model — so you talk to
//! it directly instead of through a subagent call. An agent is a Markdown file
//! with YAML frontmatter in `~/.claude/agents/` or a project's
//! `.claude/agents/`; this module reads one ([`parse`]) and orders what a
//! picker shows ([`merge`]). Finding the files is the binary's
//! (`agent::claude_agents`).

/// Where an agent file was found. A project's agent wins over the user's of
/// the same name, as in Claude Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// The workspace's `.claude/agents/` (every task links it as its own).
    Workspace,
    /// `~/.claude/agents/`.
    User,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Workspace => "workspace",
            Scope::User => "user",
        }
    }
}

/// One agent a task can start as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// What `--agent` takes: the frontmatter's `name`, else the file's stem.
    pub name: String,
    /// The frontmatter's `description`, first line only; empty when none.
    pub description: String,
    pub scope: Scope,
}

/// Read an agent file. `None` when it has no frontmatter — Claude Code skips
/// such a file, so a picker must too. `stem` is the file name without `.md`.
pub fn parse(stem: &str, text: &str, scope: Scope) -> Option<Definition> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut front = Vec::new();
    for line in lines.by_ref() {
        if line.trim() == "---" {
            break;
        }
        front.push(line);
    }
    let name = value(&front, "name").filter(|n| valid_name(n)).unwrap_or_else(|| stem.to_string());
    if !valid_name(&name) {
        return None;
    }
    let description = value(&front, "description").unwrap_or_default();
    Some(Definition { name, description, scope })
}

/// A top-level key's value: quotes taken off, a block scalar (`|`, `>`)
/// replaced by its first line, an escaped `\n` cut at.
fn value(front: &[&str], key: &str) -> Option<String> {
    let at = front.iter().position(|l| l.strip_prefix(key).is_some_and(|r| r.starts_with(':')))?;
    let raw = front[at][key.len() + 1..].trim();
    let raw = if raw.is_empty() || raw.starts_with('|') || raw.starts_with('>') {
        front.get(at + 1).filter(|l| l.starts_with(' ') || l.starts_with('\t')).map(|l| l.trim()).unwrap_or("")
    } else {
        raw
    };
    let raw = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')).or_else(|| raw.strip_prefix('\'').and_then(|r| r.strip_suffix('\''))).unwrap_or(raw);
    let first = raw.split("\\n").next().unwrap_or("").trim();
    (!first.is_empty()).then(|| first.to_string())
}

/// A name `--agent` can take as one shell word without surprises: letters,
/// digits, `-`, `_`, `.` and `:` (a plugin's agents are `plugin:name`).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

/// The picker's list: one entry per name, the workspace's over the user's,
/// sorted by name.
pub fn merge(mut found: Vec<Definition>) -> Vec<Definition> {
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.scope.cmp(&b.scope)));
    found.dedup_by(|later, kept| later.name == kept.name);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_name_and_first_description_line() {
        let text = "---\nname: sales\ndescription: \"Andreas's sales helper.\\n\\nExamples: …\"\ntools: Read\n---\nYou are…";
        let d = parse("file", text, Scope::User).unwrap();
        assert_eq!(d.name, "sales");
        assert_eq!(d.description, "Andreas's sales helper.");
    }

    #[test]
    fn block_scalar_description_and_stem_fallback() {
        let text = "---\ndescription: >\n  Reviews code.\n  More.\n---\n";
        let d = parse("reviewer", text, Scope::Workspace).unwrap();
        assert_eq!(d.name, "reviewer");
        assert_eq!(d.description, "Reviews code.");
    }

    #[test]
    fn no_frontmatter_is_no_agent() {
        assert_eq!(parse("notes", "# just notes\n", Scope::User), None);
    }

    #[test]
    fn a_name_that_is_no_shell_word_is_refused() {
        assert_eq!(parse("ok", "---\nname: rm -rf\n---\n", Scope::User).unwrap().name, "ok");
        assert_eq!(parse("bad name", "---\n---\n", Scope::User), None);
        assert!(valid_name("plugin:helper"));
        assert!(!valid_name("--model"));
    }

    #[test]
    fn workspace_wins_and_list_is_sorted() {
        let def = |name: &str, scope| Definition { name: name.into(), description: String::new(), scope };
        let merged = merge(vec![def("sales", Scope::User), def("alpha", Scope::User), def("sales", Scope::Workspace)]);
        assert_eq!(merged, vec![def("alpha", Scope::User), def("sales", Scope::Workspace)]);
    }
}
