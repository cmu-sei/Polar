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

use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    #[test]
    fn payload_survives_arc_wrap(bytes in any::<Vec<u8>>()) {
        let payload = Arc::new(bytes.clone());
        prop_assert_eq!(payload.as_ref(), &bytes);
        prop_assert_eq!(payload.len(), bytes.len());
    }

    #[test]
    fn empty_payload_arc(topic in "[a-z]{1,20}") {
        let payload: Arc<Vec<u8>> = Arc::new(vec![]);
        prop_assert_eq!(payload.len(), 0);
        prop_assert!(!topic.is_empty() && topic.len() <= 20);
    }
}

// ===== The memory budget, as a model (docs/architecture/cassini-backpressure.md, "Tests, first") =====

/// What happens to the payloads a broker holds: a reader admits one (if the budget gives its bytes), a topic fans one
/// admitted out to some holders (subscribers' outboxes, a topic's queue) and lets go of its own, a holder lets go of
/// one (acknowledged), of its oldest (expired) or of everything (its client went), or a reader drops what it admitted
/// (a refused publish).
#[derive(Debug, Clone)]
enum Event {
    Admit(usize),
    FanOut { nth: usize, holders: Vec<usize> },
    Acknowledge { holder: usize, nth: usize },
    Expire(usize),
    Disconnect(usize),
    Refuse(usize),
}

const HOLDERS: usize = 4;
const LIMIT: usize = 1_000;

fn events() -> impl Strategy<Value = Vec<Event>> {
    prop::collection::vec(
        prop_oneof![
            (0usize..400).prop_map(Event::Admit),
            (0usize..8, prop::collection::vec(0..HOLDERS, 0..HOLDERS))
                .prop_map(|(nth, holders)| Event::FanOut { nth, holders }),
            (0..HOLDERS, 0usize..8).prop_map(|(holder, nth)| Event::Acknowledge { holder, nth }),
            (0..HOLDERS).prop_map(Event::Expire),
            (0..HOLDERS).prop_map(Event::Disconnect),
            (0usize..8).prop_map(Event::Refuse),
        ],
        0..120,
    )
}

proptest! {
    /// For any interleaving of admissions, fan-outs, acknowledgements, expiries, disconnections and refusals: the
    /// bytes held never exceed the budget and are exactly those of the messages something still holds (a message fanned
    /// out to many holders counted once); each message's charge is taken once and refunded once, when its last holder
    /// lets go; and once everything is let go the budget holds nothing.
    #[test]
    fn the_budget_holds_each_message_once_and_never_more_than_its_limit(events in events()) {
        use cassini_broker::budget::Budget;
        use cassini_types::{Hold, Payload};
        use std::collections::BTreeMap;

        let budget = Budget::new(LIMIT);
        let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
        let mut in_flight: Vec<(usize, Payload)> = Vec::new();
        let mut holders: Vec<Vec<(usize, Payload)>> = vec![Vec::new(); HOLDERS];
        for event in events {
            match event {
                Event::Admit(size) => {
                    if let Some(charge) = budget.try_charge(size) {
                        let id = sizes.len();
                        sizes.insert(id, size);
                        in_flight.push((id, Payload::charged(Arc::new(vec![0; size]), Hold::new(charge))));
                    }
                }
                Event::FanOut { nth, holders: to } => {
                    if !in_flight.is_empty() {
                        let (id, payload) = in_flight.remove(nth % in_flight.len());
                        for holder in to {
                            holders[holder].push((id, payload.clone()));
                        }
                    }
                }
                Event::Acknowledge { holder, nth } => {
                    if !holders[holder].is_empty() {
                        let len = holders[holder].len();
                        holders[holder].remove(nth % len);
                    }
                }
                Event::Expire(holder) => {
                    if !holders[holder].is_empty() {
                        holders[holder].remove(0);
                    }
                }
                Event::Disconnect(holder) => holders[holder].clear(),
                Event::Refuse(nth) => {
                    if !in_flight.is_empty() {
                        let len = in_flight.len();
                        in_flight.remove(nth % len);
                    }
                }
            }
            let live: std::collections::BTreeSet<usize> = in_flight
                .iter()
                .chain(holders.iter().flatten())
                .map(|(id, _)| *id)
                .collect();
            let stats = budget.stats();
            prop_assert!(stats.used <= LIMIT, "{} held over a limit of {}", stats.used, LIMIT);
            prop_assert_eq!(stats.used, live.iter().map(|id| sizes[id]).sum::<usize>());
            prop_assert_eq!(stats.charges as usize, sizes.len(), "one charge a message admitted");
            prop_assert_eq!(
                (stats.charges - stats.refunds) as usize,
                live.len(),
                "a charge outstanding for each message still held, and only for those"
            );
        }
        drop(in_flight);
        drop(holders);
        let stats = budget.stats();
        prop_assert_eq!(stats.used, 0);
        prop_assert_eq!(stats.charges, stats.refunds);
    }
}
