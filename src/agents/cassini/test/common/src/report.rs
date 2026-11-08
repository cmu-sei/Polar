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

//! The lines a producer and a sink write. Each writes one result line when its part of a run is over, and a sink also
//! says when it is ready, once the broker has acknowledged its subscription; the script waits for every sink's before
//! it starts the producers. A result line is its mark and one line of JSON, which the script reads out of the pod's
//! log.

use crate::stats::Summary;
use serde::{Deserialize, Serialize};

/// Begins every result line.
pub const RESULT_MARK: &str = "HARNESS LOAD RESULT";
/// Begins the line a sink writes when the broker has acknowledged its subscription.
pub const READY_MARK: &str = "HARNESS LOAD READY";

/// What a producer did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducerReport {
    pub run: String,
    pub index: u64,
    pub topic: String,
    /// The payload's size, in bytes, without the header.
    pub size: u64,
    /// The rate asked for, messages a second; 0 is as fast as it could.
    pub rate: u64,
    pub count: u64,
    /// The window: at most this many messages sent and not yet acknowledged.
    pub window: u64,
    pub sent: u64,
    pub acknowledged: u64,
    /// Refused by the broker (or by the agent's own topic check).
    pub refused: u64,
    /// Not handed to the client at all.
    pub failed: u64,
    /// From the first message to the last acknowledgement (or to giving up on them).
    pub elapsed_secs: f64,
    /// Sent, over the time it took to send them.
    pub achieved_rate: f64,
}

/// What a sink received.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SinkReport {
    pub run: String,
    pub index: u64,
    pub topic: String,
    /// The producers it heard from, or should have.
    pub producers: Vec<u32>,
    pub expected: u64,
    pub received: u64,
    pub lost: u64,
    pub duplicates: u64,
    pub out_of_order: u64,
    pub unexpected: u64,
    /// The first this many of each producer's messages are left out of the latencies.
    pub warmup: u64,
    /// Nanoseconds from send to receipt, of the messages after the warm-up.
    pub latency_ns: Summary,
    /// From the first message received to the last.
    pub span_secs: f64,
    /// Messages received a second, over that span.
    pub messages_per_sec: f64,
    /// Payload bytes received a second, over that span.
    pub bytes_per_sec: f64,
    /// `complete`, or `idle` when it gave up waiting for the rest.
    pub ended: String,
}

/// A result line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Report {
    Producer(ProducerReport),
    Sink(SinkReport),
}

impl Report {
    /// The line to log.
    pub fn line(&self) -> String {
        format!(
            "{RESULT_MARK} {}",
            serde_json::to_string(self).expect("a report is plain data")
        )
    }

    /// The report in a logged line, if the line is a result line.
    pub fn parse(line: &str) -> Option<Self> {
        let at = line.find(RESULT_MARK)? + RESULT_MARK.len();
        serde_json::from_str(line[at..].trim()).ok()
    }
}

/// The line a sink logs once its subscription is acknowledged.
pub fn ready_line(run: &str, index: u64, topic: &str) -> String {
    format!("{READY_MARK} run={run} index={index} topic={topic}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sink(latency: Summary) -> Report {
        Report::Sink(SinkReport {
            run: "r".into(),
            index: 1,
            topic: "cassini.harness.lane.1".into(),
            producers: vec![1, 3],
            expected: 20,
            received: 19,
            lost: 1,
            duplicates: 0,
            out_of_order: 2,
            unexpected: 0,
            warmup: 1,
            latency_ns: latency,
            span_secs: 1.5,
            messages_per_sec: 12.6,
            bytes_per_sec: 3225.6,
            ended: "idle".into(),
        })
    }

    #[test]
    fn a_result_line_is_its_mark_and_one_line_of_json_naming_the_role() {
        let line = sink(Summary::default()).line();
        assert!(
            line.starts_with("HARNESS LOAD RESULT {\"role\":\"sink\""),
            "{line}"
        );
        assert!(!line.contains('\n'));
        assert_eq!(Report::parse("unrelated"), None);
    }

    proptest! {
        /// A report reads back from its line, wherever in a log line it is (after a timestamp, a level, colour).
        #[test]
        fn a_report_reads_back_from_its_line(
            prefix in "[ -~]{0,40}",
            count: u32, p50: u32, max: u32, quarters: u32,
        ) {
            // A mean exact in binary: JSON's text does not carry every f64 exactly, and that is not this property.
            let mean = quarters as f64 / 4.0;
            let report = sink(Summary { count: count as u64, p50: p50 as u64, p90: p50 as u64, p99: max as u64, max: max as u64, mean });
            prop_assume!(!prefix.contains(RESULT_MARK));
            prop_assert_eq!(Report::parse(&format!("{prefix}{}", report.line())), Some(report));
        }
    }
}
