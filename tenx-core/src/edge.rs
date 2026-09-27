//! When a condition earns a notification: on the *edge* into it, once, after
//! it has held for a moment — the rule the attention watcher's desktop
//! notifications and `tenx web`'s push notifications share, so the two fire
//! on exactly the same edges.
//!
//! A key (a task) that is in the condition when tracking starts is primed as
//! already notified: a backlog at startup is noise, not news. A key that
//! leaves the condition is forgotten, so its next entry notifies again.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub struct Edges {
    notified: HashSet<String>,
    /// Consecutive polls each not-yet-notified key has been in the condition.
    pending: HashMap<String, u32>,
    /// How many polls a key must already have been seen in the condition
    /// before the next one notifies (0 = on first sight).
    debounce: u32,
}

impl Edges {
    /// Start tracking with `current` (what is in the condition now) primed.
    pub fn primed<I: IntoIterator<Item = String>>(current: I, debounce: u32) -> Edges {
        Edges { notified: current.into_iter().collect(), pending: HashMap::new(), debounce }
    }

    /// One poll: `current` is every key in the condition now; the ones to
    /// notify about this time, in `current`'s order.
    pub fn step<'a>(&mut self, current: &[&'a str]) -> Vec<&'a str> {
        let mut fire = Vec::new();
        for &key in current {
            if self.notified.contains(key) {
                continue;
            }
            let seen = self.pending.entry(key.to_string()).or_insert(0);
            *seen += 1;
            if *seen > self.debounce {
                fire.push(key);
                self.notified.insert(key.to_string());
                self.pending.remove(key);
            }
        }
        let now: HashSet<&str> = current.iter().copied().collect();
        self.pending.retain(|k, _| now.contains(k.as_str()));
        self.notified.retain(|k| now.contains(k.as_str()));
        fire
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_after_the_debounce_and_again_after_leaving() {
        let mut e = Edges::primed([], 1);
        assert!(e.step(&["a"]).is_empty(), "first sight: debounced");
        assert_eq!(e.step(&["a"]), vec!["a"]);
        assert!(e.step(&["a"]).is_empty(), "once per edge");
        assert!(e.step(&[]).is_empty());
        assert!(e.step(&["a"]).is_empty());
        assert_eq!(e.step(&["a"]), vec!["a"], "a new edge");
    }

    #[test]
    fn a_blip_shorter_than_the_debounce_never_fires() {
        let mut e = Edges::primed([], 1);
        assert!(e.step(&["a"]).is_empty());
        assert!(e.step(&[]).is_empty());
        assert!(e.step(&["a"]).is_empty(), "the count starts over");
    }

    #[test]
    fn what_was_there_at_startup_is_primed() {
        let mut e = Edges::primed(["a".to_string()], 0);
        assert_eq!(e.step(&["a", "b"]), vec!["b"]);
    }
}
