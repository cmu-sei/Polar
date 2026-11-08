/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! A workload's recent transitions: each time what a round found for it changed (its status, or why), at most
//! [`MAX_TRANSITIONS`] of them, oldest first. Kept by the writer, written with the latest report, and read back from
//! the graph when the writer starts, so a restart does not forget them.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// How many transitions a workload keeps.
pub const MAX_TRANSITIONS: usize = 20;

/// One change of what a round found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// When the round that found it closed, in Unix milliseconds.
    pub at_ms: i64,
    /// `starting`, `healthy`, `degraded`, `unhealthy`, `draining`, `unresponsive` or `unreachable`.
    pub state: String,
    pub reason: Option<String>,
}

/// The last transitions of one workload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    transitions: VecDeque<Transition>,
}

impl History {
    /// A history from what the graph held: the last [`MAX_TRANSITIONS`] of them, in time order (a stable sort, so
    /// two at the same instant keep the order they were written in), a repeat of the one before left out.
    pub fn from_stored(mut stored: Vec<Transition>) -> Self {
        stored.sort_by_key(|t| t.at_ms);
        let mut history = History::default();
        for t in stored {
            history.record(t.at_ms, &t.state, t.reason.as_deref());
        }
        history
    }

    /// Records what a round found at `at_ms`, if it differs from the last transition: then it is a transition, the
    /// oldest goes beyond the bound, and it returns true. A time earlier than the last (a clock stepped back) is taken
    /// as the last's, so the list stays in order.
    pub fn record(&mut self, at_ms: i64, state: &str, reason: Option<&str>) -> bool {
        if let Some(last) = self.transitions.back()
            && last.state == state
            && last.reason.as_deref() == reason
        {
            return false;
        }
        let at_ms = self
            .transitions
            .back()
            .map_or(at_ms, |last| at_ms.max(last.at_ms));
        self.transitions.push_back(Transition {
            at_ms,
            state: state.to_string(),
            reason: reason.map(str::to_string),
        });
        while self.transitions.len() > MAX_TRANSITIONS {
            self.transitions.pop_front();
        }
        true
    }

    /// The transitions, oldest first.
    pub fn transitions(&self) -> impl Iterator<Item = &Transition> {
        self.transitions.iter()
    }

    pub fn len(&self) -> usize {
        self.transitions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn only_a_change_is_a_transition_and_the_oldest_go_past_the_bound() {
        let mut h = History::default();
        assert!(h.record(1, "starting", None));
        assert!(!h.record(2, "starting", None));
        assert!(h.record(3, "unhealthy", Some("configuration")));
        assert!(!h.record(4, "unhealthy", Some("configuration")));
        assert!(h.record(5, "unhealthy", Some("credentials")));
        assert_eq!(h.len(), 3);
        for i in 0..30 {
            h.record(
                10 + i,
                if i % 2 == 0 {
                    "healthy"
                } else {
                    "unresponsive"
                },
                None,
            );
        }
        assert_eq!(h.len(), MAX_TRANSITIONS);
        assert_eq!(h.transitions().last().unwrap().at_ms, 39);
        // A clock that stepped back does not put a transition before the last.
        h.record(0, "unreachable", None);
        assert_eq!(h.transitions().last().unwrap().at_ms, 39);
    }

    fn state() -> impl Strategy<Value = (String, Option<String>)> {
        (
            proptest::sample::select(vec![
                "starting",
                "healthy",
                "degraded",
                "unhealthy",
                "unresponsive",
                "unreachable",
            ]),
            proptest::option::of(proptest::sample::select(vec!["broker", "configuration"])),
        )
            .prop_map(|(s, r)| (s.to_string(), r.map(str::to_string)))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// After any sequence of reports, a history holds at most the bound, in time order, never the same state twice
        /// in a row, and is exactly the last transitions of an unbounded record of the same reports.
        #[test]
        fn the_transitions_stay_bounded_and_in_order(
            reports in proptest::collection::vec((-5i64..1000, state()), 0..80)
        ) {
            let mut history = History::default();
            let mut unbounded: Vec<Transition> = Vec::new();
            for (at_ms, (state, reason)) in &reports {
                history.record(*at_ms, state, reason.as_deref());
                let changed = unbounded
                    .last()
                    .is_none_or(|last| &last.state != state || &last.reason != reason);
                if changed {
                    let at_ms = unbounded.last().map_or(*at_ms, |last| (*at_ms).max(last.at_ms));
                    unbounded.push(Transition { at_ms, state: state.clone(), reason: reason.clone() });
                }
            }
            let held: Vec<Transition> = history.transitions().cloned().collect();
            prop_assert!(held.len() <= MAX_TRANSITIONS);
            prop_assert!(held.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
            prop_assert!(held.windows(2).all(|w| (&w[0].state, &w[0].reason) != (&w[1].state, &w[1].reason)));
            let tail = &unbounded[unbounded.len().saturating_sub(MAX_TRANSITIONS)..];
            prop_assert_eq!(held.as_slice(), tail);
            // What the graph would give back reads as the same history.
            prop_assert_eq!(History::from_stored(held.clone()), history);
        }
    }
}
