//! `TASK.md`: the one file a task owns for humans and agents alike. Its first
//! `# ` heading is the task's display name; the `## Links` section is the
//! authoritative source for PR/ticket URLs (`tenx standup` reads it verbatim).

/// The link labels a fresh task starts with, in order.
pub const DEFAULT_LINK_LABELS: [&str; 4] = ["Linear Project", "Linear Milestone", "Linear", "PR"];

/// The default `## Links` rows: every label, no value.
pub fn default_links() -> Vec<(String, String)> {
    DEFAULT_LINK_LABELS.iter().map(|l| (l.to_string(), String::new())).collect()
}

/// Render a complete `TASK.md`. With an empty description and the default
/// links this is byte-identical to the template tenx has always written, so a
/// ticket-imported task and a hand-created one differ only in what's filled in.
pub fn render_task_md(title: &str, description: &str, links: &[(String, String)]) -> String {
    let mut out = format!("# {title}\n\n## Description\n\n");
    let description = description.trim();
    if description.is_empty() {
        out.push('\n');
    } else {
        out.push_str(description);
        out.push_str("\n\n");
    }
    out.push_str("## Todo\n\n- [ ] \n\n## Links\n\n");
    for (label, value) in links {
        if value.is_empty() {
            out.push_str(&format!("- {label}:\n"));
        } else {
            out.push_str(&format!("- {label}: {value}\n"));
        }
    }
    out.push_str("\n## Notes\n\n");
    out
}

/// Parse a `--link "Label: value"` argument. The first `:` splits label from
/// value (URLs keep theirs); a missing value is allowed and leaves the row
/// blank, so `--link "PR:"` is a no-op rather than an error.
pub fn parse_link(arg: &str) -> Option<(String, String)> {
    let (label, value) = arg.split_once(':')?;
    let label = label.trim();
    (!label.is_empty()).then(|| (label.to_string(), value.trim().to_string()))
}

/// The `## Links` rows for a new task: the default labels, in their usual
/// order, filled from `provided` (matched case-insensitively), then any
/// extra labels the caller gave, in the order given. So a ticket import
/// lands in the same rows a hand-written task has, and nothing is lost.
pub fn merge_links(provided: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = default_links();
    let mut extra = Vec::new();
    for (label, value) in provided {
        match out.iter_mut().find(|(l, _)| l.eq_ignore_ascii_case(label)) {
            Some((_, v)) => *v = value.clone(),
            None => extra.push((label.clone(), value.clone())),
        }
    }
    out.extend(extra);
    out
}

/// The display name a `TASK.md` declares: its first line with any leading
/// `#`s stripped. `None` if the file has no usable first line, so the caller
/// can fall back to the directory name.
pub fn display_name(content: &str) -> Option<String> {
    let first = content.lines().next()?;
    let title = first.trim_start_matches('#').trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// Rewrite the display name — the first `# ` heading — leaving the rest of the
/// file untouched; prepends one if the file doesn't start with a heading.
pub fn with_title(content: &str, title: &str) -> String {
    let heading = format!("# {title}");
    let mut lines: Vec<&str> = content.lines().collect();
    if lines.first().is_some_and(|l| l.trim_start().starts_with('#')) {
        lines[0] = heading.as_str();
        lines.join("\n") + "\n"
    } else {
        format!("{heading}\n{content}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The template `cli::task::write_task_md` wrote before this module
    /// existed. Guarded verbatim so ticket import can't drift the default.
    const LEGACY: &str = "# rebuild\n\n## Description\n\n\n## Todo\n\n- [ ] \n\n## Links\n\n- Linear Project:\n- Linear Milestone:\n- Linear:\n- PR:\n\n## Notes\n\n";

    #[test]
    fn default_render_matches_legacy_template_byte_for_byte() {
        assert_eq!(render_task_md("rebuild", "", &default_links()), LEGACY);
    }

    #[test]
    fn description_and_links_fill_in() {
        let links = vec![
            ("Linear".to_string(), "https://linear.app/x/ENG-1".to_string()),
            ("PR".to_string(), String::new()),
        ];
        let md = render_task_md("Add login", "  Users can log in.\n", &links);
        assert!(md.starts_with("# Add login\n\n## Description\n\nUsers can log in.\n\n## Todo\n"));
        assert!(md.contains("- Linear: https://linear.app/x/ENG-1\n- PR:\n\n## Notes\n\n"));
    }

    #[test]
    fn links_parse_and_merge_into_default_rows() {
        assert_eq!(parse_link("Linear: https://linear.app/x/ENG-1").unwrap(), ("Linear".into(), "https://linear.app/x/ENG-1".into()));
        assert_eq!(parse_link("PR:").unwrap(), ("PR".into(), String::new()));
        assert!(parse_link("no separator").is_none());
        assert!(parse_link(": value").is_none());

        let merged = merge_links(&[
            ("linear".to_string(), "https://l/1".to_string()),
            ("Jira".to_string(), "https://j/2".to_string()),
        ]);
        let labels: Vec<&str> = merged.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Linear Project", "Linear Milestone", "Linear", "PR", "Jira"]);
        assert_eq!(merged[2].1, "https://l/1");
        assert_eq!(merged[4].1, "https://j/2");
        let md = render_task_md("t", "", &merged);
        assert!(md.contains("- Linear: https://l/1\n- PR:\n- Jira: https://j/2\n\n## Notes"));
    }

    #[test]
    fn display_name_reads_first_heading() {
        assert_eq!(display_name("# Hello world\n\nbody").as_deref(), Some("Hello world"));
        assert_eq!(display_name("## deep\n").as_deref(), Some("deep"));
        assert_eq!(display_name("#\n"), None);
        assert_eq!(display_name(""), None);
    }

    #[test]
    fn with_title_replaces_or_prepends() {
        assert_eq!(with_title("# old\nbody\n", "new"), "# new\nbody\n");
        assert_eq!(with_title("body\n", "new"), "# new\nbody\n");
    }
}
