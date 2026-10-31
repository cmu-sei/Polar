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

//! The broker's pressure (docs/architecture/cassini-backpressure.md, "Decisions reviewed with the maintainer"): one
//! supervised actor, the [`PressureManager`], that watches the memory budget's alarm and paces the stall checks, and
//! the one stall policy, [`verdict`], which each session applies to its own facts.
//!
//! The manager writes no other actor's state. At each stall check it tells the subscriber manager, which tells each
//! subscriber: `BudgetExhausted` while the budget is exhausted, `StallCheck` otherwise. A subscriber looks at its own
//! backlog; one away lets go of what it holds while the budget is exhausted, and one connected that has stalled says
//! so to its session (`SubscriberStalled`). The session, told by its connection's reader what it read and whether it
//! waits for room (`ConnectionProgress`, `ConnectionWaiting`), decides by [`verdict`]: nothing, spare the connection
//! (its reader refuses for now, and reads on), or end it.

use crate::budget::Bounds;
use crate::runtime_budget::{BudgetSnapshot, Controller, LimitedBy, MemorySource, WarmStart};
use cassini_types::BrokerMessage;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// How many samples go by between two of the controller's log lines, when nothing it decides changes.
pub const LOG_EVERY_SAMPLES: u32 = 10;

/// The runtime budget: its controller and where memory is read.
#[derive(Clone)]
pub struct BudgetControl {
    pub controller: Controller,
    pub memory: Arc<dyn MemorySource>,
}

impl std::fmt::Debug for BudgetControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BudgetControl")
            .field("controller", &self.controller)
            .finish_non_exhaustive()
    }
}

/// What the pressure manager is told.
#[derive(Debug)]
pub enum PressureMsg {
    /// Its own, at each stall check: look at the budget, and have every subscriber look whether it has stalled.
    Tick,
    /// The subscriber manager, through which the checks reach the subscribers.
    SetSubscribers(ActorRef<BrokerMessage>),
    /// The broker's drain has begun: the manager ends (what the drain lets go of is the drain's).
    Drain,
    /// Its own, every `Tunables::sample_every`: sample memory and set the budget (with a runtime budget).
    Sample,
    /// A learned value to start from, whenever it comes: adopted within the floor, the ceiling and the cap.
    WarmStart(WarmStart),
    /// The runtime budget's state, for pulled health; none with a fixed budget.
    Snapshot(RpcReplyPort<Option<BudgetSnapshot>>),
}

/// The broker's pressure: the budget's alarm, and the pace of the stall checks.
pub struct PressureManager;

pub struct PressureManagerArgs {
    pub bounds: Bounds,
    /// The subscriber manager, if it is already running (otherwise `PressureMsg::SetSubscribers`).
    pub subscribers: Option<ActorRef<BrokerMessage>>,
    /// The runtime budget; none keeps the budget as it was given.
    pub control: Option<BudgetControl>,
}

pub struct PressureState {
    bounds: Bounds,
    subscribers: Option<ActorRef<BrokerMessage>>,
    /// The budget was exhausted at the last look.
    exhausted: bool,
    control: Option<BudgetControl>,
    started: tokio::time::Instant,
    /// What the controller last said: what limited the budget, whether it starved, and samples since its last line.
    said: Option<(LimitedBy, bool)>,
    samples_since_said: u32,
}

