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

//! What a sink measures. Latencies are kept, every one, and summarised at the end (a run's few million samples are a
//! few tens of megabytes); what arrived is tallied per producer, so a lost, repeated or reordered message is counted
//! exactly, not estimated.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The nearest-rank `p`th percentile (0 < `p` <= 100) of `sorted`, which must be in ascending order: the smallest
/// sample with at least `p` percent of the samples at or below it. `None` when there are no samples.
pub fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// A summary of latencies, in nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub count: u64,
    pub p50: u64,
    pub p90: u64,
    pub p99: u64,
    pub max: u64,
    pub mean: f64,
}

impl Summary {
    /// The summary of `samples`, in any order. All zero when there are none.
    pub fn of(mut samples: Vec<u64>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_unstable();
        let at = |p| percentile(&samples, p).expect("not empty");
        Self {
            count: samples.len() as u64,
            p50: at(50.0),
            p90: at(90.0),
            p99: at(99.0),
            max: *samples.last().expect("not empty"),
            mean: samples.iter().map(|s| *s as f64).sum::<f64>() / samples.len() as f64,
        }
    }
}

/// What became of one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// The first time this message arrived.
    New,
    /// It had arrived before.
    Duplicate,
    /// No producer this sink hears from sent it, or its sequence number is past the run's count.
    Unexpected,
}

/// What a sink has received, against what it should receive: `count` messages, numbered from 0, from each of its
/// producers.
#[derive(Debug, Clone)]
pub struct Tally {
    count: u64,
    seen: BTreeMap<u32, Vec<u64>>,
    highest: BTreeMap<u32, u64>,
    pub deliveries: u64,
    pub unique: u64,
    pub duplicates: u64,
    pub out_of_order: u64,
    pub unexpected: u64,
}

impl Tally {
    pub fn new(producers: &[u32], count: u64) -> Self {
        let words = count.div_ceil(64) as usize;
        Self {
            count,
            seen: producers.iter().map(|p| (*p, vec![0; words])).collect(),
            highest: BTreeMap::new(),
            deliveries: 0,
            unique: 0,
            duplicates: 0,
            out_of_order: 0,
            unexpected: 0,
        }
    }

    /// Counts a delivery of `producer`'s `seqno`. A new message that arrives after a later one from the same producer
    /// is out of order.
    pub fn record(&mut self, producer: u32, seqno: u64) -> Seen {
        self.deliveries += 1;
        let Some(bits) = self.seen.get_mut(&producer).filter(|_| seqno < self.count) else {
            self.unexpected += 1;
            return Seen::Unexpected;
        };
        let (word, bit) = ((seqno / 64) as usize, 1u64 << (seqno % 64));
        if bits[word] & bit != 0 {
            self.duplicates += 1;
            return Seen::Duplicate;
        }
        bits[word] |= bit;
        self.unique += 1;
        let highest = self.highest.entry(producer).or_insert(seqno);
        if seqno < *highest {
            self.out_of_order += 1;
        }
        *highest = (*highest).max(seqno);
        Seen::New
    }

    /// How many messages this sink should receive in all.
    pub fn expected(&self) -> u64 {
        self.count * self.seen.len() as u64
    }

    /// How many it has not received.
    pub fn lost(&self) -> u64 {
        self.expected() - self.unique
    }

