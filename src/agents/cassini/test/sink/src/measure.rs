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

//! What a sink makes of what arrives, apart from the actor: each message's stamp and when it arrived go in, and a
//! report comes out. Latency is the receipt time less the send time, both by the wall clock, so it is right only when
//! the producer and the sink share a clock (one node); the script says when they do not.

use harness_common::envelope::Stamp;
use harness_common::report::SinkReport;
use harness_common::settings::Run;
use harness_common::stats::{Seen, Summary, Tally};
use std::time::Instant;

pub struct Measure {
    producers: Vec<u32>,
    warmup: u64,
    tally: Tally,
    latencies: Vec<u64>,
    bytes: u64,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Measure {
    /// For a sink that should hear `count` messages from each of `producers`, the first `warmup` of each left out of
    /// the latencies.
    pub fn new(producers: Vec<u32>, count: u64, warmup: u64) -> Self {
        Self {
            tally: Tally::new(&producers, count),
            producers,
            warmup,
            latencies: Vec::new(),
            bytes: 0,
            first: None,
            last: None,
        }
    }

    /// A message stamped `stamp` arrived at `at_ns` (the wall clock) and `at` (the monotonic one).
    pub fn receive(&mut self, stamp: Stamp, at_ns: u64, at: Instant) {
        if self.tally.record(stamp.producer, stamp.seqno) != Seen::New {
            return;
        }
        if stamp.seqno >= self.warmup {
            self.latencies.push(at_ns.saturating_sub(stamp.sent_at_ns));
        }
        self.bytes += stamp.payload_len as u64;
        self.first.get_or_insert(at);
        self.last = Some(at);
    }

    pub fn complete(&self) -> bool {
        self.tally.complete()
    }

    /// When the last new message arrived.
    pub fn last(&self) -> Option<Instant> {
        self.last
    }

    /// The report, the sink having ended as `ended` (`complete` or `idle`).
    pub fn report(&self, run: &Run, ended: &str) -> SinkReport {
        let span = match (self.first, self.last) {
            (Some(first), Some(last)) => last.duration_since(first).as_secs_f64(),
            _ => 0.0,
        };
        let per_sec = |n: u64| if span > 0.0 { n as f64 / span } else { 0.0 };
        SinkReport {
            run: run.id.clone(),
            index: run.index,
            topic: run.topic().to_string(),
            producers: self.producers.clone(),
            expected: self.tally.expected(),
            received: self.tally.unique,
            lost: self.tally.lost(),
            duplicates: self.tally.duplicates,
            out_of_order: self.tally.out_of_order,
            unexpected: self.tally.unexpected,
            warmup: self.warmup,
            latency_ns: Summary::of(self.latencies.clone()),
            span_secs: span,
            messages_per_sec: per_sec(self.tally.unique),
            bytes_per_sec: per_sec(self.bytes),
            ended: ended.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn run() -> Run {
        Run {
            id: "r".into(),
            index: 0,
            producers: 2,
            lanes: 1,
            count: 3,
        }
    }

    fn stamp(producer: u32, seqno: u64, sent_at_ns: u64) -> Stamp {
        Stamp {
            producer,
            seqno,
            sent_at_ns,
            payload_len: 100,
        }
    }

    #[test]
    fn the_warm_up_is_received_but_not_timed_and_a_repeat_is_neither() {
        let start = Instant::now();
        let mut m = Measure::new(vec![0, 1], 3, 1);
        m.receive(stamp(0, 0, 1_000), 9_000, start);
        m.receive(stamp(0, 1, 1_000), 2_000, start + Duration::from_secs(1));
        m.receive(stamp(0, 1, 1_000), 5_000, start + Duration::from_secs(1));
        m.receive(stamp(1, 1, 1_000), 4_000, start + Duration::from_secs(2));
        let r = m.report(&run(), "idle");
        assert_eq!((r.expected, r.received, r.lost, r.duplicates), (6, 3, 3, 1));
        assert_eq!(r.latency_ns.count, 2, "the warm-up message is not timed");
        assert_eq!((r.latency_ns.p50, r.latency_ns.max), (1_000, 3_000));
        assert_eq!(r.span_secs, 2.0);
        assert_eq!(r.messages_per_sec, 1.5);
        assert_eq!(r.bytes_per_sec, 150.0);
        assert!(!m.complete());
    }

    #[test]
    fn a_clock_behind_the_producer_s_is_no_latency_not_a_wrap() {
        let mut m = Measure::new(vec![0], 1, 0);
        m.receive(stamp(0, 0, 5_000), 1_000, Instant::now());
        assert_eq!(m.report(&run(), "complete").latency_ns.max, 0);
        assert!(m.complete());
    }

    #[test]
    fn nothing_received_reports_nothing() {
        let r = Measure::new(vec![0], 5, 0).report(&run(), "idle");
        assert_eq!(
            (r.received, r.lost, r.span_secs, r.messages_per_sec),
            (0, 5, 0.0, 0.0)
        );
        assert_eq!(r.topic, "cassini.harness.lane.0");
    }
}
