//! A task title made from its first prompt: the request a small model gets,
//! and what of its answer is kept.
//!
//! The new-task form with a prompt and no name, and the quick task
//! (`Ctrl+Shift+n`), ask Claude for a title before the task exists. The
//! binary runs the call (`cli::task::title_for_prompt`); this module decides
//! what is sent and whether the reply is a usable title. Anything it rejects
//! falls back to [`crate::orchestrate::ask_title`], the prompt's first words.

use crate::orchestrate::ASK_TITLE_MAX;

/// How much of the prompt the model sees. A title needs the gist; a pasted
/// log of thousands of lines only makes the call slower.
pub const PROMPT_MAX: usize = 4000;

/// The model the title call asks for: the fastest one, since the task waits.
pub const MODEL: &str = "haiku";

/// How long the call may take, in seconds, before the first words are used.
pub const TIMEOUT_SECS: u64 = 15;

/// What the model is asked, with the prompt (cut to [`PROMPT_MAX`]) inside.
pub fn request(prompt: &str) -> String {
    let prompt: String = prompt.trim().chars().take(PROMPT_MAX).collect();
    format!(
        "Write a short title (2 to 6 words) for a coding task that starts with the request below. \
         Reply with the title only: no quotes, no punctuation at the end, no explanation. \
         Use the language of the request.\n\n<request>\n{prompt}\n</request>"
    )
}

/// The model's reply as a title, or `None` when it isn't one: empty, no
/// letters or digits to make a slug from, or not a single short line.
pub fn clean(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|l| !l.is_empty())?;
    // Models like to wrap a title in quotes, bold it or label it.
    let line = line.strip_prefix("Title:").or_else(|| line.strip_prefix("title:")).unwrap_or(line);
    let line = line.trim().trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '#' | '“' | '”' | '‘' | '’'));
    let line = line.trim().trim_end_matches(['.', '!', ':', ';', ',']).trim();
    let title = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() || title.chars().count() > ASK_TITLE_MAX || crate::slug::slugify(&title).is_empty() {
        return None;
    }
    // More than one non-empty line is an explanation, not a title.
    if reply.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        return None;
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::{clean, request, PROMPT_MAX};

    #[test]
    fn the_request_holds_the_prompt_cut_to_size() {
        let r = request("  fix the login bug  ");
        assert!(r.contains("<request>\nfix the login bug\n</request>"));
        let long = "§".repeat(PROMPT_MAX + 100);
        assert_eq!(request(&long).matches('§').count(), PROMPT_MAX);
    }

    #[test]
    fn a_plain_reply_is_the_title() {
        assert_eq!(clean("Fix login redirect\n").as_deref(), Some("Fix login redirect"));
        assert_eq!(clean("  Fix   login  ").as_deref(), Some("Fix login"));
    }

    #[test]
    fn quotes_labels_and_end_punctuation_go() {
        assert_eq!(clean("\"Fix login redirect.\"").as_deref(), Some("Fix login redirect"));
        assert_eq!(clean("**Fix login**").as_deref(), Some("Fix login"));
        assert_eq!(clean("Title: Fix login").as_deref(), Some("Fix login"));
        assert_eq!(clean("“Login reparieren”").as_deref(), Some("Login reparieren"));
    }

    #[test]
    fn non_titles_are_refused() {
        assert_eq!(clean(""), None);
        assert_eq!(clean("   \n "), None);
        assert_eq!(clean("…"), None);
        assert_eq!(clean(&"word ".repeat(30)), None);
        assert_eq!(clean("Fix login\nThis title describes the task."), None);
    }
}
