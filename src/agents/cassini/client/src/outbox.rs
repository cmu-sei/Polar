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

//! The facade's outbox: the messages it has numbered and the broker has not yet acknowledged
//! (docs/architecture/cassini-client-reliability.md, section 3, and build step 2).
//!
//! A message gets its `(epoch, seq)` when it enters the outbox and leaves only on its own acknowledgement, or when
//! the broker refuses it. Acknowledgements may arrive in any order, and one names its message alone: there is no
//! cumulative acknowledgement, which would let go of an earlier message the broker never accepted. What is left is
//! sent again, in order, whenever a transport registers. The outbox is bounded by count and by bytes; it always takes
//! one message when it is empty, so a message as large as a frame can be sent.

use cassini_types::{MAX_FRAME_BYTES, MessageId, WireTraceCtx};
use std::collections::BTreeMap;
use std::sync::Arc;

/// How much the outbox holds: so many messages, and so many bytes of payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxLimits {
    pub messages: usize,
    pub bytes: usize,
}

/// A thousand messages or one frame's bytes, as the load harness's producer's window (`test/common/src/schedule.rs`).
pub const DEFAULT_OUTBOX_LIMITS: OutboxLimits = OutboxLimits {
    messages: 1_000,
    bytes: MAX_FRAME_BYTES,
};

impl Default for OutboxLimits {
    fn default() -> Self {
        DEFAULT_OUTBOX_LIMITS
    }
}

/// A message the outbox holds.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: MessageId,
    pub topic: String,
    pub payload: Arc<Vec<u8>>,
    pub trace_ctx: Option<WireTraceCtx>,
    pub share_identity: bool,
}

/// The messages numbered in one epoch and not yet acknowledged, oldest first.
#[derive(Debug)]
pub struct Outbox {
    limits: OutboxLimits,
    epoch: u64,
    next_seq: u64,
    entries: BTreeMap<u64, Entry>,
    bytes: usize,
}

impl Outbox {
    /// An empty outbox numbering in `epoch` from 1.
    pub fn new(epoch: u64, limits: OutboxLimits) -> Self {
        Self {
            limits,
            epoch,
            next_seq: 1,
            entries: BTreeMap::new(),
            bytes: 0,
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn limits(&self) -> OutboxLimits {
        self.limits
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The payload bytes it holds.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether a message of `bytes` fits: under both limits, or into an empty outbox whatever its size.
    pub fn has_room(&self, bytes: usize) -> bool {
        self.entries.is_empty()
            || (self.entries.len() < self.limits.messages
                && self.bytes.saturating_add(bytes) <= self.limits.bytes)
    }

    /// Numbers a message and keeps it until it is acknowledged. The caller has asked `has_room`.
    pub fn push(
        &mut self,
        topic: String,
        payload: Arc<Vec<u8>>,
        trace_ctx: Option<WireTraceCtx>,
        share_identity: bool,
    ) -> &Entry {
        let id = MessageId {
            epoch: self.epoch,
            seq: self.next_seq,
        };
        self.next_seq += 1;
        self.bytes += payload.len();
        self.entries.entry(id.seq).or_insert(Entry {
            id,
            topic,
            payload,
            trace_ctx,
            share_identity,
        })
    }

    /// Lets go of the message numbered `id`, if the outbox holds it: its acknowledgement, or its refusal. Any other
    /// message stays, whatever its number.
    pub fn settle(&mut self, id: MessageId) -> Option<Entry> {
        if id.epoch != self.epoch {
            return None;
        }
        let entry = self.entries.remove(&id.seq)?;
        self.bytes -= entry.payload.len();
        Some(entry)
    }

    /// The message numbered `id`, if the outbox still holds it.
    pub fn get(&self, id: MessageId) -> Option<&Entry> {
        if id.epoch != self.epoch {
            return None;
        }
        self.entries.get(&id.seq)
    }

    /// What it holds, oldest first: what is sent again when a transport registers.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbox(messages: usize, bytes: usize) -> Outbox {
        Outbox::new(7, OutboxLimits { messages, bytes })
    }

    fn push(outbox: &mut Outbox, len: usize) -> MessageId {
        outbox
            .push("t".into(), Arc::new(vec![0; len]), None, true)
            .id
    }

    #[test]
    fn messages_are_numbered_one_two_three_in_the_epoch() {
        let mut o = outbox(10, 100);
        let ids: Vec<_> = (0..3).map(|_| push(&mut o, 1)).collect();
        assert_eq!(
            ids,
            [1, 2, 3].map(|seq| MessageId { epoch: 7, seq }).to_vec()
        );
    }

    #[test]
    fn an_acknowledgement_lets_go_of_its_own_message_only_in_any_order() {
        let mut o = outbox(10, 100);
        let ids: Vec<_> = (0..3).map(|_| push(&mut o, 1)).collect();
        assert!(o.settle(ids[2]).is_some());
        assert_eq!(
            o.entries().map(|e| e.id).collect::<Vec<_>>(),
            [ids[0], ids[1]]
        );
        assert!(o.settle(ids[2]).is_none(), "twice is once");
        assert!(
            o.settle(MessageId { epoch: 8, seq: 1 }).is_none(),
            "another epoch's"
        );
        assert_eq!(o.len(), 2);
    }

    #[test]
    fn it_is_bounded_by_count_and_bytes_and_an_empty_one_takes_anything() {
        let mut o = outbox(2, 10);
        assert!(
            o.has_room(1_000),
            "empty: a message larger than the bound fits"
        );
        push(&mut o, 6);
        assert!(o.has_room(4));
        assert!(!o.has_room(5), "over the bytes");
        push(&mut o, 1);
        assert!(!o.has_room(0), "over the count");
        assert_eq!(o.bytes(), 7);
    }
}
