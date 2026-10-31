//! Which commits the git observer has already shown us.
//!
//! The observer reports a ref each time it looks, and again from nothing (`old` is empty) whenever it starts, which
//! it does about hourly as its certificate is renewed. A commit is built once, so one that has been seen is not
//! built again for the same report. A build that did not succeed is forgotten (`forget`), so that the next time the
//! commit is reported it is tried again; one that succeeded stays remembered. A manual request for a build is not
//! asked about here: whoever makes one wants it.
//!
//! Held in memory and bounded: an orchestrator that restarts builds each ref's commit once more, when the observer
//! also starts from nothing, and the oldest commits are let go past `cap`.

use std::collections::{HashSet, VecDeque};

pub struct Observed {
    cap: usize,
    seen: HashSet<(String, String)>,
    /// Oldest first, to let the oldest go.
    order: VecDeque<(String, String)>,
}

impl Observed {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Remember this commit of this repository; true if it was not already.
    pub fn first_sight(&mut self, repo_url: &str, commit: &str) -> bool {
        let key = (repo_url.to_string(), commit.to_string());
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }

    /// Let this commit be built again the next time it is reported.
    pub fn forget(&mut self, repo_url: &str, commit: &str) {
        let key = (repo_url.to_string(), commit.to_string());
        if self.seen.remove(&key) {
            self.order.retain(|k| k != &key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_reported_again_is_not_new() {
        let mut observed = Observed::new(8);
        assert!(observed.first_sight("https://git/r", "aaa"));
        assert!(!observed.first_sight("https://git/r", "aaa"));
    }

    #[test]
    fn the_same_commit_of_another_repository_is_new() {
        let mut observed = Observed::new(8);
        assert!(observed.first_sight("https://git/r", "aaa"));
        assert!(observed.first_sight("https://git/s", "aaa"));
        assert!(observed.first_sight("https://git/r", "bbb"));
    }

    #[test]
    fn a_forgotten_commit_is_built_again() {
        let mut observed = Observed::new(8);
        observed.first_sight("https://git/r", "aaa");
        observed.forget("https://git/r", "aaa");
        assert!(observed.first_sight("https://git/r", "aaa"));
        // Forgetting what was never seen does nothing.
        observed.forget("https://git/r", "zzz");
    }

    #[test]
    fn the_oldest_commit_is_let_go_past_the_cap() {
        let mut observed = Observed::new(2);
        observed.first_sight("u", "a");
        observed.first_sight("u", "b");
        observed.first_sight("u", "c");
        assert!(observed.first_sight("u", "a"), "a was the oldest, so it is new again");
        assert!(!observed.first_sight("u", "c"), "c is still held");
    }

    #[test]
    fn a_forgotten_then_seen_commit_does_not_leave_two_places_in_the_line() {
        let mut observed = Observed::new(2);
        observed.first_sight("u", "a");
        observed.forget("u", "a");
        observed.first_sight("u", "a");
        observed.first_sight("u", "b");
        assert!(!observed.first_sight("u", "a"), "a is within the cap and must still be held");
    }
}