#[ractor::async_trait]
impl Actor for PressureManager {
    type Msg = PressureMsg;
    type State = PressureState;
    type Arguments = PressureManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<PressureMsg>,
        args: PressureManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        drop(myself.send_interval(args.bounds.stall_check(), || PressureMsg::Tick));
        if let Some(control) = &args.control {
            drop(
                myself.send_interval(control.controller.tunables().sample_every, || {
                    PressureMsg::Sample
                }),
            );
        }
        Ok(PressureState {
            bounds: args.bounds,
            subscribers: args.subscribers,
            exhausted: false,
            control: args.control,
            started: tokio::time::Instant::now(),
            said: None,
            samples_since_said: 0,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<PressureMsg>,
        message: PressureMsg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            PressureMsg::Tick => Self::look(state),
            PressureMsg::SetSubscribers(subscribers) => state.subscribers = Some(subscribers),
            PressureMsg::Drain => myself.stop(Some("the broker's drain has begun".to_string())),
            PressureMsg::Sample => Self::sample(state),
            PressureMsg::WarmStart(learned) => {
                if let Some(control) = state.control.as_mut() {
                    control.controller.warm_start(learned);
                    let budget = control.controller.budget() as usize;
                    state.bounds.budget.set_limit(budget);
                    info!(
                        ?learned,
                        budget, "the broker's memory budget starts from what was learned"
                    );
                }
            }
            PressureMsg::Snapshot(reply) => {
                let budget = &state.bounds.budget;
                let snapshot = state.control.as_ref().map(|c| {
                    c.controller
                        .snapshot(budget.is_exhausted(), budget.used() as u64)
                });
                let _ = reply.send(snapshot);
            }
        }
        Ok(())
    }
}

impl PressureManager {
    /// Samples memory and sets the budget from what the controller decides; says so when what limits it changes, or
    /// memory falls below the floor or returns, and every `LOG_EVERY_SAMPLES` samples otherwise.
    fn sample(state: &mut PressureState) {
        let Some(control) = state.control.as_mut() else {
            return;
        };
        let budget = &state.bounds.budget;
        let memory = control.memory.sample();
        let decision = control.controller.sample(
            state.started.elapsed(),
            &memory,
            budget.used() as u64,
            budget.refunded_bytes(),
        );
        budget.set_limit(decision.budget as usize);
        budget.set_starved(decision.starved);
        let snapshot = control
            .controller
            .snapshot(budget.is_exhausted(), budget.used() as u64);
        let now = (snapshot.limited_by, snapshot.starved);
        state.samples_since_said += 1;
        if state.said != Some(now) || state.samples_since_said >= LOG_EVERY_SAMPLES {
            if snapshot.starved && state.said.is_none_or(|(_, starved)| !starved) {
                warn!(
                    ?memory,
                    floor = control.controller.tunables().floor(),
                    "the memory the broker can have is below its floor: it admits nothing until memory returns"
                );
            }
            info!(
                budget = snapshot.budget,
                ceiling = snapshot.ceiling,
                target = snapshot.target,
                limited_by = ?snapshot.limited_by,
                drain_rate = snapshot.drain_rate,
                held = snapshot.held,
                alarm = snapshot.alarm,
                starved = snapshot.starved,
                "the broker's memory budget"
            );
            state.said = Some(now);
            state.samples_since_said = 0;
        }
    }

    /// Looks at the budget, says when its alarm rises or lifts, and has every subscriber look whether it has stalled.
    fn look(state: &mut PressureState) {
        let budget = &state.bounds.budget;
        let exhausted = budget.is_exhausted();
        if exhausted && !state.exhausted {
            warn!(
                used = budget.used(),
                limit = budget.limit(),
                largest = ?budget.largest_holders(5),
                "the broker's memory budget is exhausted: every reader waits until what is held falls to its low \
                 water, subscribers whose clients are away let go of what they hold, and subscribers that make no \
                 progress for the stall bound have their connections ended"
            );
        } else if !exhausted && state.exhausted {
            info!(
                used = budget.used(),
                limit = budget.limit(),
                "the broker's memory budget is below its low water again: its readers read"
            );
        }
        state.exhausted = exhausted;
        if let Some(subscribers) = &state.subscribers {
            let check = if exhausted {
                BrokerMessage::BudgetExhausted
            } else {
                BrokerMessage::StallCheck
            };
            let _ = subscribers.send_message(check);
        }
    }
}

