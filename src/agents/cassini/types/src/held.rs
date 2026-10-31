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
    /// The bytes it is counted as: its frame's, which it was charged, or its own if it took no charge.
    weight: usize,
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
            weight: bytes.len(),
            bytes,
            _charge: Hold::none(),
        }))
    }

    /// A payload carrying `charge`, of `weight` bytes (its frame's), until the last holder lets it go.
    pub fn charged(bytes: Arc<Vec<u8>>, charge: Hold, weight: usize) -> Self {
        Payload(Arc::new(Charged {
            weight: weight.max(bytes.len()),
            bytes,
            _charge: charge,
        }))
    }

    /// The bytes it counts for against the budget and a subscriber's high water: the frame it came in, which is more
    /// than its payload by the envelope (for a small message, many times more).
    pub fn weight(&self) -> usize {
        self.0.weight
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
    /// When it last made progress (an acknowledgement, or a return), or began to hold anything, in milliseconds since
    /// the epoch.
    progress_ms: AtomicU64,
    connected: AtomicBool,
    /// Whose it is, when it is a subscriber's: its registration id and topic.
    owner: Option<(String, String)>,
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
            progress_ms: AtomicU64::new(now_ms()),
            connected: AtomicBool::new(true),
            owner: None,
        }
    }

    /// The backlog of the subscriber of the session `registration_id` on `topic`, named `<registration id>:<topic>`.
    pub fn subscriber(registration_id: &str, topic: &str, high_water: usize) -> Self {
        Backlog {
            owner: Some((registration_id.to_string(), topic.to_string())),
            ..Self::new(format!("{registration_id}:{topic}"), high_water)
        }
    }

    /// Whose it is: a subscriber's registration id and topic.
    pub fn owner(&self) -> Option<(&str, &str)> {
        self.owner.as_ref().map(|(r, t)| (r.as_str(), t.as_str()))
    }

    /// How long a connected subscriber holding anything has gone without progress (no acknowledgement, no return):
    /// what the broker's watchdog ends a connection for while the budget is exhausted.
    pub fn stalled_for(&self) -> Option<Duration> {
        if !self.is_connected() || self.bytes() == 0 {
            return None;
        }
        let since = self.progress_ms.load(Ordering::SeqCst);
        Some(Duration::from_millis(now_ms().saturating_sub(since)))
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

    /// Counts `n` more bytes held. True if that took it above its high water. The stall clock starts then only if the
    /// subscriber is connected; one that is away starts it when it comes back.
    pub fn add(&self, n: usize) -> bool {
        let before = self.bytes.fetch_add(n, Ordering::SeqCst);
        if before == 0 {
            // Its clock of progress starts with the first thing it holds.
            self.progress_ms.store(now_ms(), Ordering::SeqCst);
        }
        let now = before + n;
        if now > self.high && !self.above.swap(true, Ordering::SeqCst) {
            if self.is_connected() {
                self.above_since_ms.store(now_ms().max(1), Ordering::SeqCst);
            }
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
        self.progress_ms.store(now_ms(), Ordering::SeqCst);
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

    /// How long it has been above its high water while connected, if it is (the stall clock).
    pub fn above_for(&self) -> Option<Duration> {
        match self.above_since_ms.load(Ordering::SeqCst) {
            0 => None,
            since => Some(Duration::from_millis(now_ms().saturating_sub(since))),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Its client came back, or went. The stall clock measures only time spent connected above high water: it starts
    /// again on a return above it (what it is owed is replayed, and it is given `T_stall` to read it), and stops while
    /// it is away.
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
        if connected {
            self.progress_ms.store(now_ms(), Ordering::SeqCst);
        }
        let since = if connected && self.is_above() {
            now_ms().max(1)
        } else {
            0
        };
        self.above_since_ms.store(since, Ordering::SeqCst);
    }

    /// Starts its stall clock (and its progress) again, as on a return: it is given the stall bound anew.
    pub fn restart_stall_clock(&self) {
        if self.is_connected() {
            self.set_connected(true);
        }
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
        let payload =
            Payload::charged(Arc::new(vec![1, 2, 3]), Hold::new(Flag(dropped.clone())), 3);
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

    /// The stall clock counts only time spent connected above high water: a subscriber that went above it while away
    /// starts its clock when it comes back, not when it went above.
    #[test]
    fn the_stall_clock_runs_only_while_the_subscriber_is_connected() {
        let backlog = Backlog::new("r:t", 100);
        backlog.set_connected(false);
        backlog.add(500);
        assert_eq!(backlog.above_for(), None, "away: no clock");
        std::thread::sleep(Duration::from_millis(30));
        backlog.set_connected(true);
        let since_back = backlog.above_for().expect("above, and connected");
        assert!(
            since_back < Duration::from_millis(20),
            "counted from its return, not from when it went above: {since_back:?}"
        );
        backlog.set_connected(false);
        assert_eq!(backlog.above_for(), None, "away again: no clock");
    }

    /// What the watchdog reads: how long a connected subscriber holding anything has made no progress, counted from
    /// when it began to hold, its last acknowledgement, or its return; nothing for one away or holding nothing.
    #[test]
    fn a_subscriber_stalls_from_its_last_progress_while_it_holds_anything() {
        let backlog = Backlog::subscriber("r", "t", 100);
        assert_eq!(backlog.owner(), Some(("r", "t")));
        assert_eq!(backlog.stalled_for(), None, "holding nothing");
        std::thread::sleep(Duration::from_millis(40));
        backlog.add(10);
        let held = backlog.stalled_for().expect("holding, and connected");
        assert!(
            held < Duration::from_millis(30),
            "from when it began to hold: {held:?}"
        );
        std::thread::sleep(Duration::from_millis(40));
        backlog.add(10);
        assert!(
            backlog.stalled_for().unwrap() >= Duration::from_millis(40),
            "more to hold is not progress"
        );
        backlog.remove(5);
        assert!(
            backlog.stalled_for().unwrap() < Duration::from_millis(30),
            "an acknowledgement is"
        );
        std::thread::sleep(Duration::from_millis(40));
        backlog.set_connected(false);
        assert_eq!(backlog.stalled_for(), None, "away");
        backlog.set_connected(true);
        assert!(
            backlog.stalled_for().unwrap() < Duration::from_millis(30),
            "a return is progress"
        );
        backlog.remove(15);
        assert_eq!(backlog.stalled_for(), None, "holding nothing again");
    }
}
