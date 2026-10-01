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

/// The first of `base`, `base-2`, `base-3`, … that `taken` doesn't claim —
/// how a detached session gets a slug of its own when its title repeats an
/// earlier one ("question" twice). Workspaces with repos never take this
/// path: their slug is a branch name, and a duplicate there is an error the
/// user should see.
pub fn unique_slug(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken(s))
        .expect("an unbounded range always finds a free slug")
}

#[cfg(test)]
mod tests {
    use super::{slugify, unique_slug};

    #[test]
    fn unique_slug_counts_up_from_two() {
        assert_eq!(unique_slug("q", |_| false), "q");
        assert_eq!(unique_slug("q", |s| s == "q"), "q-2");
        assert_eq!(unique_slug("q", |s| ["q", "q-2", "q-3"].contains(&s)), "q-4");
    }

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
