/// Convert a user-supplied task name into a slug that is safe as a directory
/// name, a git branch name, and a tmux window name: ASCII-lowercased, every
/// run of anything outside `[a-z0-9]` collapsed to a single `-`, no leading or
/// trailing `-`. Non-ASCII letters are dropped rather than transliterated —
/// predictable beats clever for something that becomes a branch name.
///
/// The result can be empty (a name made only of punctuation); callers must
/// treat that as an error rather than a task named `""`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn spaces_and_underscores_become_dashes() {
        assert_eq!(slugify("My Task"), "my-task");
        assert_eq!(slugify("foo_bar"), "foo-bar");
    }

    #[test]
    fn punctuation_is_stripped_not_kept() {
        assert_eq!(slugify("Fix: it's broken!"), "fix-it-s-broken");
        assert_eq!(slugify("ENG-123: Add login"), "eng-123-add-login");
    }

    #[test]
    fn runs_collapse_and_ends_trim() {
        assert_eq!(slugify("  --x--  y  "), "x-y");
        assert_eq!(slugify("a...b"), "a-b");
    }

    #[test]
    fn empty_and_all_punctuation_give_empty() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn already_a_slug_is_unchanged() {
        assert_eq!(slugify("add-repos"), "add-repos");
    }
}
