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
//! - **A high water per subscriber** ([`cassini_types::Backlog`]) on its delivery outbox, which its topic reads, and
//!   **the stall bound**: a subscriber above its high water for longer than it has its connection ended.

use cassini_types::{Backlog, BrokerMessage, MAX_FRAME_BYTES};
use ractor::ActorRef;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{info, warn};

const MIB: usize = 1024 * 1024;

/// The share of the container's memory limit the budget takes when no setting gives it: a half, so the broker's own
/// working memory (its actors, TLS, the copies a write makes) has the rest.
pub const BUDGET_SHARE_OF_MEMORY_LIMIT: (u64, u64) = (1, 2);

/// The budget where the container has no memory limit (`memory.max` is `max`, or there is no cgroup v2 to read): four
/// frames' worth, for the small machines the broker runs on.
pub const FALLBACK_BUDGET: usize = 256 * MIB;

/// Where the container's memory limit is read from (cgroup v2).
pub const MEMORY_MAX_PATH: &str = "/sys/fs/cgroup/memory.max";

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
    pub budget: Budget,
    /// Each connection's window: a quarter of the budget, and never less than one frame.
    pub window: usize,
    /// Each subscriber's high water: an eighth of the budget, and never less than 1 MiB.
    pub high_water: usize,
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
            window: (budget / 4).max(MAX_FRAME_BYTES),
            high_water: (budget / 8).max(MIB),
            stall,
        })
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

/// The container's memory limit, from `memory.max` at `path`: `None` when there is none (`max`) or it cannot be read.
pub fn read_memory_max(path: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text == "max" {
        return None;
    }
    text.parse().ok()
}

/// The budget when no setting gives it: the share of the memory limit, or [`FALLBACK_BUDGET`] with none.
pub fn default_budget(memory_max: Option<u64>) -> usize {
    let (numerator, denominator) = BUDGET_SHARE_OF_MEMORY_LIMIT;
    memory_max
        .map(|limit| (limit / denominator * numerator) as usize)
        .unwrap_or(FALLBACK_BUDGET)
}

#[derive(Debug, Default)]
struct State {
    used: usize,
    peak: usize,
    /// Exhausted: no charge is taken until what is held falls to the low water.
    alarm: bool,
    waiting: usize,
}

#[derive(Debug)]
struct Inner {
    limit: usize,
    low_water: usize,
    state: Mutex<State>,
    notify: Notify,
    charges: AtomicU64,
    refunds: AtomicU64,
    refused: AtomicU64,
    slow: AtomicU64,
    alarms: AtomicU64,
    holders: Mutex<Vec<Weak<Backlog>>>,
    /// Told when the budget is exhausted: the subscriber manager, whose subscribers that are away let go.
    listener: Mutex<Option<ActorRef<BrokerMessage>>>,
    /// The broker's drain has begun: the watchdog stops.
    draining: AtomicBool,
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
    fn refund(&self, bytes: usize) {
        self.refunds.fetch_add(1, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        state.used = state.used.saturating_sub(bytes);
        if state.alarm && state.used <= self.low_water {
            state.alarm = false;
            let used = state.used;
            drop(state);
            info!(
                used,
                limit = self.limit,
                "the broker's memory budget is below its low water again: its readers read"
            );
            self.notify.notify_waiters();
        }
    }
}

impl Budget {
    /// A budget of `limit` bytes, whose alarm lifts at three quarters of it.
    pub fn new(limit: usize) -> Self {
        Budget(Arc::new(Inner {
            limit,
            low_water: limit / 4 * 3,
            state: Mutex::new(State::default()),
            notify: Notify::new(),
            charges: AtomicU64::new(0),
            refunds: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            alarms: AtomicU64::new(0),
            holders: Mutex::new(Vec::new()),
            listener: Mutex::new(None),
            draining: AtomicBool::new(false),
        }))
    }

    /// The broker's drain has begun: from now its sessions are let go by the drain, and the watchdog ends (it would
    /// otherwise go on telling the subscribers and cutting connections the drain is ending).
    pub fn begin_drain(&self) {
        self.0.draining.store(true, Ordering::SeqCst);
    }

    /// Whether the broker's drain has begun.
    pub fn is_draining(&self) -> bool {
        self.0.draining.load(Ordering::SeqCst)
    }

    pub fn limit(&self) -> usize {
        self.0.limit
    }

    pub fn used(&self) -> usize {
        self.0.state.lock().unwrap().used
    }

    /// Whether it is exhausted: a holder that may refuse refuses the newest.
    pub fn is_exhausted(&self) -> bool {
        self.0.state.lock().unwrap().alarm
    }

