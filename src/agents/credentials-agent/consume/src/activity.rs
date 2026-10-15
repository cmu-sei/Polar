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

//! What the consumer has been asked, so that silence can be told from idleness.
//!
//! A consumer that is subscribed to nothing and one with nothing to do look the same in its log
//! unless it says, now and then, how many requests it has seen. This counts requests by topic,
//! and `take_report` gives the counts since the last report along with how long ago the last
//! request arrived. It records identifiers and counts only, never what a request carries.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub struct Activity {
    since_report: BTreeMap<String, u64>,
    total: u64,
    last_request: Option<Instant>,
}

/// What `take_report` says.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    /// Requests received per topic since the previous report, topics with none left out.
    pub by_topic: Vec<(String, u64)>,
    /// Requests received since the consumer started.
    pub total: u64,
    /// How long ago the last request arrived; `None` if none ever has.
    pub last_request_ago: Option<Duration>,
}

impl Activity {
    pub fn new() -> Self {
        Self {
            since_report: BTreeMap::new(),
            total: 0,
            last_request: None,
        }
    }

    pub fn record(&mut self, topic: &str, at: Instant) {
        *self.since_report.entry(topic.to_string()).or_default() += 1;
        self.total += 1;
        self.last_request = Some(at);
    }

    pub fn take_report(&mut self, now: Instant) -> Report {
        Report {
            by_topic: std::mem::take(&mut self.since_report).into_iter().collect(),
            total: self.total,
            last_request_ago: self
                .last_request
                .map(|at| now.saturating_duration_since(at)),
        }
    }
}

impl Default for Activity {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let last = match self.last_request_ago {
            Some(ago) => format!("{}s ago", ago.as_secs()),
            None => "never".to_string(),
        };
        if self.by_topic.is_empty() {
            write!(
                f,
                "no requests since the last report ({} since start, last seen {last})",
                self.total
            )
        } else {
            let topics: Vec<String> = self
                .by_topic
                .iter()
                .map(|(topic, count)| format!("{topic}={count}"))
                .collect();
            write!(
                f,
                "requests since the last report: {} ({} since start, last seen {last})",
                topics.join(", "),
                self.total
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_consumer_that_has_seen_nothing_says_so_and_says_it_never_has() {
        let mut activity = Activity::new();
        let report = activity.take_report(Instant::now());
        assert_eq!(report.by_topic, vec![]);
        assert_eq!(report.total, 0);
        assert_eq!(report.last_request_ago, None);
        assert!(report.to_string().contains("last seen never"));
    }

    #[test]
    fn requests_are_counted_by_topic_and_a_report_starts_the_next_count_afresh() {
        let t0 = Instant::now();
        let mut activity = Activity::new();
        activity.record("a", t0);
        activity.record("b", t0);
        activity.record("a", t0);

        let first = activity.take_report(t0 + Duration::from_secs(60));
        assert_eq!(
            first.by_topic,
            vec![("a".to_string(), 2), ("b".to_string(), 1)]
        );
        assert_eq!(first.total, 3);
        assert_eq!(first.last_request_ago, Some(Duration::from_secs(60)));

        let second = activity.take_report(t0 + Duration::from_secs(120));
        assert_eq!(
            second.by_topic,
            vec![],
            "nothing new since the first report"
        );
        assert_eq!(second.total, 3, "the total is since start");
        assert_eq!(second.last_request_ago, Some(Duration::from_secs(120)));
        assert!(
            second
                .to_string()
                .contains("no requests since the last report")
        );
    }

    #[test]
    fn the_report_names_each_topic_with_its_count() {
        let t0 = Instant::now();
        let mut activity = Activity::new();
        activity.record("store", t0);
        let text = activity.take_report(t0).to_string();
        assert!(text.contains("store=1"), "{text}");
    }
}
