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

//! What the broker holds, and what it counts of it (docs/architecture/cassini-backpressure.md, phase 1).
//!
//! A payload carries its charge against the broker's memory budget: the broker wraps it once, when its reader admits
//! the frame, every hop after shares the one `Arc`, and the last holder to let it go refunds the charge, so a message
//! fanned out to many subscribers is counted once and nothing can forget to refund. The budget itself is the
//! broker's (`cassini_broker::budget`); these are the parts the broker's messages carry.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Something held for as long as what carries it is, and let go with it: a charge, or a connection's credit.
pub struct Hold(Option<Box<dyn Send + Sync>>);

impl Hold {
    pub fn new(held: impl Send + Sync + 'static) -> Self {
        Hold(Some(Box::new(held)))
    }

    /// Nothing held.
    pub fn none() -> Self {
        Hold(None)
    }

    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }

    /// Lets go of it now.
    pub fn release(self) {}
}

impl Default for Hold {
    fn default() -> Self {
        Hold::none()
    }
}

impl std::fmt::Debug for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Hold(held)"
        } else {
            "Hold(none)"
        })
    }
}

struct Charged {
    bytes: Arc<Vec<u8>>,
    _charge: Hold,
}

/// A payload as the broker holds it: its bytes, and the charge it took against the broker's memory budget, refunded
/// when the last clone is dropped. Cloning shares it; nothing copies the bytes.
#[derive(Clone)]
pub struct Payload(Arc<Charged>);

impl Payload {
    /// A payload that took no charge: the broker's own, or an empty one.
    pub fn uncharged(bytes: Arc<Vec<u8>>) -> Self {
        Payload(Arc::new(Charged {
            bytes,
            _charge: Hold::none(),
        }))
    }

    /// A payload carrying `charge` until the last holder lets it go.
    pub fn charged(bytes: Arc<Vec<u8>>, charge: Hold) -> Self {
        Payload(Arc::new(Charged {
            bytes,
            _charge: charge,
        }))
    }

    pub fn bytes(&self) -> &Arc<Vec<u8>> {
        &self.0.bytes
    }

    pub fn len(&self) -> usize {
        self.0.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.bytes.is_empty()
    }

    /// How many hold this payload (its clones), for tests.
    pub fn holders(&self) -> usize {
        Arc::strong_count(&self.0)
    }
}

impl std::ops::Deref for Payload {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0.bytes
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Payload({} bytes)", self.len())
    }
}

impl PartialEq for Payload {
    fn eq(&self, other: &Self) -> bool {
        self.0.bytes == other.0.bytes
    }
}

impl From<Arc<Vec<u8>>> for Payload {
    fn from(bytes: Arc<Vec<u8>>) -> Self {
        Payload::uncharged(bytes)
    }
}

impl From<Vec<u8>> for Payload {
    fn from(bytes: Vec<u8>) -> Self {
        Payload::uncharged(Arc::new(bytes))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One subscriber's backlog: the bytes delivered to its client and not yet acknowledged (its delivery outbox), with a
/// high-water mark and hysteresis. Its subscriber writes it; its topic reads it, and accepts no publish while a
/// connected subscriber of it is above its high water, until that subscriber falls below its low water (half the
/// high). A subscriber that is not connected holds nothing back.
#[derive(Debug)]
pub struct Backlog {
    name: String,
    high: usize,
    low: usize,
    bytes: AtomicUsize,
    above: AtomicBool,
    /// When it went above its high water, in milliseconds since the epoch; zero when it is not above.
    above_since_ms: AtomicU64,
    connected: AtomicBool,
}

impl Backlog {
    /// `name` says whose it is (a subscriber's `<registration id>:<topic>`); `high_water` is in bytes.
    pub fn new(name: impl Into<String>, high_water: usize) -> Self {
        Backlog {
            name: name.into(),
            high: high_water,
            low: high_water / 2,
            bytes: AtomicUsize::new(0),
            above: AtomicBool::new(false),
            above_since_ms: AtomicU64::new(0),
            connected: AtomicBool::new(true),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::SeqCst)
    }

    pub fn high_water(&self) -> usize {
        self.high
    }

    /// Counts `n` more bytes held. True if that took it above its high water.
    pub fn add(&self, n: usize) -> bool {
        let now = self.bytes.fetch_add(n, Ordering::SeqCst) + n;
        if now > self.high && !self.above.swap(true, Ordering::SeqCst) {
            self.above_since_ms.store(now_ms().max(1), Ordering::SeqCst);
            return true;
        }
        false
    }

    /// Counts `n` fewer bytes held. True if that took it from above its high water to below its low water: its
    /// topic may move again.
    pub fn remove(&self, n: usize) -> bool {
        let before = self
            .bytes
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |b| {
                Some(b.saturating_sub(n))
            })
            .unwrap_or(0);
        let now = before.saturating_sub(n);
        if now < self.low.max(1) && self.above.swap(false, Ordering::SeqCst) {
            self.above_since_ms.store(0, Ordering::SeqCst);
            return true;
        }
        false
    }

    /// Above its high water (and not yet back below its low water).
    pub fn is_above(&self) -> bool {
        self.above.load(Ordering::SeqCst)
    }

    /// How long it has been above its high water, if it is.
    pub fn above_for(&self) -> Option<Duration> {
        match self.above_since_ms.load(Ordering::SeqCst) {
            0 => None,
            since => Some(Duration::from_millis(now_ms().saturating_sub(since))),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    /// Whether its topic must wait for it: connected and above its high water.
    pub fn holds_back(&self) -> bool {
        self.is_connected() && self.is_above()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_s_charge_is_let_go_with_its_last_clone() {
        struct Flag(Arc<AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let payload = Payload::charged(Arc::new(vec![1, 2, 3]), Hold::new(Flag(dropped.clone())));
        let copies: Vec<Payload> = (0..4).map(|_| payload.clone()).collect();
        assert_eq!(payload.holders(), 5);
        drop(payload);
        drop(copies[..3].to_vec());
        assert!(!dropped.load(Ordering::SeqCst), "one clone still holds it");
        drop(copies);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn a_backlog_holds_its_topic_back_from_above_high_water_to_below_low_water() {
        let backlog = Backlog::new("r:t", 100);
        assert!(!backlog.add(100), "at the mark is not above it");
        assert!(backlog.add(1));
        assert!(backlog.holds_back());
        assert!(backlog.above_for().is_some());
        assert!(!backlog.remove(51), "50 is not below the low water");
        assert!(backlog.holds_back());
        assert!(backlog.remove(1), "49 is");
        assert!(!backlog.holds_back());
        assert_eq!(backlog.above_for(), None);
        backlog.add(200);
        backlog.set_connected(false);
        assert!(
            !backlog.holds_back(),
            "a subscriber away holds nothing back"
        );
    }
}