    pub fn complete(&self) -> bool {
        self.unique == self.expected()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let s: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&s, 50.0), Some(5));
        assert_eq!(percentile(&s, 90.0), Some(9));
        assert_eq!(percentile(&s, 99.0), Some(10));
        assert_eq!(percentile(&s, 100.0), Some(10));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn a_summary_of_nothing_is_zero() {
        assert_eq!(Summary::of(vec![]), Summary::default());
        let s = Summary::of(vec![30, 10, 20]);
        assert_eq!((s.count, s.p50, s.max, s.mean), (3, 20, 30, 20.0));
    }

    #[test]
    fn a_tally_counts_each_kind_of_delivery() {
        let mut t = Tally::new(&[0, 2], 3);
        assert_eq!(t.record(0, 1), Seen::New);
        assert_eq!(t.record(0, 0), Seen::New);
        assert_eq!(t.record(0, 0), Seen::Duplicate);
        assert_eq!(t.record(1, 0), Seen::Unexpected);
        assert_eq!(t.record(2, 3), Seen::Unexpected);
        assert_eq!(
            (
                t.deliveries,
                t.unique,
                t.duplicates,
                t.out_of_order,
                t.unexpected
            ),
            (5, 2, 1, 1, 2)
        );
        assert_eq!((t.expected(), t.lost(), t.complete()), (6, 4, false));
    }

    proptest! {
        /// A percentile is one of the samples, at least `p` percent of the samples are at or below it and fewer are
        /// below it, and a higher percentile is never smaller.
        #[test]
        fn a_percentile_is_the_nearest_rank(
            mut samples in prop::collection::vec(0u64..1000, 1..200),
            p in 0.1f64..=100.0,
            q in 0.1f64..=100.0,
        ) {
            samples.sort_unstable();
            let v = percentile(&samples, p).unwrap();
            prop_assert!(samples.contains(&v));
            let needed = ((p / 100.0) * samples.len() as f64).ceil().max(1.0) as usize;
            prop_assert!(samples.iter().filter(|s| **s <= v).count() >= needed);
            prop_assert!(samples.iter().filter(|s| **s < v).count() < needed);
            let (lo, hi) = if p <= q { (p, q) } else { (q, p) };
            prop_assert!(percentile(&samples, lo) <= percentile(&samples, hi));
            prop_assert_eq!(percentile(&samples, 100.0), samples.last().copied());
        }

        /// Whatever arrives, in whatever order: every delivery is new, a duplicate or unexpected; what was received
        /// and what was lost make up what was expected; and what was received, lost and repeated do not depend on the
        /// order.
        #[test]
        fn a_tally_accounts_for_every_delivery_in_any_order(
            producers in prop::collection::btree_set(0u32..6, 0..4),
            count in 0u64..130,
            deliveries in prop::collection::vec((0u32..8, 0u64..140), 0..300),
            seed: u64,
        ) {
            let producers: Vec<u32> = producers.into_iter().collect();
            let tally = |order: &[(u32, u64)]| {
                let mut t = Tally::new(&producers, count);
                for (p, s) in order {
                    t.record(*p, *s);
                }
                t
            };
            let t = tally(&deliveries);
            prop_assert_eq!(t.deliveries, deliveries.len() as u64);
            prop_assert_eq!(t.deliveries, t.unique + t.duplicates + t.unexpected);
            prop_assert_eq!(t.unique + t.lost(), count * producers.len() as u64);
            prop_assert!(t.out_of_order <= t.unique);

            let mut shuffled = deliveries.clone();
            let mut state = seed | 1;
            for i in (1..shuffled.len()).rev() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                shuffled.swap(i, (state % (i as u64 + 1)) as usize);
            }
            let u = tally(&shuffled);
            prop_assert_eq!((u.unique, u.lost(), u.duplicates, u.unexpected), (t.unique, t.lost(), t.duplicates, t.unexpected));
        }

        /// Every message once, each producer's in order: complete, with nothing repeated or out of order.
        #[test]
        fn everything_once_and_in_order_is_complete(
            producers in prop::collection::btree_set(0u32..6, 0..4),
            count in 0u64..130,
        ) {
            let producers: Vec<u32> = producers.into_iter().collect();
            let mut t = Tally::new(&producers, count);
            for s in 0..count {
                for p in &producers {
                    prop_assert_eq!(t.record(*p, s), Seen::New);
                }
            }
            prop_assert!(t.complete());
            prop_assert_eq!((t.lost(), t.duplicates, t.out_of_order, t.unexpected), (0, 0, 0, 0));
        }
    }
}