    /// Takes `bytes` if they fit and the budget is not exhausted. When they do not fit the budget is exhausted from now
    /// until what is held falls to its low water, and that is logged, once, with its largest holders.
    pub fn try_charge(&self, bytes: usize) -> Option<Charge> {
        let mut state = self.0.state.lock().unwrap();
        if !state.alarm && state.used + bytes <= self.0.limit {
            state.used += bytes;
            state.peak = state.peak.max(state.used);
            drop(state);
            self.0.charges.fetch_add(1, Ordering::Relaxed);
            return Some(Charge {
                budget: self.0.clone(),
                bytes,
            });
        }
        if !state.alarm {
            state.alarm = true;
            let used = state.used;
            drop(state);
            self.raise_alarm(used, bytes);
        }
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

    fn raise_alarm(&self, used: usize, wanted: usize) {
        self.0.alarms.fetch_add(1, Ordering::Relaxed);
        let largest = self.largest_holders(5);
        warn!(
            used,
            limit = self.0.limit,
            wanted,
            largest = ?largest,
            "the broker's memory budget is exhausted: every reader waits until what is held falls to {} bytes, and \
             subscribers whose clients are away let go of what they hold",
            self.0.low_water
        );
        if let Some(listener) = self.0.listener.lock().unwrap().as_ref() {
            let _ = listener.send_message(BrokerMessage::BudgetExhausted);
        }
    }

    /// Who is told when the budget is exhausted.
    pub fn tell_when_exhausted(&self, listener: ActorRef<BrokerMessage>) {
        *self.0.listener.lock().unwrap() = Some(listener);
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
            limit: self.0.limit,
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

/// The connected subscribers the watchdog ends while the budget is exhausted: each holding something and without
/// progress (an acknowledgement, a return) for at least `stall`. Pure, for its tests.
pub fn stalled_while_exhausted(holders: &[Arc<Backlog>], stall: Duration) -> Vec<Arc<Backlog>> {
    holders
        .iter()
        .filter(|b| b.owner().is_some() && b.stalled_for().is_some_and(|d| d >= stall))
        .cloned()
        .collect()
}

/// While the budget is exhausted, every stall check: tells the subscriber manager again (a subscriber that went away
/// since the alarm rose lets go of what it holds), and ends the connection of every connected subscriber that holds
/// something and has made no progress for the stall bound. A topic sees a stall only when a subscriber is above its
/// high water and holds the topic back; with the budget exhausted no reader reads, so nothing may be held back while
/// subscribers below their high water hold the budget. This bounds the broker's recovery by the stall bound
/// (docs/architecture/cassini-backpressure.md, review finding 1). Ends with `broker`.
pub fn spawn_watchdog(bounds: Bounds, broker: ActorRef<BrokerMessage>) {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(bounds.stall_check());
        loop {
            every.tick().await;
            // The drain lets go of what is held; the watchdog would only cut what it is ending.
            if bounds.budget.is_draining()
                || matches!(
                    broker.get_status(),
                    ractor::ActorStatus::Stopping | ractor::ActorStatus::Stopped
                )
            {
                return;
            }
            if !bounds.budget.is_exhausted() {
                continue;
            }
            if let Some(listener) = bounds.budget.0.listener.lock().unwrap().as_ref() {
                let _ = listener.send_message(BrokerMessage::BudgetExhausted);
            }
            let sessions = ractor::registry::where_is(crate::SESSION_MANAGER_NAME.to_string());
            // Decided once a session a look: its other stalled subscriptions go with it.
            let mut ended = std::collections::HashSet::new();
            let mut spared = std::collections::HashSet::new();
            for backlog in stalled_while_exhausted(&bounds.budget.holders(), bounds.stall) {
                let Some((registration_id, topic)) = backlog.owner() else {
                    continue;
                };
                if spared.contains(registration_id) {
                    continue;
                }
                if let Some(connection) = crate::listener::connection_of(registration_id) {
                    // Progress is the connection's: an acknowledgement on any topic, read by its reader, however long
                    // its Listener takes to pass it on (cassini-backpressure.md, second review, finding B).
                    if connection.idle_for() < bounds.stall {
                        continue;
                    }
                    // Its reader waits for room, so what it has acknowledged is not read: the broker's doing, not
                    // its. Its publishes are refused for now instead, and its reader reads on (finding A).
                    if connection.spare(bounds.stall) {
                        spared.insert(registration_id.to_string());
                        info!(
                            subscriber = backlog.name(),
                            held = backlog.bytes(),
                            "the broker's memory budget is exhausted and a subscriber's acknowledgements wait behind \
                             its own publishes: they are refused for now, and its connection kept"
                        );
                        continue;
                    }
                }
                if !ended.insert(registration_id.to_string()) {
                    backlog.set_connected(false);
                    continue;
                }
                let stalled = backlog.stalled_for().unwrap_or_default();
                backlog.set_connected(false);
                bounds.budget.count_slow_disconnection();
                warn!(
                    subscriber = backlog.name(),
                    held = backlog.bytes(),
                    ?stalled,
                    "the broker's memory budget is exhausted and a subscriber has acknowledged nothing for the stall \
                     bound: its connection is ended"
                );
                if let Some(sessions) = &sessions {
                    let _ = sessions.send_message(BrokerMessage::SlowSubscriber {
                        registration_id: registration_id.to_string(),
                        topic: topic.to_string(),
                        held: backlog.bytes(),
                        above_for: stalled,
                    });
                }
            }
        }
    });
}

/// One connection's ingress window: the bytes of the publishes its reader admitted that no topic has accepted yet.
#[derive(Debug)]
pub struct Window {
    limit: usize,
    used: Mutex<usize>,
    notify: Notify,
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
            limit: limit.max(MAX_FRAME_BYTES),
            used: Mutex::new(0),
            notify: Notify::new(),
        })
    }

    pub fn used(&self) -> usize {
        *self.used.lock().unwrap()
    }

    /// Takes `bytes` if they fit, or into an empty window whatever their size.
    pub fn try_take(self: &Arc<Self>, bytes: usize) -> Option<WindowCharge> {
        let mut used = self.used.lock().unwrap();
        if *used == 0 || *used + bytes <= self.limit {
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
            bounds.window, MAX_FRAME_BYTES,
            "a window is never less than a frame"
        );
        assert_eq!(bounds.high_water, MAX_FRAME_BYTES / 8);
        let large = Bounds::new(8 * MAX_FRAME_BYTES, DEFAULT_STALL).unwrap();
        assert_eq!(large.window, 2 * MAX_FRAME_BYTES);
    }

    #[test]
    fn the_default_budget_is_half_the_memory_limit_or_the_fallback_with_none() {
        assert_eq!(default_budget(Some(1 << 30)), 1 << 29);
        assert_eq!(default_budget(None), FALLBACK_BUDGET);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.max");
        std::fs::write(&path, "max\n").unwrap();
        assert_eq!(read_memory_max(path.to_str().unwrap()), None);
        std::fs::write(&path, "536870912\n").unwrap();
        assert_eq!(read_memory_max(path.to_str().unwrap()), Some(536_870_912));
        assert_eq!(read_memory_max("/nowhere/memory.max"), None);
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

    /// An actor that records the messages it is sent, standing in for whoever the watchdog tells.
    struct Recorder;

    #[ractor::async_trait]
    impl ractor::Actor for Recorder {
        type Msg = BrokerMessage;
        type State = Arc<Mutex<Vec<String>>>;
        type Arguments = Arc<Mutex<Vec<String>>>;

        async fn pre_start(
            &self,
            _myself: ActorRef<BrokerMessage>,
            seen: Self::Arguments,
        ) -> Result<Self::State, ractor::ActorProcessingErr> {
            Ok(seen)
        }

        async fn handle(
            &self,
            _myself: ActorRef<BrokerMessage>,
            message: BrokerMessage,
            seen: &mut Self::State,
        ) -> Result<(), ractor::ActorProcessingErr> {
            seen.lock().unwrap().push(format!("{message:?}"));
            Ok(())
        }
    }

    /// The watchdog tells the listener again while the budget is exhausted, and stops once the broker's drain has
    /// begun: what the drain lets go of is the drain's (finding F).
    #[tokio::test]
    async fn the_watchdog_stops_when_the_drain_begins() {
        let bounds = Bounds::new(MAX_FRAME_BYTES, Duration::from_millis(400)).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (listener, _) = ractor::Actor::spawn(None, Recorder, seen.clone())
            .await
            .unwrap();
        let (broker, _) = ractor::Actor::spawn(None, Recorder, Arc::new(Mutex::new(Vec::new())))
            .await
            .unwrap();
        bounds.budget.tell_when_exhausted(listener.clone());
        let _held = bounds.budget.try_charge(MAX_FRAME_BYTES).unwrap();
        assert!(bounds.budget.try_charge(1).is_none());
        let told = || {
            seen.lock()
                .unwrap()
                .iter()
                .filter(|m| m.contains("BudgetExhausted"))
                .count()
        };
        let raised = told();
        spawn_watchdog(bounds.clone(), broker.clone());
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert!(told() > raised, "told again while exhausted");
        bounds.budget.begin_drain();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let at_drain = told();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(told(), at_drain, "not told again once the drain has begun");
        listener.stop(None);
        broker.stop(None);
    }
}
