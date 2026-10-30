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

//! The client's credit (docs/architecture/cassini-backpressure.md, phase 1, client side, with the outbox of phase 2):
//! what the facade's outbox and its buffer hold, counted where a caller can see it.
//!
//! A publish takes its room before it is handed to the facade: in the outbox, when there is room and nothing waits in
//! the buffer before it; otherwise the caller's `OfflineBehavior` decides: `Queue` takes room in the buffer (and is
//! refused, the newest, when the buffer is full), `Fail` is refused as busy, `Drop` is discarded and counted. The
//! facade gives room back as the broker acknowledges what the outbox holds, and moves what waits in the buffer into
//! the room made. A caller that can await waits for room instead (`TcpClient::publish_when_ready`).

use crate::outbox::{DEFAULT_OUTBOX_LIMITS, OutboxLimits};
use cassini_types::MAX_FRAME_BYTES;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Notify;

/// How many publishes wait in the buffer, by count and by bytes, when the outbox is full or the client is offline.
pub const DEFAULT_BUFFER_LIMITS: OutboxLimits = OutboxLimits {
    messages: crate::facade::DEFAULT_OFFLINE_BUFFER,
    bytes: MAX_FRAME_BYTES,
};

/// Where a publish was given room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Outbox,
    Buffer,
}

/// Why a publish was given no room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRoom {
    /// The outbox is full and the caller does not queue.
    Busy,
    /// The outbox and the buffer are full: the newest is refused.
    BufferFull,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    outbox: usize,
    outbox_bytes: usize,
    buffer: usize,
    buffer_bytes: usize,
}

/// What the outbox and the buffer hold, shared by the facade and whoever publishes through it.
#[derive(Debug)]
pub struct Credit {
    outbox: OutboxLimits,
    buffer: OutboxLimits,
    counts: Mutex<Counts>,
    room: Notify,
    dropped: AtomicU64,
    refused: AtomicU64,
}

impl Default for Credit {
    fn default() -> Self {
        Self::new(DEFAULT_OUTBOX_LIMITS, DEFAULT_BUFFER_LIMITS)
    }
}

fn fits(limits: OutboxLimits, count: usize, bytes: usize, more: usize) -> bool {
    count == 0 || (count < limits.messages && bytes.saturating_add(more) <= limits.bytes)
}

impl Credit {
    pub fn new(outbox: OutboxLimits, buffer: OutboxLimits) -> Self {
        Self {
            outbox,
            buffer,
            counts: Mutex::new(Counts::default()),
            room: Notify::new(),
            dropped: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        }
    }

    pub fn outbox_limits(&self) -> OutboxLimits {
        self.outbox
    }

    /// Takes room in the outbox for a publish of `bytes`, if there is room and nothing waits in the buffer.
    pub fn try_outbox(&self, bytes: usize) -> bool {
        let mut counts = self.counts.lock().unwrap();
        if counts.buffer == 0 && fits(self.outbox, counts.outbox, counts.outbox_bytes, bytes) {
            counts.outbox += 1;
            counts.outbox_bytes += bytes;
            return true;
        }
        false
    }

    /// Takes room for a publish of `bytes`: in the outbox if it can, otherwise, if `queue`, in the buffer.
    pub fn take(&self, bytes: usize, queue: bool) -> Result<Slot, NoRoom> {
        let mut counts = self.counts.lock().unwrap();
        if counts.buffer == 0 && fits(self.outbox, counts.outbox, counts.outbox_bytes, bytes) {
            counts.outbox += 1;
            counts.outbox_bytes += bytes;
            return Ok(Slot::Outbox);
        }
        if !queue {
            return Err(NoRoom::Busy);
        }
        if fits(self.buffer, counts.buffer, counts.buffer_bytes, bytes) {
            counts.buffer += 1;
            counts.buffer_bytes += bytes;
            return Ok(Slot::Buffer);
        }
        drop(counts);
        self.refused.fetch_add(1, Ordering::Relaxed);
        Err(NoRoom::BufferFull)
    }

