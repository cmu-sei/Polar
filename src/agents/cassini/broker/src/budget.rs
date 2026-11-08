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

//! The broker's bounds (docs/architecture/cassini-backpressure.md, phase 1, as decided 2026-10-30).
//!
//! - **One budget** for every payload byte the broker holds: in flight from a reader, in a subscriber's delivery
//!   outbox, in a topic waiting for subscribers or for a subscriber to catch up. A reader takes a frame's bytes from it
//!   before it reads the frame; the charge travels with the payload ([`cassini_types::Payload`]) and is refunded when
//!   the last holder lets go. When a frame cannot be charged the budget is exhausted: every reader waits (a memory
//!   alarm), until what is held falls to its low water. Deliveries keep draining, so it recovers.
//! - **A window per connection:** the bytes of the frames its reader has admitted that no topic has yet accepted. A
//!   reader that cannot take a frame's bytes from it does not read, and TCP pushes back on the publisher.
//! - **A high water per subscriber** ([`cassini_types::Backlog`]) on its delivery outbox, which the subscriber owns and
//!   tells its topic of (`TopicHold`), and **the stall bound**, kept by the pressure manager and decided by the one
//!   stall policy (`crate::pressure`).
//!
//! The budget is accounting only: what it holds, its alarm, and its counters. No decision across actors is made
//! through it.

use cassini_types::{Backlog, MAX_FRAME_BYTES};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::Notify;

const MIB: usize = 1024 * 1024;

/// A fixed budget for bounds built without the runtime budget (a test's): four frames, the runtime budget's floor.
pub const FALLBACK_BUDGET: usize = 256 * MIB;

/// How long a subscriber may hold its topic back above its high water when no setting says.
pub const DEFAULT_STALL: Duration = Duration::from_secs(30);

/// The stall bound must be under the sockets' `TCP_USER_TIMEOUT` (`cassini_types::socket`), or the kernel would end a
/// stalled peer first, without the broker's word.
pub const STALL_LIMIT: Duration = Duration::from_secs(60);

/// A frame no larger than this is read before it is charged: it is held by its reader only, so a connection's
/// acknowledgements, subscriptions and small publishes are read whatever the budget, and an acknowledgement is never
/// stuck behind the frames it would make room for. A publish read so is charged before it goes on.
pub const SMALL_FRAME: usize = 64 * 1024;

/// What bounds the broker, as one value its actors share.
#[derive(Clone, Debug)]
pub struct Bounds {
    /// The budget, whose limit the pressure manager may set at run time (`crate::runtime_budget`).
    pub budget: Budget,
    pub stall: Duration,
}

impl Bounds {
    /// The bounds of a budget of `budget` bytes and a stall bound of `stall`. A budget under one frame could never
    /// admit one, and a stall bound of zero or of `STALL_LIMIT` or more is not one: either is refused, saying why.
    pub fn new(budget: usize, stall: Duration) -> Result<Self, String> {
        if budget < MAX_FRAME_BYTES {
            return Err(format!(
                "the broker's memory budget, {budget} bytes, is below one frame ({MAX_FRAME_BYTES} bytes, \
                 cassini_types::MAX_FRAME_BYTES), so it could never take a frame of that size: give \
                 CASSINI_MEMORY_BUDGET_BYTES at least {MAX_FRAME_BYTES}, or the container more memory"
            ));
        }
        if stall.is_zero() || stall >= STALL_LIMIT {
            return Err(format!(
                "the stall bound, {stall:?}, must be more than zero and under {STALL_LIMIT:?} (TCP_USER_TIMEOUT), \
                 or the kernel ends a stalled subscriber's connection before the broker does"
            ));
        }
        Ok(Self {
            budget: Budget::new(budget),
            stall,
        })
    }

    /// Each connection's window, from the budget as it is now: a quarter of it, and never less than one frame.
    pub fn window(&self) -> usize {
        window_of(self.budget.limit())
    }

    /// Each subscriber's high water, from the budget as it is now: an eighth of it, and never less than 1 MiB.
    pub fn high_water(&self) -> usize {
        (self.budget.limit() / 8).max(MIB)
    }

