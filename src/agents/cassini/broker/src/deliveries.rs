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

//! A subscriber's delivery outbox (docs/architecture/cassini-client-reliability.md, section 5, and build step 5): every
//! message the subscriber is given, numbered, from when it is given until its client acknowledges it. What a client
//! did not acknowledge before its connection went is sent again when its session is resumed, and handed to the
//! session that takes its identity over.

use cassini_types::HeldDelivery;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DELIVERY_ID: AtomicU64 = AtomicU64::new(1);

/// A number for a delivery no other delivery of this broker has: what the client returns in `PublishResponseAck`, and
/// what its Listener finds the subscriber by.
pub fn next_delivery_id() -> u64 {
    NEXT_DELIVERY_ID.fetch_add(1, Ordering::Relaxed)
}

/// The deliveries a subscriber holds, oldest first, and the payload bytes they come to.
#[derive(Debug, Default)]
pub struct DeliveryOutbox {
    entries: BTreeMap<u64, HeldDelivery>,
    bytes: usize,
}

impl DeliveryOutbox {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The payload bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Keeps `delivery` under `delivery_id` until it is acknowledged.
    pub fn hold(&mut self, delivery_id: u64, delivery: HeldDelivery) {
        self.bytes += delivery.payload.len();
        if let Some(replaced) = self.entries.insert(delivery_id, delivery) {
            self.bytes -= replaced.payload.len();
        }
    }

    /// The client has the delivery numbered `delivery_id`: it is let go. A number it does not hold changes nothing.
    pub fn acknowledge(&mut self, delivery_id: u64) -> Option<HeldDelivery> {
        let delivery = self.entries.remove(&delivery_id)?;
        self.bytes -= delivery.payload.len();
        Some(delivery)
    }

    /// What is held, oldest first: what is sent again to a resumed session.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &HeldDelivery)> {
        self.entries.iter()
    }

    /// Everything held, oldest first, leaving it empty: what a superseded session hands over.
    pub fn take(&mut self) -> Vec<HeldDelivery> {
        self.bytes = 0;
        std::mem::take(&mut self.entries).into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery(len: usize) -> HeldDelivery {
        HeldDelivery {
            payload: vec![0; len].into(),
            published_by: None,
            published_instance: None,
            origin: None,
        }
    }

    #[test]
    fn a_delivery_is_held_until_its_own_acknowledgement_in_any_order() {
        let mut outbox = DeliveryOutbox::default();
        for (id, len) in [(1, 3), (2, 5), (3, 7)] {
            outbox.hold(id, delivery(len));
        }
        assert_eq!(outbox.bytes(), 15);
        assert!(outbox.acknowledge(2).is_some());
        assert!(outbox.acknowledge(2).is_none(), "twice is once");
        assert!(outbox.acknowledge(9).is_none(), "one it never held");
        assert_eq!(outbox.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(outbox.bytes(), 10);
        assert_eq!(
            outbox
                .take()
                .iter()
                .map(|d| d.payload.len())
                .collect::<Vec<_>>(),
            [3, 7],
            "handed over oldest first"
        );
        assert!(outbox.is_empty());
        assert_eq!(outbox.bytes(), 0);
    }

    #[test]
    fn delivery_numbers_are_never_reused() {
        let a = next_delivery_id();
        let b = next_delivery_id();
        assert!(b > a);
    }
}
