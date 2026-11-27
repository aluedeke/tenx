//! The "delete …? [y/N]" question before `tenx task rm` deletes a task.

/// `tenx task rm --ws-dir`: ask unless `force`, and only when stdin is a
/// terminal — a person who can answer. A script or front end is not asked.
/// stderr, where the question goes, is not looked at: sent to a file it hides
/// the question and the command waits, which is safer than deleting unasked.
pub fn ask_before_delete(force: bool, stdin_tty: bool) -> bool {
    !force && stdin_tty
}

/// Whether an answer to the question is a yes: `y` or `yes`, any case.
pub fn is_yes(answer: &str) -> bool {
    let a = answer.trim();
    a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_at_a_terminal_is_asked() {
        assert!(ask_before_delete(false, true));
    }

    #[test]
    fn force_never_asks() {
        assert!(!ask_before_delete(true, true));
        assert!(!ask_before_delete(true, false));
    }

    #[test]
    fn a_script_is_not_asked() {
        assert!(!ask_before_delete(false, false));
    }

    #[test]
    fn y_and_yes_are_yes() {
        for a in ["y", "Y", "yes", "YES", " Yes\n"] {
            assert!(is_yes(a), "{a:?}");
        }
    }

    #[test]
    fn anything_else_is_no() {
        for a in ["", "\n", "n", "no", "ye", "yes please", "y es"] {
            assert!(!is_yes(a), "{a:?}");
        }
    }
}
