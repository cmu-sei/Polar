//! The health decisions, with no I/O.
//!
//! `HealthMachine` holds what the actor knows (connections, graph successes, the
//! rejuvenation phase) and decides what each message and each tick means: what to
//! write to the health file and what the actor should do next. The time, the
//! certificate reading, the dependency result and the jitter all come in as
//! parameters, so a whole rejuvenation can be run in a test in microseconds.
//!
//! The actor (lib.rs) gathers those inputs, calls the machine, and carries out
//! what it returns.

use crate::certs::CertReading;
use crate::{
    DEFAULT_JITTER_WINDOW_SECS, HealthCheckMessage, HealthConfig, HealthPhase, HealthState,
    HealthVerdict, RejuvenationPhase,
};

/// What the actor does after a tick.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Next {
    /// Schedule another tick.
    Reschedule,
    /// The machine has decided the agent must end. Emitted once; from then on the
    /// machine only keeps the health file fresh.
    Verdict(HealthVerdict),
}

#[derive(Debug, Clone)]
pub(crate) struct TickOutcome {
    /// The state to write to the health file, if any.
    pub write: Option<HealthState>,
    pub next: Next,
}

/// The result of the first half of a tick.
#[derive(Debug, Clone)]
pub(crate) enum TickStep {
    Done(TickOutcome),
    /// The tick cannot finish without knowing whether the dependency
    /// certificates are healthy. The caller checks them and then calls
    /// [`HealthMachine::deps_checked`]. Kept separate so the (slow) network
    /// check only happens once the hard certificate and graph checks have passed.
    NeedDeps,
}

/// How many health file writes in a row may fail before the actor gives up.
/// The probe tolerates a stale file for 90 s, three ticks, so by the time this
/// fires the file is already about to read as dead.
pub(crate) const MAX_CONSECUTIVE_WRITE_FAILURES: u32 = 3;

/// Turns a random number into a delay inside the jitter window.
pub(crate) fn jitter_secs(random: u32) -> u64 {
    u64::from(random) % DEFAULT_JITTER_WINDOW_SECS
}

#[derive(Debug)]
pub(crate) struct HealthMachine {
    expects_graph: bool,
    rejuvenation_threshold_secs: i64,
    graph_deadline_secs: u64,
    cassini_connected: bool,
    graph_connected: bool,
    phase: RejuvenationPhase,
    /// Unix seconds of the last `GraphAvailable`; `None` if never seen.
    last_graph_success_secs: Option<u64>,
    /// Unix seconds the machine started; anchors the "graph never came up" deadline.
    start_secs: u64,
    /// Health file writes that have failed since the last one that worked.
    write_failures: u32,
    /// The verdict, once one has been given. After it the machine judges nothing
    /// more: it only keeps the health file fresh while the agent shuts down.
    concluded: Option<HealthVerdict>,
}

impl HealthMachine {
    pub(crate) fn new(config: &HealthConfig, now: u64) -> Self {
        Self {
            expects_graph: config.expects_graph,
            rejuvenation_threshold_secs: config.rejuvenation_threshold_secs,
            graph_deadline_secs: config.graph_deadline_secs,
            cassini_connected: false,
            graph_connected: false,
            phase: RejuvenationPhase::Normal,
            last_graph_success_secs: None,
            start_secs: now,
            write_failures: 0,
            concluded: None,
        }
    }

    /// Records that a verdict has been given. The actor calls this for a verdict
    /// it raised itself (a health file that cannot be written); the machine does
    /// it for the ones it emits.
    pub(crate) fn conclude(&mut self, verdict: &HealthVerdict) {
        self.concluded = Some(verdict.clone());
    }

    /// The phase to put in the health file.
    fn file_phase(&self) -> HealthPhase {
        if self.concluded.is_some() {
            HealthPhase::ShuttingDown
        } else {
            HealthPhase::from(&self.phase)
        }
    }