/// What a session knows of a stalled subscriber and its own connection, at the moment it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallFacts {
    /// The stall was seen while the budget was exhausted (nothing acknowledged), not above high water.
    pub exhausted: bool,
    /// How long since the connection's reader last read an acknowledgement, on any topic (or since it opened).
    pub since_progress: Duration,
    /// The connection's reader waits for a frame's admission, so what follows (its acknowledgements among it) is not
    /// read: a stall that may be the broker's doing.
    pub reader_waiting: bool,
    /// How long since the connection was last spared, if it was.
    pub since_spared: Option<Duration>,
    /// It has made progress since it was last spared.
    pub progressed_since_spared: bool,
    /// How long since a judgement first found it should be ended, if one did and none has found otherwise since.
    pub since_condemned: Option<Duration>,
}

/// What a session does about a stalled subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing: it is not stalled, as far as its connection shows.
    Ignore,
    /// Spare its connection: its reader refuses for now, for the stall bound, what it cannot admit at once, and reads
    /// on; it is not judged again within that refusal window.
    Spare,
    /// It would be ended, but its reader's facts may not have reached the session yet (they come in the session's
    /// mailbox, in order with the stall reports): ended only if a later look still finds so.
    Condemn,
    /// End its connection.
    End,
}

/// How long after a connection is first condemned a later look may end it: an eighth of the stall bound, so a
/// subscriber's reports of one stall check (several subscriptions of a session report together) do not confirm each
/// other, and the next check's do.
pub fn confirmation(stall: Duration) -> Duration {
    stall / 8
}