    /// Gives back the outbox room of a publish of `bytes`: the broker acknowledged or refused it.
    pub fn settle(&self, bytes: usize) {
        let mut counts = self.counts.lock().unwrap();
        counts.outbox = counts.outbox.saturating_sub(1);
        counts.outbox_bytes = counts.outbox_bytes.saturating_sub(bytes);
        drop(counts);
        self.room.notify_waiters();
    }

    /// Gives back the room a publish of `bytes` took in `slot` without using it (it was refused before it was kept).
    pub fn release(&self, slot: Slot, bytes: usize) {
        match slot {
            Slot::Outbox => self.settle(bytes),
            Slot::Buffer => {
                let mut counts = self.counts.lock().unwrap();
                counts.buffer = counts.buffer.saturating_sub(1);
                counts.buffer_bytes = counts.buffer_bytes.saturating_sub(bytes);
                drop(counts);
                self.room.notify_waiters();
            }
        }
    }

    /// Moves a publish of `bytes` from the buffer into the outbox, if the outbox has room for it.
    pub fn promote(&self, bytes: usize) -> bool {
        let mut counts = self.counts.lock().unwrap();
        if !fits(self.outbox, counts.outbox, counts.outbox_bytes, bytes) {
            return false;
        }
        counts.buffer = counts.buffer.saturating_sub(1);
        counts.buffer_bytes = counts.buffer_bytes.saturating_sub(bytes);
        counts.outbox += 1;
        counts.outbox_bytes += bytes;
        drop(counts);
        self.room.notify_waiters();
        true
    }

    /// Waits until a publish of `bytes` can take room in the outbox, and takes it.
    pub async fn wait_for_outbox(&self, bytes: usize) {
        loop {
            let notified = self.room.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.try_outbox(bytes) {
                return;
            }
            notified.await;
        }
    }

    /// Counts a publish discarded because the caller chose to drop it rather than wait.
    pub fn count_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Publishes discarded (`Drop` at the bound) since the client started.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Publishes refused because the buffer was full, since the client started.
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    /// What the outbox and the buffer hold: (outbox messages, outbox bytes, buffer messages, buffer bytes).
    pub fn held(&self) -> (usize, usize, usize, usize) {
        let counts = self.counts.lock().unwrap();
        (
            counts.outbox,
            counts.outbox_bytes,
            counts.buffer,
            counts.buffer_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn credit(outbox: usize, buffer: usize) -> Credit {
        Credit::new(
            OutboxLimits {
                messages: outbox,
                bytes: usize::MAX,
            },
            OutboxLimits {
                messages: buffer,
                bytes: usize::MAX,
            },
        )
    }

    #[test]
    fn the_outbox_first_then_the_buffer_then_the_newest_is_refused() {
        let credit = credit(1, 1);
        assert_eq!(credit.take(1, true), Ok(Slot::Outbox));
        assert_eq!(credit.take(1, false), Err(NoRoom::Busy));
        assert_eq!(credit.take(1, true), Ok(Slot::Buffer));
        assert_eq!(credit.take(1, true), Err(NoRoom::BufferFull));
        assert_eq!(credit.refused(), 1);
        credit.settle(1);
        assert!(
            !credit.try_outbox(1),
            "what waits in the buffer goes before anything newer"
        );
        assert!(credit.promote(1));
        assert_eq!(credit.held(), (1, 1, 0, 0));
    }

    #[tokio::test]
    async fn a_caller_waiting_for_room_is_woken_when_the_broker_acknowledges() {
        let credit = Arc::new(credit(1, 0));
        assert!(credit.try_outbox(1));
        let waiting = tokio::spawn({
            let credit = credit.clone();
            async move { credit.wait_for_outbox(1).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        credit.settle(1);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("woken")
            .unwrap();
        assert_eq!(credit.held().0, 1);
    }
}