    /// Records how a health file write went. Returns the reason to fail the actor
    /// once [`MAX_CONSECUTIVE_WRITE_FAILURES`] writes in a row have failed.
    pub(crate) fn record_write(&mut self, result: Result<(), String>) -> Option<String> {
        match result {
            Ok(()) => {
                self.write_failures = 0;
                None
            }
            Err(reason) => {
                self.write_failures += 1;
                if self.concluded.is_some() {
                    return None;
                }
                (self.write_failures >= MAX_CONSECUTIVE_WRITE_FAILURES).then(|| {
                    format!(
                        "health file could not be written {} times in a row: {reason}",
                        self.write_failures
                    )
                })
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn phase(&self) -> &RejuvenationPhase {
        &self.phase
    }

    fn unhealthy(&self, now: u64, cassini: bool, graph: bool) -> HealthState {
        HealthState::at(crate::HealthStatus::Unhealthy, now, cassini, graph)
            .with_phase(self.file_phase())
    }

    fn healthy(&self, now: u64) -> HealthState {
        HealthState::at(
            crate::HealthStatus::Healthy,
            now,
            self.cassini_connected,
            self.graph_connected,
        )
        .with_phase(self.file_phase())
    }

    /// The state to write on a tick that has no other news: healthy or unhealthy
    /// according to the connections, as in the Normal phase. Every tick writes
    /// one, in every phase, because the file is the liveness probe's only evidence
    /// that the agent is still running.
    fn current_state(&self, now: u64) -> HealthState {
        if self.connections_healthy() {
            self.healthy(now)
        } else {
            self.unhealthy(now, self.cassini_connected, self.graph_connected)
        }
    }

    /// Applies a connection or graph message. Returns the state to write, if the
    /// message calls for one. `Tick` is not handled here (see [`tick`](Self::tick)).
    pub(crate) fn on_message(&mut self, msg: &HealthCheckMessage, now: u64) -> Option<HealthState> {
        match msg {
            HealthCheckMessage::CassiniConnected => {
                self.cassini_connected = true;
                tracing::debug!("healthcheck: cassini connected");
                None
            }
            HealthCheckMessage::CassiniDisconnected => {
                self.cassini_connected = false;
                tracing::warn!("healthcheck: cassini disconnected");
                // Don't fail: the TCP client will attempt to reconnect. The tick
                // catches it if reconnection does not succeed.
                Some(self.unhealthy(now, false, self.graph_connected))
            }
            HealthCheckMessage::GraphConnected => {
                self.graph_connected = true;
                tracing::debug!("healthcheck: graph connected (actor spawned; not yet verified)");
                None
            }
            HealthCheckMessage::GraphDisconnected => {
                self.graph_connected = false;
                tracing::warn!("healthcheck: graph disconnected");
                // Don't fail: allow reconnection to recover.
                self.expects_graph
                    .then(|| self.unhealthy(now, self.cassini_connected, false))
            }
            HealthCheckMessage::GraphAvailable => {
                self.graph_connected = true;
                self.last_graph_success_secs = Some(now);
                tracing::debug!("healthcheck: graph available (real op succeeded)");
                None
            }
            HealthCheckMessage::GraphOpFailed(reason) => {
                tracing::warn!("healthcheck: graph op failed: {reason}");
                // Not immediately fatal: a single failed op could be transient.
                // The graph deadline, checked every tick, decides when it has
                // gone on too long.
                Some(self.unhealthy(now, self.cassini_connected, self.graph_connected))
            }
            HealthCheckMessage::Tick => None,
        }
    }

    /// All required connections are up. Uses `graph_connected` (the graph actor
    /// exists), not graph availability, which the deadline tracks separately.
    fn connections_healthy(&self) -> bool {
        if !self.cassini_connected {
            return false;
        }
        if self.expects_graph && !self.graph_connected {
            return false;
        }
        true
    }

    /// `Err` when an `expects_graph` agent has gone longer than the deadline
    /// without a real graph success, whether it never connected or went dark.
    fn check_graph_deadline(&self, now: u64) -> Result<(), String> {
        if !self.expects_graph {
            return Ok(());
        }

        match self.last_graph_success_secs {
            Some(last_success) => {
                let elapsed = now.saturating_sub(last_success);
                if elapsed > self.graph_deadline_secs {
                    return Err(format!(
                        "graph unavailable for {elapsed}s (deadline: {}s)",
                        self.graph_deadline_secs
                    ));
                }
            }
            None => {
                let elapsed = now.saturating_sub(self.start_secs);
                if elapsed > self.graph_deadline_secs {
                    return Err(format!(
                        "graph never became available within {elapsed}s (deadline: {}s)",
                        self.graph_deadline_secs
                    ));
                }
            }
        }
        Ok(())
    }

    /// Gives an unhealthy verdict. The file is written as shutting down, with the
    /// graph reported as `graph` (a graph deadline reports it down).
    fn unhealthy_verdict(&mut self, now: u64, reason: String, graph: bool) -> TickStep {
        let verdict = HealthVerdict::Unhealthy(reason);
        self.conclude(&verdict);
        TickStep::Done(TickOutcome {
            write: Some(self.unhealthy(now, self.cassini_connected, graph)),
            next: Next::Verdict(verdict),
        })
    }

    /// The first half of a tick: the hard deadlines, then the phase.
    pub(crate) fn tick(&mut self, now: u64, certs: &CertReading) -> TickStep {
        // Once the verdict is given there is nothing left to judge. The agent is
        // shutting down, and the health file must stay fresh until it has.
        if let Some(verdict) = &self.concluded {
            let write = match verdict {
                HealthVerdict::Rejuvenate => self.current_state(now),
                HealthVerdict::Unhealthy(_) => {
                    self.unhealthy(now, self.cassini_connected, self.graph_connected)
                }
            };
            return TickStep::Done(TickOutcome {
                write: Some(write),
                next: Next::Reschedule,
            });
        }

        // Hard deadlines come first, in every phase.
        if let Err(reason) = &certs.hard {
            tracing::error!("healthcheck: cert check failed (hard deadline): {reason}");
            return self.unhealthy_verdict(now, reason.clone(), self.graph_connected);
        }

        if let Err(reason) = self.check_graph_deadline(now) {
            tracing::error!("healthcheck: {reason}");
            return self.unhealthy_verdict(now, reason, false);
        }

        match self.phase.clone() {
            RejuvenationPhase::Normal => {
                if !self.connections_healthy() {
                    tracing::warn!(
                        "healthcheck: connections unhealthy (cassini={} graph={} expects_graph={}), waiting for recovery",
                        self.cassini_connected,
                        self.graph_connected,
                        self.expects_graph
                    );
                    // Don't fail: reschedule and wait for recovery.
                    return TickStep::Done(TickOutcome {
                        write: Some(self.unhealthy(
                            now,
                            self.cassini_connected,
                            self.graph_connected,
                        )),
                        next: Next::Reschedule,
                    });
                }

                match certs.min_ttl {
                    None => {
                        tracing::warn!(
                            "healthcheck: could not determine cert TTL, \
                             skipping rejuvenation window check"
                        );
                    }
                    Some(min_ttl) => {
                        if min_ttl <= self.rejuvenation_threshold_secs {
                            tracing::info!(
                                "healthcheck: entering rejuvenation mode \
                                 (cert TTL {min_ttl}s <= threshold {}s)",
                                self.rejuvenation_threshold_secs
                            );
                            self.phase = RejuvenationPhase::WaitingForDeps;
                        }
                    }
                }

                TickStep::Done(TickOutcome {
                    write: Some(self.healthy(now)),
                    next: Next::Reschedule,
                })
            }

            RejuvenationPhase::WaitingForDeps => TickStep::NeedDeps,

            RejuvenationPhase::Jittering { fire_at_secs } => {
                if now >= fire_at_secs {
                    tracing::info!("healthcheck: jitter elapsed, rejuvenating");
                    let verdict = HealthVerdict::Rejuvenate;
                    self.conclude(&verdict);
                    TickStep::Done(TickOutcome {
                        write: Some(self.current_state(now)),
                        next: Next::Verdict(verdict),
                    })
                } else {
                    let remaining = fire_at_secs.saturating_sub(now);
                    tracing::debug!(
                        "healthcheck: jitter pending, {remaining}s until shutdown signal"
                    );
                    TickStep::Done(TickOutcome {
                        write: Some(self.current_state(now)),
                        next: Next::Reschedule,
                    })
                }
            }
        }
    }

    /// The second half of a `WaitingForDeps` tick, once the dependencies have been
    /// checked. `jitter_secs` is the delay to wait if they are healthy.
    pub(crate) fn deps_checked(
        &mut self,
        now: u64,
        deps_healthy: bool,
        jitter_secs: u64,
    ) -> TickOutcome {
        debug_assert_eq!(self.phase, RejuvenationPhase::WaitingForDeps);

        if deps_healthy {
            // The jitter window must dominate the tick, or every agent that enters
            // this phase around the same time (after a shared cert-issuer rotation)
            // fires within one tick of each other whatever jitter it draws: a
            // thundering herd on cert-issuer.
            let fire_at_secs = now.checked_add(jitter_secs).unwrap_or(now);
            tracing::info!(
                "healthcheck: deps healthy, jitter {jitter_secs}s (window {}s), \
                 will signal shutdown at t={fire_at_secs}",
                DEFAULT_JITTER_WINDOW_SECS
            );
            self.phase = RejuvenationPhase::Jittering { fire_at_secs };
        } else {
            tracing::info!("healthcheck: rejuvenation — waiting for dep certs to become healthy");
        }

        TickOutcome {
            write: Some(self.current_state(now)),
            next: Next::Reschedule,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HealthPhase, HealthStatus, HealthVerdict};

    const T0: u64 = 1_000_000;

    fn config(expects_graph: bool) -> HealthConfig {
        HealthConfig {
            expects_graph,
            rejuvenation_threshold_secs: 300,
            expiry_threshold_secs: 60,
            tick_secs: 30,
            graph_deadline_secs: 120,
            cert_paths: vec![],
            dep_cert_endpoints: vec![],
        }
    }

    fn machine(expects_graph: bool) -> HealthMachine {
        HealthMachine::new(&config(expects_graph), T0)
    }

    /// A machine whose connections are all up.
    fn connected(expects_graph: bool) -> HealthMachine {
        let mut m = machine(expects_graph);
        m.on_message(&HealthCheckMessage::CassiniConnected, T0);
        if expects_graph {
            m.on_message(&HealthCheckMessage::GraphAvailable, T0);
        }
        m
    }

    fn certs(min_ttl: Option<i64>) -> CertReading {
        CertReading {
            hard: Ok(()),
            min_ttl,
        }
    }

    fn done(step: TickStep) -> TickOutcome {
        match step {
            TickStep::Done(o) => o,
            TickStep::NeedDeps => panic!("expected the tick to finish, but it needs deps"),
        }
    }

    // -- messages -------------------------------------------------------------

    #[test]
    fn connecting_writes_nothing() {
        let mut m = machine(true);
        for msg in [
            HealthCheckMessage::CassiniConnected,
            HealthCheckMessage::GraphConnected,
            HealthCheckMessage::GraphAvailable,
        ] {
            assert!(m.on_message(&msg, T0).is_none(), "{msg:?}");
        }
    }

    #[test]
    fn a_cassini_disconnect_writes_unhealthy_at_once() {
        let mut m = connected(true);
        let w = m
            .on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 7)
            .unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert_eq!(w.timestamp, T0 + 7);
        assert!(!w.cassini_connected);
        assert!(w.graph_connected);
    }

    #[test]
    fn a_graph_disconnect_writes_only_for_agents_that_expect_a_graph() {
        let mut with = connected(true);
        let w = with
            .on_message(&HealthCheckMessage::GraphDisconnected, T0)
            .unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert!(w.cassini_connected && !w.graph_connected);

        let mut without = connected(false);
        assert!(
            without
                .on_message(&HealthCheckMessage::GraphDisconnected, T0)
                .is_none()
        );
    }

    #[test]
    fn a_failed_graph_op_writes_unhealthy_without_changing_connections() {
        let mut m = connected(true);
        let w = m
            .on_message(&HealthCheckMessage::GraphOpFailed("boom".into()), T0)
            .unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert!(w.cassini_connected && w.graph_connected);
    }

    // -- the Normal phase -----------------------------------------------------

    #[test]
    fn a_normal_tick_writes_healthy_and_reschedules() {
        let mut m = connected(false);
        let o = done(m.tick(T0 + 30, &certs(Some(3000))));
        let w = o.write.unwrap();
        assert_eq!(w.status, HealthStatus::Healthy);
        assert_eq!(w.timestamp, T0 + 30);
        assert_eq!(o.next, Next::Reschedule);
        assert_eq!(m.phase(), &RejuvenationPhase::Normal);
    }

    #[test]
    fn a_normal_tick_without_cassini_writes_unhealthy_and_reschedules() {
        let mut m = machine(false);
        let o = done(m.tick(T0 + 30, &certs(Some(3000))));
        let w = o.write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert!(!w.cassini_connected);
        assert_eq!(o.next, Next::Reschedule);
    }

    #[test]
    fn a_normal_tick_without_the_expected_graph_writes_unhealthy_and_reschedules() {
        let mut m = machine(true);
        m.on_message(&HealthCheckMessage::CassiniConnected, T0);
        // The graph actor never reported in, but the deadline has not passed yet.
        let o = done(m.tick(T0 + 30, &certs(Some(3000))));
        let w = o.write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert!(w.cassini_connected && !w.graph_connected);
        assert_eq!(o.next, Next::Reschedule);
    }

    #[test]
    fn a_graph_that_is_not_expected_does_not_need_to_be_connected() {
        let mut m = connected(false);
        let w = done(m.tick(T0 + 30, &certs(Some(3000)))).write.unwrap();
        assert_eq!(w.status, HealthStatus::Healthy);
        assert!(!w.graph_connected);
    }

    #[test]
    fn rejuvenation_starts_when_the_cert_ttl_reaches_the_threshold() {
        let mut just_outside = connected(false);
        done(just_outside.tick(T0 + 30, &certs(Some(301))));
        assert_eq!(just_outside.phase(), &RejuvenationPhase::Normal);

        let mut on_the_line = connected(false);
        let o = done(on_the_line.tick(T0 + 30, &certs(Some(300))));
        assert_eq!(on_the_line.phase(), &RejuvenationPhase::WaitingForDeps);
        // The tick that enters the window still reports healthy.
        assert_eq!(o.write.unwrap().status, HealthStatus::Healthy);
    }

    #[test]
    fn an_unknown_cert_ttl_never_starts_rejuvenation() {
        let mut m = connected(false);
        let o = done(m.tick(T0 + 30, &certs(None)));
        assert_eq!(m.phase(), &RejuvenationPhase::Normal);
        assert_eq!(o.write.unwrap().status, HealthStatus::Healthy);
    }

    #[test]
    fn rejuvenation_is_not_considered_while_a_connection_is_down() {
        let mut m = machine(false);
        done(m.tick(T0 + 30, &certs(Some(10))));
        assert_eq!(m.phase(), &RejuvenationPhase::Normal);
    }

    // -- hard deadlines -------------------------------------------------------

    #[test]
    fn a_hard_cert_failure_fails_the_actor_and_writes_unhealthy() {
        let mut m = connected(false);
        let reading = CertReading {
            hard: Err("cert expires soon".to_string()),
            min_ttl: Some(10),
        };
        let o = done(m.tick(T0 + 30, &reading));
        assert_eq!(
            o.next,
            Next::Verdict(HealthVerdict::Unhealthy("cert expires soon".to_string()))
        );
        let w = o.write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert!(w.cassini_connected);
    }

    #[test]
    fn a_hard_cert_failure_wins_in_every_phase() {
        let bad = CertReading {
            hard: Err("expired".to_string()),
            min_ttl: None,
        };
        for phase in [
            RejuvenationPhase::Normal,
            RejuvenationPhase::WaitingForDeps,
            RejuvenationPhase::Jittering {
                fire_at_secs: T0 + 1000,
            },
        ] {
            let mut m = connected(false);
            m.phase = phase.clone();
            let o = done(m.tick(T0 + 30, &bad));
            assert_eq!(
                o.next,
                Next::Verdict(HealthVerdict::Unhealthy("expired".to_string())),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn the_graph_deadline_fails_the_actor_and_writes_graph_down() {
        let mut m = machine(true);
        m.on_message(&HealthCheckMessage::CassiniConnected, T0);
        m.on_message(&HealthCheckMessage::GraphConnected, T0);
        let o = done(m.tick(T0 + 121, &certs(Some(3000))));
        match o.next {
            Next::Verdict(HealthVerdict::Unhealthy(reason)) => {
                assert!(reason.contains("never became available"), "{reason}")
            }
            other => panic!("expected an Unhealthy verdict, got {other:?}"),
        }
        let w = o.write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert!(!w.graph_connected, "the deadline reports the graph as down");
    }

    #[test]
    fn graph_deadline_is_ignored_when_no_graph_is_expected() {
        let m = machine(false);
        assert!(m.check_graph_deadline(T0 + 10_000).is_ok());
    }

    #[test]
    fn graph_deadline_holds_after_a_recent_success() {
        let mut m = machine(true);
        m.on_message(&HealthCheckMessage::GraphAvailable, T0 + 1000);
        assert!(m.check_graph_deadline(T0 + 1100).is_ok());
    }

    #[test]
    fn graph_deadline_fires_after_the_graph_goes_dark() {
        let mut m = machine(true);
        m.on_message(&HealthCheckMessage::GraphAvailable, T0 + 1000);
        let err = m.check_graph_deadline(T0 + 1121).unwrap_err();
        assert!(err.contains("graph unavailable"), "{err}");
    }

    #[test]
    fn graph_deadline_fires_when_the_graph_never_came_up() {
        let err = machine(true).check_graph_deadline(T0 + 121).unwrap_err();
        assert!(err.contains("never became available"), "{err}");
    }

    #[test]
    fn graph_deadline_boundary_is_exclusive() {
        assert!(machine(true).check_graph_deadline(T0 + 120).is_ok());
        assert!(machine(true).check_graph_deadline(T0 + 121).is_err());
    }

    // -- rejuvenation ---------------------------------------------------------

    #[test]
    fn waiting_for_deps_asks_the_caller_to_check_them() {
        let mut m = connected(false);
        m.phase = RejuvenationPhase::WaitingForDeps;
        assert!(matches!(
            m.tick(T0 + 30, &certs(Some(200))),
            TickStep::NeedDeps
        ));
    }

    #[test]
    fn unhealthy_deps_keep_waiting() {
        let mut m = connected(false);
        m.phase = RejuvenationPhase::WaitingForDeps;
        let o = m.deps_checked(T0 + 30, false, 100);
        assert_eq!(o.next, Next::Reschedule);
        assert_eq!(m.phase(), &RejuvenationPhase::WaitingForDeps);
    }

    #[test]
    fn healthy_deps_start_the_jitter() {
        let mut m = connected(false);
        m.phase = RejuvenationPhase::WaitingForDeps;
        let o = m.deps_checked(T0 + 30, true, 100);
        assert_eq!(o.next, Next::Reschedule);
        assert_eq!(
            m.phase(),
            &RejuvenationPhase::Jittering {
                fire_at_secs: T0 + 130
            }
        );
    }

    #[test]
    fn zero_jitter_fires_on_the_next_tick() {
        let mut m = connected(false);
        m.phase = RejuvenationPhase::WaitingForDeps;
        m.deps_checked(T0 + 30, true, 0);
        let o = done(m.tick(T0 + 60, &certs(Some(200))));
        assert_eq!(o.next, Next::Verdict(HealthVerdict::Rejuvenate));
    }

    #[test]
    fn jittering_waits_until_the_fire_time_then_stops() {
        let mut m = connected(false);
        m.phase = RejuvenationPhase::Jittering {
            fire_at_secs: T0 + 100,
        };
        assert_eq!(
            done(m.tick(T0 + 99, &certs(Some(200)))).next,
            Next::Reschedule
        );
        assert_eq!(
            done(m.tick(T0 + 100, &certs(Some(200)))).next,
            Next::Verdict(HealthVerdict::Rejuvenate),
            "now == fire_at fires"
        );
    }

    #[test]
    fn jitter_is_always_inside_the_window() {
        for random in [0, 1, 179, 180, 181, u32::MAX] {
            assert!(jitter_secs(random) < DEFAULT_JITTER_WINDOW_SECS);
        }
        assert_eq!(jitter_secs(0), 0);
        assert_eq!(jitter_secs(181), 1);
    }

    #[test]
    fn rejuvenation_ticks_still_report_the_connection_state() {
        for phase in [
            RejuvenationPhase::WaitingForDeps,
            RejuvenationPhase::Jittering {
                fire_at_secs: T0 + 1000,
            },
        ] {
            // Connected: healthy.
            let mut up = connected(false);
            up.phase = phase.clone();
            let o = match up.tick(T0 + 30, &certs(Some(200))) {
                TickStep::Done(o) => o,
                TickStep::NeedDeps => up.deps_checked(T0 + 30, false, 0),
            };
            assert_eq!(o.write.unwrap().status, HealthStatus::Healthy, "{phase:?}");

            // Cassini down: unhealthy, still written.
            let mut down = connected(false);
            down.on_message(&HealthCheckMessage::CassiniDisconnected, T0);
            down.phase = phase.clone();
            let o = match down.tick(T0 + 30, &certs(Some(200))) {
                TickStep::Done(o) => o,
                TickStep::NeedDeps => down.deps_checked(T0 + 30, false, 0),
            };
            let w = o.write.expect("a write");
            assert_eq!(w.status, HealthStatus::Unhealthy, "{phase:?}");
            assert!(!w.cassini_connected);
        }
    }

    #[test]
    fn every_written_state_names_the_phase_it_was_written_in() {
        // Normal.
        let mut m = connected(false);
        let w = done(m.tick(T0 + 30, &certs(Some(3000)))).write.unwrap();
        assert_eq!(w.phase, HealthPhase::Normal);

        // The tick that enters the window already reports WaitingForDeps.
        let w = done(m.tick(T0 + 60, &certs(Some(200)))).write.unwrap();
        assert_eq!(w.phase, HealthPhase::WaitingForDeps);

        // WaitingForDeps, then the tick that starts the jitter reports Jittering.
        let w = m.deps_checked(T0 + 90, false, 0).write.unwrap();
        assert_eq!(w.phase, HealthPhase::WaitingForDeps);
        let w = m.deps_checked(T0 + 120, true, 100).write.unwrap();
        assert_eq!(w.phase, HealthPhase::Jittering);
        let w = done(m.tick(T0 + 150, &certs(Some(200)))).write.unwrap();
        assert_eq!(w.phase, HealthPhase::Jittering);
    }

    #[test]
    fn messages_and_failures_also_carry_the_phase() {
        let mut m = connected(false);
        m.phase = RejuvenationPhase::Jittering {
            fire_at_secs: T0 + 100,
        };
        let w = m
            .on_message(&HealthCheckMessage::CassiniDisconnected, T0)
            .unwrap();
        assert_eq!(w.phase, HealthPhase::Jittering);

        // A hard failure is a verdict, so the file it writes already says the agent
        // is shutting down.
        let bad = CertReading {
            hard: Err("expired".into()),
            min_ttl: None,
        };
        let w = done(m.tick(T0 + 30, &bad)).write.unwrap();
        assert_eq!(w.phase, HealthPhase::ShuttingDown);
    }

    #[test]
    fn write_failures_are_tolerated_until_the_third_in_a_row() {
        let mut m = machine(false);
        assert_eq!(m.record_write(Err("disk full".into())), None);
        assert_eq!(m.record_write(Err("disk full".into())), None);
        let reason = m
            .record_write(Err("disk full".into()))
            .expect("third fails");
        assert!(reason.contains("3 times in a row"), "{reason}");
        assert!(reason.contains("disk full"), "{reason}");
    }

    #[test]
    fn a_successful_write_resets_the_failure_count() {
        let mut m = machine(false);
        for _ in 0..2 {
            assert_eq!(m.record_write(Err("x".into())), None);
        }
        assert_eq!(m.record_write(Ok(())), None);
        for _ in 0..2 {
            assert_eq!(m.record_write(Err("x".into())), None);
        }
    }

    // -- verdicts -------------------------------------------------------------

    /// A machine one tick from a rejuvenation verdict.
    fn about_to_rejuvenate() -> HealthMachine {
        let mut m = connected(false);
        m.phase = RejuvenationPhase::Jittering { fire_at_secs: T0 };
        m
    }

    #[test]
    fn a_verdict_is_followed_by_a_write_that_says_the_agent_is_shutting_down() {
        let mut m = about_to_rejuvenate();
        let o = done(m.tick(T0 + 30, &certs(Some(200))));
        assert_eq!(o.next, Next::Verdict(HealthVerdict::Rejuvenate));
        let w = o.write.expect("the verdict tick still refreshes the file");
        assert_eq!(w.phase, HealthPhase::ShuttingDown);
        assert_eq!(
            w.status,
            HealthStatus::Healthy,
            "rejuvenating is not unhealthy"
        );
    }

    #[test]
    fn an_unhealthy_verdict_writes_unhealthy_while_shutting_down() {
        let mut m = connected(false);
        let bad = CertReading {
            hard: Err("expired".into()),
            min_ttl: None,
        };
        let w = done(m.tick(T0 + 30, &bad)).write.unwrap();
        assert_eq!(w.phase, HealthPhase::ShuttingDown);
        assert_eq!(w.status, HealthStatus::Unhealthy);
    }

    #[test]
    fn after_a_verdict_the_machine_keeps_the_file_fresh_and_judges_nothing_more() {
        let mut m = about_to_rejuvenate();
        done(m.tick(T0 + 30, &certs(Some(200))));

        // Even a certificate that has since expired produces no second verdict.
        let bad = CertReading {
            hard: Err("expired".into()),
            min_ttl: None,
        };
        for i in 2..6 {
            let o = done(m.tick(T0 + 30 * i, &bad));
            assert_eq!(o.next, Next::Reschedule, "tick {i}");
            let w = o.write.expect("still refreshed");
            assert_eq!(w.timestamp, T0 + 30 * i);
            assert_eq!(w.phase, HealthPhase::ShuttingDown);
        }
    }

    #[test]
    fn a_rejuvenation_verdict_follows_the_connections_afterwards() {
        let mut m = about_to_rejuvenate();
        done(m.tick(T0 + 30, &certs(Some(200))));
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 40);
        let w = done(m.tick(T0 + 60, &certs(Some(200)))).write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert_eq!(w.phase, HealthPhase::ShuttingDown);
    }

    #[test]
    fn an_unhealthy_verdict_stays_unhealthy_afterwards_whatever_the_connections() {
        let mut m = connected(false);
        let bad = CertReading {
            hard: Err("expired".into()),
            min_ttl: None,
        };
        done(m.tick(T0 + 30, &bad));
        let w = done(m.tick(T0 + 60, &certs(Some(3000)))).write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
    }

    #[test]
    fn messages_after_a_verdict_are_written_as_shutting_down() {
        let mut m = about_to_rejuvenate();
        done(m.tick(T0 + 30, &certs(Some(200))));
        let w = m
            .on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 40)
            .unwrap();
        assert_eq!(w.phase, HealthPhase::ShuttingDown);
    }

    #[test]
    fn once_concluded_a_failing_write_does_not_start_a_second_verdict() {
        let mut m = about_to_rejuvenate();
        done(m.tick(T0 + 30, &certs(Some(200))));
        for _ in 0..10 {
            assert_eq!(m.record_write(Err("disk full".into())), None);
        }
    }

    #[test]
    fn the_actor_can_conclude_the_machine_itself_for_a_write_failure() {
        let mut m = connected(false);
        m.conclude(&HealthVerdict::Unhealthy("cannot write".into()));
        let w = done(m.tick(T0 + 30, &certs(Some(3000)))).write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert_eq!(w.phase, HealthPhase::ShuttingDown);
    }

    /// The health file is the liveness probe's only evidence that the agent is
    /// alive, so every tick must refresh it, in every phase. Writing it only in
    /// the Normal phase let it go stale during a rejuvenation that waits longer
    /// than the probe's 90 s limit, and the kubelet killed the pod before the
    /// graceful shutdown could run.
    #[test]
    fn rejuvenation_refreshes_the_file_on_every_tick() {
        let mut m = connected(false);
        let mut now = T0;
        let mut written = vec![];
        let mut stopped = false;

        for _ in 0..100 {
            now += 30;
            let outcome = match m.tick(now, &certs(Some(200))) {
                TickStep::Done(o) => o,
                TickStep::NeedDeps => m.deps_checked(now, true, 150),
            };
            if let Next::Verdict(_) = outcome.next {
                stopped = true;
                break;
            }
            match outcome.write {
                Some(w) => written.push(w.timestamp),
                None => panic!(
                    "the tick at +{}s wrote nothing while in {:?}",
                    now - T0,
                    m.phase()
                ),
            }
        }

        assert!(stopped, "the rejuvenation never fired");
        // 150 s of jitter is more than the probe's 90 s limit; every gap between
        // writes, and the gap to the stop, must stay within one tick.
        assert!(written.windows(2).all(|w| w[1] - w[0] == 30), "{written:?}");
        assert!(now - written.last().unwrap() <= 30);
    }
}
