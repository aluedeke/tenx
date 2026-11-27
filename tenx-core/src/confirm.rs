//! Whether a deleting command asks "…? [y/N]" before it acts.

/// `tenx task rm --ws-dir`: ask unless `force`, and only when a person can see
/// the question and answer it — stderr (where it is written) and stdin (where
/// the answer comes from) both a terminal. A script or front end is not
/// asked: there is nobody to answer, and a question would only make it hang.
pub fn ask_before_delete(force: bool, stdin_tty: bool, stderr_tty: bool) -> bool {
    !force && stdin_tty && stderr_tty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_at_a_terminal_is_asked() {
        assert!(ask_before_delete(false, true, true));
    }

    #[test]
    fn force_never_asks() {
        assert!(!ask_before_delete(true, true, true));
    }

    #[test]
    fn a_script_is_not_asked() {
        assert!(!ask_before_delete(false, false, false));
        assert!(!ask_before_delete(false, false, true));
    }

    #[test]
    fn a_hidden_question_is_not_asked() {
        // stderr to a log: the question would wait where nobody sees it.
        assert!(!ask_before_delete(false, true, false));
    }
}