    /// The bounds with no setting and no memory limit: the fallback budget and the default stall bound.
    pub fn fallback() -> Self {
        Self::new(FALLBACK_BUDGET, DEFAULT_STALL).expect("the fallback bounds work")
    }

    /// How often a held-back topic looks whether a subscriber has stalled: a quarter of the bound, between 50 ms and
    /// 5 s.
    pub fn stall_check(&self) -> Duration {
        (self.stall / 4).clamp(Duration::from_millis(50), Duration::from_secs(5))
    }
}

/// A connection's window for a budget of `limit`: a quarter of it, never less than one frame.
fn window_of(limit: usize) -> usize {
    (limit / 4).max(MAX_FRAME_BYTES)
}

#[derive(Debug, Default)]
struct State {
    used: usize,
    peak: usize,
    /// Exhausted: no charge is taken until what is held falls to the low water.
    alarm: bool,
    /// What memory allows is below the floor: no charge is taken until it returns (`crate::runtime_budget`).
    starved: bool,
    waiting: usize,
}

#[derive(Debug)]
struct Inner {
    /// Set at run time by the pressure manager's controller, or fixed.
    limit: std::sync::atomic::AtomicUsize,
    state: Mutex<State>,
    notify: Notify,
    charges: AtomicU64,
    refunds: AtomicU64,
    /// The bytes refunded since the start: what holders let go (subscribers' acknowledgements, mostly), whose rate
    /// the controller takes for the drain rate.
    refunded_bytes: AtomicU64,
    refused: AtomicU64,
    slow: AtomicU64,
    alarms: AtomicU64,
    /// What is held, by name, for the budget's report: read for it only, never for a decision.
    holders: Mutex<Vec<Weak<Backlog>>>,
}

/// A reader counted as waiting on the budget, until it is dropped.
struct Waiting<'a>(&'a Inner);

impl<'a> Waiting<'a> {
    fn new(inner: &'a Inner) -> Self {
        inner.state.lock().unwrap().waiting += 1;
        Waiting(inner)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().waiting -= 1;
    }
}

/// The broker's memory budget. Cloning shares it.
#[derive(Clone, Debug)]
pub struct Budget(Arc<Inner>);

/// Bytes taken from the budget, refunded when dropped.
#[derive(Debug)]
pub struct Charge {
    budget: Arc<Inner>,
    bytes: usize,
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.refund(self.bytes);
    }
}

impl Charge {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// What the budget holds now, and what it has counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetStats {
    pub limit: usize,
    pub used: usize,
    pub peak: usize,
    pub exhausted: bool,
    pub readers_waiting: usize,
    pub charges: u64,
    pub refunds: u64,
    /// Messages a holder refused, or let go, because the budget was exhausted.
    pub refused: u64,
    /// Subscribers whose connection was ended for holding their topic back too long.
    pub slow_disconnections: u64,
    pub alarms: u64,
}

impl Inner {
    fn limit(&self) -> usize {
        self.limit.load(Ordering::SeqCst)
    }

    /// Three quarters of the limit: where the alarm lifts.
    fn low_water(&self) -> usize {
        self.limit() / 4 * 3
    }

    fn refund(&self, bytes: usize) {
        self.refunds.fetch_add(1, Ordering::Relaxed);
        self.refunded_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        state.used = state.used.saturating_sub(bytes);
        self.lift_if_drained(state);
    }

    /// Lifts the alarm if what is held has fallen to the low water and memory is not below the floor: its readers
    /// read. The pressure manager says so when it next looks.
    fn lift_if_drained(&self, mut state: std::sync::MutexGuard<'_, State>) {
        if state.alarm && !state.starved && state.used <= self.low_water() {
            state.alarm = false;
            drop(state);
            self.notify.notify_waiters();
        }
    }