/// The one stall policy (cassini-backpressure.md, the second review and the decisions reviewed with the maintainer):
/// - a connection spared less than the stall bound ago is in its refusal window, and not judged again;
/// - while the budget is exhausted, progress is the connection's: an acknowledgement read on any topic within the
///   bound is progress for every subscription of the session;
/// - a connection whose reader waits for room may be stalled by the broker, not by its client: it is spared, and spared
///   again only once it has made progress since;
/// - otherwise it is condemned, and ended at a later look that still finds so: the reader's facts reach the session
///   by message, and one that began waiting just after the subscriber reported has a check's time to say so.
pub fn verdict(stall: Duration, facts: StallFacts) -> Verdict {
    if facts.since_spared.is_some_and(|since| since < stall) {
        return Verdict::Ignore;
    }
    if facts.exhausted && facts.since_progress < stall {
        return Verdict::Ignore;
    }
    if facts.reader_waiting && (facts.since_spared.is_none() || facts.progressed_since_spared) {
        return Verdict::Spare;
    }
    match facts.since_condemned {
        Some(since) if since >= confirmation(stall) => Verdict::End,
        _ => Verdict::Condemn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Bounds;
    use cassini_types::MAX_FRAME_BYTES;
    use proptest::prelude::*;
    use std::sync::{Arc, Mutex};

    const STALL: Duration = Duration::from_secs(2);

    fn facts() -> StallFacts {
        StallFacts {
            exhausted: true,
            since_progress: STALL,
            reader_waiting: false,
            since_spared: None,
            progressed_since_spared: false,
            since_condemned: Some(STALL),
        }
    }

    #[test]
    fn the_stall_policy_decides_by_progress_waiting_and_sparing() {
        assert_eq!(
            verdict(STALL, facts()),
            Verdict::End,
            "no progress, not waiting"
        );
        assert_eq!(
            verdict(
                STALL,
                StallFacts {
                    since_condemned: None,
                    ..facts()
                }
            ),
            Verdict::Condemn,
            "first condemned, then ended at a later look"
        );
        assert_eq!(
            verdict(
                STALL,
                StallFacts {
                    since_condemned: Some(confirmation(STALL) - Duration::from_millis(1)),
                    ..facts()
                }
            ),
            Verdict::Condemn,
            "not by a report of the same look"
        );
        let progressing = StallFacts {
            since_progress: STALL - Duration::from_millis(1),
            ..facts()
        };
        assert_eq!(
            verdict(STALL, progressing),
            Verdict::Ignore,
            "progress is the connection's"
        );
        assert_eq!(
            verdict(
                STALL,
                StallFacts {
                    exhausted: false,
                    ..progressing
                }
            ),
            Verdict::End,
            "above high water, progress does not excuse it"
        );
        let waiting = StallFacts {
            reader_waiting: true,
            ..facts()
        };
        assert_eq!(
            verdict(STALL, waiting),
            Verdict::Spare,
            "its reader waits: spared"
        );
        let in_window = StallFacts {
            since_spared: Some(STALL - Duration::from_millis(1)),
            ..waiting
        };
        assert_eq!(
            verdict(STALL, in_window),
            Verdict::Ignore,
            "in its refusal window"
        );
        let again = StallFacts {
            since_spared: Some(STALL),
            ..waiting
        };
        assert_eq!(
            verdict(STALL, again),
            Verdict::End,
            "no progress since it was spared"
        );
        assert_eq!(
            verdict(
                STALL,
                StallFacts {
                    progressed_since_spared: true,
                    ..again
                }
            ),
            Verdict::Spare,
            "spared again once it has made progress"
        );
    }

    fn any_facts() -> impl Strategy<Value = StallFacts> {
        (
            any::<bool>(),
            0u64..5_000,
            any::<bool>(),
            proptest::option::of(0u64..5_000),
            any::<bool>(),
            proptest::option::of(0u64..5_000),
        )
            .prop_map(
                |(exhausted, progress, reader_waiting, spared, progressed, condemned)| StallFacts {
                    exhausted,
                    since_progress: Duration::from_millis(progress),
                    reader_waiting,
                    since_spared: spared.map(Duration::from_millis),
                    progressed_since_spared: progressed,
                    since_condemned: condemned.map(Duration::from_millis),
                },
            )
    }

    proptest! {
        /// A connection is ended only if it is out of its refusal window, made no progress within the bound while the
        /// budget is exhausted, cannot be spared (its reader does not wait, or it was spared and made no progress
        /// since), and was condemned at an earlier look.
        #[test]
        fn a_connection_is_ended_only_when_nothing_spares_it(facts in any_facts()) {
            let ended = verdict(STALL, facts) == Verdict::End;
            let in_window = facts.since_spared.is_some_and(|s| s < STALL);
            let progressing = facts.exhausted && facts.since_progress < STALL;
            let sparable = facts.reader_waiting && (facts.since_spared.is_none() || facts.progressed_since_spared);
            let confirmed = facts.since_condemned.is_some_and(|s| s >= confirmation(STALL));
            prop_assert_eq!(ended, !in_window && !progressing && !sparable && confirmed);
        }

        /// A client that never makes progress is spared at most once, however its reader waits, and is ended within
        /// two stall bounds of its first look.
        #[test]
        fn a_client_that_never_progresses_is_spared_at_most_once(waits in proptest::collection::vec(any::<bool>(), 1..40)) {
            let tick = STALL / 4;
            let mut since_spared: Option<Duration> = None;
            let mut since_condemned: Option<Duration> = None;
            let mut spares = 0;
            let mut ended_at = None;
            for (n, reader_waiting) in waits.iter().enumerate() {
                let facts = StallFacts {
                    exhausted: true,
                    since_progress: STALL + tick * n as u32,
                    reader_waiting: *reader_waiting,
                    since_spared,
                    progressed_since_spared: false,
                    since_condemned,
                };
                match verdict(STALL, facts) {
                    Verdict::Spare => { spares += 1; since_spared = Some(Duration::ZERO); since_condemned = None; }
                    Verdict::End => { ended_at = Some(n); break; }
                    Verdict::Condemn => { since_condemned.get_or_insert(Duration::ZERO); }
                    Verdict::Ignore => since_condemned = None,
                }
                since_spared = since_spared.map(|s| s + tick);
                since_condemned = since_condemned.map(|s| s + tick);
            }
            prop_assert!(spares <= 1);
            if waits.len() > 10 {
                prop_assert!(ended_at.is_some_and(|n| n <= 9), "ended within two stall bounds and a look: {:?}", ended_at);
            }
        }
    }

    /// Memory as a test says it is.
    struct Fake(std::sync::Mutex<crate::runtime_budget::MemorySample>);

    impl MemorySource for Fake {
        fn sample(&self) -> crate::runtime_budget::MemorySample {
            self.0.lock().unwrap().clone()
        }
    }

    /// The manager sets the budget from the controller at each sample, takes a warm start whenever it comes (clipped),
    /// and tells its state; with a fixed budget it tells none.
    #[tokio::test]
    async fn the_manager_sets_the_budget_takes_a_warm_start_and_tells_its_state() {
        use crate::runtime_budget::{MemorySample, Tunables};
        let tunables = Tunables {
            sample_every: Duration::from_millis(20),
            ..Tunables::default()
        };
        let floor = tunables.floor();
        let bounds = Bounds::new(floor as usize, Duration::from_secs(2)).unwrap();
        let memory = Arc::new(Fake(std::sync::Mutex::new(MemorySample {
            mem_available: Some(64 << 30),
            ..MemorySample::default()
        })));
        let cap = 3 * floor;
        let (manager, _) = ractor::Actor::spawn(
            None,
            PressureManager,
            PressureManagerArgs {
                bounds: bounds.clone(),
                subscribers: None,
                control: Some(BudgetControl {
                    controller: Controller::new(tunables, Duration::from_secs(2), Some(cap)),
                    memory: memory.clone(),
                }),
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            bounds.budget.limit() as u64,
            floor,
            "no demand yet: the floor"
        );
        manager
            .cast(PressureMsg::WarmStart(WarmStart {
                budget: 10 * floor,
                drain_rate: 0,
            }))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let snapshot = ractor::call!(manager, PressureMsg::Snapshot)
            .unwrap()
            .unwrap();
        assert!(
            snapshot.budget <= cap,
            "a warm start is clipped to the cap: {snapshot:?}"
        );
        // Memory falls below the floor: the budget admits nothing, and says so; it returns, and admission does.
        *memory.0.lock().unwrap() = MemorySample {
            mem_available: Some(0),
            ..MemorySample::default()
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(bounds.budget.try_charge(1).is_none(), "starved");
        let snapshot = ractor::call!(manager, PressureMsg::Snapshot)
            .unwrap()
            .unwrap();
        assert!(
            snapshot.starved && snapshot.limited_by == LimitedBy::Floor,
            "{snapshot:?}"
        );
        *memory.0.lock().unwrap() = MemorySample {
            mem_available: Some(64 << 30),
            ..MemorySample::default()
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(bounds.budget.try_charge(1).is_some(), "memory returned");
        manager.stop(None);

        let fixed = Bounds::new(floor as usize, Duration::from_secs(2)).unwrap();
        let (manager, _) = ractor::Actor::spawn(
            None,
            PressureManager,
            PressureManagerArgs {
                bounds: fixed.clone(),
                subscribers: None,
                control: None,
            },
        )
        .await
        .unwrap();
        manager
            .cast(PressureMsg::WarmStart(WarmStart {
                budget: 10 * floor,
                drain_rate: 0,
            }))
            .unwrap();
        assert_eq!(ractor::call!(manager, PressureMsg::Snapshot).unwrap(), None);
        assert_eq!(
            fixed.budget.limit() as u64,
            floor,
            "a fixed budget stays as given"
        );
        manager.stop(None);
    }

    /// An actor that records the messages it is sent, standing in for whoever the manager tells.
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
        let (watchdog, _) = ractor::Actor::spawn_linked(
            None,
            PressureManager,
            PressureManagerArgs {
                bounds: bounds.clone(),
                subscribers: Some(listener.clone()),
                control: None,
            },
            broker.get_cell(),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert!(told() > raised, "told again while exhausted");
        watchdog.cast(PressureMsg::Drain).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let at_drain = told();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(told(), at_drain, "not told again once the drain has begun");
        listener.stop(None);
        broker.stop(None);
    }
}