    /// Raises the alarm, counting it, if it is not raised.
    fn raise(&self, state: &mut State) {
        if !state.alarm {
            state.alarm = true;
            self.alarms.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Budget {
    /// A budget of `limit` bytes, whose alarm lifts at three quarters of it.
    pub fn new(limit: usize) -> Self {
        Budget(Arc::new(Inner {
            limit: std::sync::atomic::AtomicUsize::new(limit),
            state: Mutex::new(State::default()),
            notify: Notify::new(),
            charges: AtomicU64::new(0),
            refunds: AtomicU64::new(0),
            refunded_bytes: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            alarms: AtomicU64::new(0),
            holders: Mutex::new(Vec::new()),
        }))
    }

    pub fn limit(&self) -> usize {
        self.0.limit()
    }

    /// Sets the limit (the pressure manager's controller). A limit under what is held evicts nothing: the alarm stops
    /// admission until what is held drains to the new low water.
    pub fn set_limit(&self, limit: usize) {
        self.0.limit.store(limit, Ordering::SeqCst);
        let mut state = self.0.state.lock().unwrap();
        if state.used > limit {
            self.0.raise(&mut state);
        } else {
            self.0.lift_if_drained(state);
        }
    }

    /// What memory allows is below the floor (`starved`), or no longer is: while it is, nothing is admitted.
    pub fn set_starved(&self, starved: bool) {
        let mut state = self.0.state.lock().unwrap();
        state.starved = starved;
        if starved {
            self.0.raise(&mut state);
        } else {
            self.0.lift_if_drained(state);
        }
    }

    /// The bytes refunded since the start.
    pub fn refunded_bytes(&self) -> u64 {
        self.0.refunded_bytes.load(Ordering::Relaxed)
    }

    pub fn used(&self) -> usize {
        self.0.state.lock().unwrap().used
    }

    /// Whether it is exhausted: a holder that may refuse refuses the newest.
    pub fn is_exhausted(&self) -> bool {
        self.0.state.lock().unwrap().alarm
    }

    /// Takes `bytes` if they fit and the budget is not exhausted. When they do not fit the budget is exhausted from now
    /// until what is held falls to its low water (counted; the pressure manager says so, with its largest holders).
    pub fn try_charge(&self, bytes: usize) -> Option<Charge> {
        let mut state = self.0.state.lock().unwrap();
        if !state.alarm && state.used + bytes <= self.0.limit() {
            state.used += bytes;
            state.peak = state.peak.max(state.used);
            drop(state);
            self.0.charges.fetch_add(1, Ordering::Relaxed);
            return Some(Charge {
                budget: self.0.clone(),
                bytes,
            });
        }
        self.0.raise(&mut state);
        None
    }

    /// Takes `bytes`, waiting while the budget cannot give them. A reader waits here, and reads nothing meanwhile.
    pub async fn charge(&self, bytes: usize) -> Charge {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(charge) = self.try_charge(bytes) {
                return charge;
            }
            // Counted while it waits, and no longer once it stops, however it stops: a reader whose connection is
            // cut drops this wait.
            let _waiting = Waiting::new(&self.0);
            notified.await;
        }
    }

    /// Counts what is held under `backlog`'s name in what the budget reports: a subscriber's delivery outbox, a
    /// topic's queue. Forgotten when the backlog is dropped.
    pub fn register(&self, backlog: &Arc<Backlog>) {
        let mut holders = self.0.holders.lock().unwrap();
        holders.retain(|h| h.strong_count() > 0);
        holders.push(Arc::downgrade(backlog));
    }

    /// Every holder still counted.
    pub fn holders(&self) -> Vec<Arc<Backlog>> {
        self.0
            .holders
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// The `n` largest holders, by name and bytes.
    pub fn largest_holders(&self, n: usize) -> Vec<(String, usize)> {
        let mut holders: Vec<(String, usize)> = self
            .0
            .holders
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .map(|b| (b.name().to_string(), b.bytes()))
            .filter(|(_, bytes)| *bytes > 0)
            .collect();
        holders.sort_by_key(|holder| std::cmp::Reverse(holder.1));
        holders.truncate(n);
        holders
    }

    /// Counts a message a holder refused, or let go of, because the budget was exhausted.
    pub fn count_refused(&self, messages: u64) {
        self.0.refused.fetch_add(messages, Ordering::Relaxed);
    }

    /// Counts a subscriber whose connection was ended for stalling its topic.
    pub fn count_slow_disconnection(&self) {
        self.0.slow.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stats(&self) -> BudgetStats {
        let state = self.0.state.lock().unwrap();
        BudgetStats {
            limit: self.0.limit(),
            used: state.used,
            peak: state.peak,
            exhausted: state.alarm,
            readers_waiting: state.waiting,
            charges: self.0.charges.load(Ordering::Relaxed),
            refunds: self.0.refunds.load(Ordering::Relaxed),
            refused: self.0.refused.load(Ordering::Relaxed),
            slow_disconnections: self.0.slow.load(Ordering::Relaxed),
            alarms: self.0.alarms.load(Ordering::Relaxed),
        }
    }
}

/// One connection's ingress window: the bytes of the publishes its reader admitted that no topic has accepted yet.
#[derive(Debug)]
pub struct Window {
    limit: WindowLimit,
    used: Mutex<usize>,
    notify: Notify,
}

/// A window's limit: fixed, or a budget's as it is now (`Bounds::window`).
#[derive(Debug)]
enum WindowLimit {
    Fixed(usize),
    Of(Budget),
}

/// Bytes taken from a connection's window, given back when dropped: when the topic accepts the publish.
#[derive(Debug)]
pub struct WindowCharge {
    window: Arc<Window>,
    bytes: usize,
}

impl Drop for WindowCharge {
    fn drop(&mut self) {
        *self.window.used.lock().unwrap() -= self.bytes;
        self.window.notify.notify_waiters();
    }
}

impl Window {
    /// A window of `limit` bytes, never less than one frame.
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Window {
            limit: WindowLimit::Fixed(limit.max(MAX_FRAME_BYTES)),
            used: Mutex::new(0),
            notify: Notify::new(),
        })
    }

    /// A window that follows `budget`: a quarter of it as it is now, never less than one frame.
    pub fn of(budget: &Budget) -> Arc<Self> {
        Arc::new(Window {
            limit: WindowLimit::Of(budget.clone()),
            used: Mutex::new(0),
            notify: Notify::new(),
        })
    }

    fn limit(&self) -> usize {
        match &self.limit {
            WindowLimit::Fixed(limit) => *limit,
            WindowLimit::Of(budget) => window_of(budget.limit()),
        }
    }

    pub fn used(&self) -> usize {
        *self.used.lock().unwrap()
    }

    /// Takes `bytes` if they fit, or into an empty window whatever their size.
    pub fn try_take(self: &Arc<Self>, bytes: usize) -> Option<WindowCharge> {
        let mut used = self.used.lock().unwrap();
        if *used == 0 || *used + bytes <= self.limit() {
            *used += bytes;
            return Some(WindowCharge {
                window: self.clone(),
                bytes,
            });
        }
        None
    }

    /// Takes `bytes`, waiting until what the topics accept makes room.
    pub async fn take(self: &Arc<Self>, bytes: usize) -> WindowCharge {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(charge) = self.try_take(bytes) {
                return charge;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_below_one_frame_or_a_stall_bound_past_the_socket_s_is_refused_saying_why() {
        let short = Bounds::new(MAX_FRAME_BYTES - 1, DEFAULT_STALL).unwrap_err();
        assert!(short.contains("below one frame"), "{short}");
        let late = Bounds::new(MAX_FRAME_BYTES, STALL_LIMIT).unwrap_err();
        assert!(late.contains("TCP_USER_TIMEOUT"), "{late}");
        assert!(Bounds::new(MAX_FRAME_BYTES, Duration::ZERO).is_err());
        let bounds = Bounds::new(MAX_FRAME_BYTES, DEFAULT_STALL).unwrap();
        assert_eq!(
            bounds.window(),
            MAX_FRAME_BYTES,
            "a window is never less than a frame"
        );
        assert_eq!(bounds.high_water(), MAX_FRAME_BYTES / 8);
        let large = Bounds::new(8 * MAX_FRAME_BYTES, DEFAULT_STALL).unwrap();
        assert_eq!(large.window(), 2 * MAX_FRAME_BYTES);
        // Re-derived from the budget as it is now, with their ratios and minimums.
        large.budget.set_limit(16 * MAX_FRAME_BYTES);
        assert_eq!(large.window(), 4 * MAX_FRAME_BYTES);
        assert_eq!(large.high_water(), 2 * MAX_FRAME_BYTES);
    }

    #[test]
    fn an_exhausted_budget_takes_nothing_until_it_falls_to_its_low_water() {
        let budget = Budget::new(100);
        let a = budget.try_charge(80).unwrap();
        let b = budget.try_charge(10).unwrap();
        assert!(budget.try_charge(20).is_none(), "over the limit");
        assert!(budget.is_exhausted());
        assert!(budget.try_charge(1).is_none(), "nothing while exhausted");
        drop(b);
        assert!(budget.is_exhausted(), "80 is not yet at the low water, 75");
        assert!(budget.try_charge(1).is_none());
        drop(a);
        assert!(!budget.is_exhausted());
        assert!(budget.try_charge(100).is_some());
        let stats = budget.stats();
        assert_eq!((stats.charges, stats.refunds, stats.peak), (3, 3, 100));
    }

    #[tokio::test]
    async fn a_reader_waiting_on_the_budget_or_its_window_is_woken_when_room_is_made() {
        let budget = Budget::new(100);
        let held = budget.try_charge(100).unwrap();
        let waiting = tokio::spawn({
            let budget = budget.clone();
            async move { budget.charge(50).await.bytes() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(budget.try_charge(1).is_none());
        assert_eq!(budget.stats().readers_waiting, 1);
        drop(held);
        assert_eq!(waiting.await.unwrap(), 50);

        let window = Window::new(MAX_FRAME_BYTES);
        let first = window.try_take(MAX_FRAME_BYTES).unwrap();
        assert!(window.try_take(1).is_none());
        let next = tokio::spawn({
            let window = window.clone();
            async move { window.take(1).await.bytes }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(first);
        assert_eq!(next.await.unwrap(), 1);
        assert_eq!(window.used(), 0);
    }

    /// A budget set below what it holds evicts nothing: it admits nothing until what is held drains to the new low
    /// water, then admits again (cassini-backpressure.md, "Runtime budget", 5).
    #[test]
    fn a_shrinking_budget_evicts_nothing_and_admits_again_once_drained() {
        let budget = Budget::new(400);
        let a = budget.try_charge(150).unwrap();
        let b = budget.try_charge(150).unwrap();
        budget.set_limit(200);
        assert_eq!(budget.used(), 300, "nothing evicted");
        assert!(budget.is_exhausted(), "held over the limit: the alarm");
        assert!(budget.try_charge(1).is_none());
        drop(a);
        assert!(!budget.is_exhausted(), "150 is at the new low water");
        assert!(budget.try_charge(50).is_some());
        drop(b);
        assert_eq!(budget.refunded_bytes(), 350);
        budget.set_limit(400);
        assert!(budget.try_charge(399).is_some(), "grown again");
    }

    /// While memory is below the floor nothing is admitted, whatever is held; when it returns, admission does.
    #[test]
    fn a_starved_budget_admits_nothing_until_memory_returns() {
        let budget = Budget::new(400);
        budget.set_starved(true);
        assert!(budget.is_exhausted());
        assert!(budget.try_charge(1).is_none(), "starved");
        budget.set_starved(false);
        assert!(!budget.is_exhausted());
        assert!(budget.try_charge(1).is_some());
    }

    /// A window that follows its budget grows and shrinks with it.
    #[test]
    fn a_window_follows_its_budget() {
        let budget = Budget::new(8 * MAX_FRAME_BYTES);
        let window = Window::of(&budget);
        let first = window.try_take(2 * MAX_FRAME_BYTES).unwrap();
        assert!(window.try_take(1).is_none(), "a quarter of the budget");
        budget.set_limit(16 * MAX_FRAME_BYTES);
        assert!(window.try_take(1).is_some(), "the budget grew");
        drop(first);
    }

    /// A reader that gives up waiting (its connection cut, its wait dropped) is no longer counted as waiting
    /// (cassini-backpressure.md, second review, finding E).
    #[tokio::test]
    async fn a_wait_given_up_is_no_longer_counted() {
        let budget = Budget::new(100);
        let _held = budget.try_charge(100).unwrap();
        let waiting = tokio::spawn({
            let budget = budget.clone();
            async move { budget.charge(50).await.bytes() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(budget.stats().readers_waiting, 1);
        waiting.abort();
        let _ = waiting.await;
        assert_eq!(budget.stats().readers_waiting, 0);
    }
}
