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
use crate::custody::Custody;
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

/// How many ticks in a row the custody daemon may be unreachable before the agent rejuvenates, as for one found empty.
pub(crate) const MAX_CUSTODY_UNREACHABLE_TICKS: u32 = 3;

/// Turns a random number into a delay inside the jitter window.
pub(crate) fn jitter_secs(random: u32) -> u64 {
    u64::from(random) % DEFAULT_JITTER_WINDOW_SECS
}

#[derive(Debug)]
pub(crate) struct HealthMachine {
    expects_graph: bool,
    /// Whether the session counts toward health; when it does not, it is only reported.
    expects_cassini: bool,
    rejuvenation_threshold_secs: i64,
    graph_deadline_secs: u64,
    cassini_deadline_secs: u64,
    cassini_connected: bool,
    /// Since when Cassini has been unreachable: from startup until the first
    /// connect, and from each disconnect until the next connect. `None` while
    /// connected.
    cassini_down_since: Option<u64>,
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
    /// The topics the agent was last told it is refused, carried in every write.
    refused: Vec<crate::RefusedTopic>,
    /// Ticks in a row the custody daemon could not be asked.
    custody_unreachable_ticks: u32,
}

impl HealthMachine {
    pub(crate) fn new(config: &HealthConfig, now: u64) -> Self {
        Self {
            expects_graph: config.expects_graph,
            expects_cassini: config.expects_cassini,
            rejuvenation_threshold_secs: config.rejuvenation_threshold_secs,
            graph_deadline_secs: config.graph_deadline_secs,
            cassini_deadline_secs: config.cassini_deadline_secs,
            cassini_connected: false,
            cassini_down_since: Some(now),
            graph_connected: false,
            phase: RejuvenationPhase::Normal,
            last_graph_success_secs: None,
            start_secs: now,
            write_failures: 0,
            concluded: None,
            refused: Vec::new(),
            custody_unreachable_ticks: 0,
        }
    }

    /// Why the custody daemon means the agent must rejuvenate, if it does (docs/architecture/key-custody.md, step 4,
    /// item 6): it is empty, having restarted, or it has been unreachable for too many ticks. Either way the pod can
    /// make no new connection, and the agent ends in order and is replaced, waiting first for what issues its
    /// replacement. The agent's health actor decides this, not the Kubernetes probe.
    fn custody_rejuvenation(&mut self, custody: &Custody) -> Option<String> {
        match custody {
            Custody::NotUsed | Custody::Holds => {
                self.custody_unreachable_ticks = 0;
                None
            }
            Custody::Empty => Some(
                "the custody daemon holds no certificate: it restarted, and this pod can make no new connection"
                    .to_string(),
            ),
            Custody::Unreachable(why) => {
                self.custody_unreachable_ticks += 1;
                if self.custody_unreachable_ticks >= MAX_CUSTODY_UNREACHABLE_TICKS {
                    Some(format!(
                        "the custody daemon could not be asked {} times in a row: {why}",
                        self.custody_unreachable_ticks
                    ))
                } else {
                    tracing::warn!(
                        "healthcheck: the custody daemon could not be asked ({}): {why}",
                        self.custody_unreachable_ticks
                    );
                    None
                }
            }
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
            .with_refused(self.refused.clone())
    }

    fn healthy(&self, now: u64) -> HealthState {
        HealthState::at(
            crate::HealthStatus::Healthy,
            now,
            self.cassini_connected,
            self.graph_connected,
        )
        .with_phase(self.file_phase())
        .with_refused(self.refused.clone())
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
                self.cassini_down_since = None;
                tracing::debug!("healthcheck: cassini connected");
                None
            }
            HealthCheckMessage::CassiniDisconnected => {
                self.cassini_connected = false;
                // A repeated disconnect does not restart the outage.
                self.cassini_down_since.get_or_insert(now);
                tracing::warn!("healthcheck: cassini disconnected");
                // Don't fail: the TCP client will attempt to reconnect. The tick
                // catches it if reconnection does not succeed. An agent that does not
                // expect Cassini is not made unhealthy by it; its next write reports it.
                self.expects_cassini
                    .then(|| self.unhealthy(now, false, self.graph_connected))
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
            // News for the next write, which every tick makes; it judges nothing.
            HealthCheckMessage::TopicsRefused(topics) => {
                self.refused = topics.clone();
                None
            }
            HealthCheckMessage::Tick | HealthCheckMessage::DepsChecked(_) => None,
        }
    }

    /// All required connections are up. Uses `graph_connected` (the graph actor
    /// exists), not graph availability, which the deadline tracks separately.
    fn connections_healthy(&self) -> bool {
        if self.expects_cassini && !self.cassini_connected {
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

    /// `Err` when an agent that expects Cassini has had it unreachable for longer than the deadline.
    fn check_cassini_deadline(&self, now: u64) -> Result<(), String> {
        if !self.expects_cassini {
            return Ok(());
        }
        match self.cassini_down_since {
            Some(since) => {
                let elapsed = now.saturating_sub(since);
                if elapsed > self.cassini_deadline_secs {
                    return Err(format!(
                        "cassini unreachable for {elapsed}s (deadline: {}s)",
                        self.cassini_deadline_secs
                    ));
                }
                Ok(())
            }
            None => Ok(()),
        }
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

        if let Err(reason) = self.check_cassini_deadline(now) {
            tracing::error!("healthcheck: {reason}");
            return self.unhealthy_verdict(now, reason, self.graph_connected);
        }

        match self.phase.clone() {
            RejuvenationPhase::Normal => {
                // Before the connections: an empty daemon may be why they are down, and they will not come back.
                if let Some(why) = self.custody_rejuvenation(&certs.custody) {
                    tracing::warn!("healthcheck: entering rejuvenation mode ({why})");
                    self.phase = RejuvenationPhase::WaitingForDeps;
                    return TickStep::Done(TickOutcome {
                        write: Some(self.healthy(now)),
                        next: Next::Reschedule,
                    });
                }
                if !self.connections_healthy() {
                    tracing::warn!(
                        "healthcheck: connections unhealthy (cassini={} graph={} expects_cassini={} expects_graph={}), waiting for recovery",
                        self.cassini_connected,
                        self.graph_connected,
                        self.expects_cassini,
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
            expects_cassini: true,
            rejuvenation_threshold_secs: 300,
            expiry_threshold_secs: 60,
            tick_secs: 30,
            graph_deadline_secs: 120,
            cassini_deadline_secs: 60,
            probe_stale_secs: 90,
            file_path: String::new(),
            cert_paths: vec![],
            dep_cert_endpoints: vec![],
            custody_socket: None,
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
            custody: crate::custody::Custody::NotUsed,
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

    // -- an agent whose health does not count Cassini ---------------------------

    /// A machine for an agent with no dependency on Cassini that still speaks over it, as cert-issuer does
    /// (docs/architecture/cert-issuer-isolation.md, "The model", revised 2026-10-23).
    fn independent(expects_graph: bool) -> HealthMachine {
        HealthMachine::new(
            &HealthConfig {
                expects_cassini: false,
                ..config(expects_graph)
            },
            T0,
        )
    }

    #[test]
    fn a_cassini_disconnect_writes_nothing_for_an_agent_that_does_not_expect_cassini() {
        let mut m = independent(false);
        m.on_message(&HealthCheckMessage::CassiniConnected, T0);
        assert!(
            m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 7)
                .is_none()
        );
    }

    #[test]
    fn an_agent_that_does_not_expect_cassini_stays_healthy_past_the_deadline_without_it() {
        // Never connected, and connected then lost: either way well past the 60 s deadline.
        let mut lost = independent(false);
        lost.on_message(&HealthCheckMessage::CassiniConnected, T0);
        lost.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 1);
        for (label, mut m) in [("never connected", independent(false)), ("lost", lost)] {
            let o = done(m.tick(T0 + 600, &certs(Some(10_000))));
            let w = o.write.unwrap();
            assert_eq!(w.status, HealthStatus::Healthy, "{label}");
            assert!(
                !w.cassini_connected,
                "{label}: the file still says the session is down"
            );
            assert!(matches!(o.next, Next::Reschedule), "{label}");
        }

        // The same, for an agent that expects Cassini, is the deadline's verdict.
        let mut expecting = machine(false);
        let o = done(expecting.tick(T0 + 600, &certs(Some(10_000))));
        assert!(
            matches!(&o.next, Next::Verdict(HealthVerdict::Unhealthy(r)) if r.contains("cassini")),
            "{:?}",
            o.next
        );
    }

    #[test]
    fn an_agent_that_does_not_expect_cassini_still_rejuvenates_without_it() {
        let mut m = independent(false);
        // A certificate inside the rejuvenation threshold (300 s): into rejuvenation.
        let o = done(m.tick(T0 + 30, &certs(Some(100))));
        assert_eq!(o.write.unwrap().status, HealthStatus::Healthy);
        assert!(matches!(
            m.tick(T0 + 60, &certs(Some(70))),
            TickStep::NeedDeps
        ));
        m.deps_checked(T0 + 60, true, 0);
        let o = done(m.tick(T0 + 90, &certs(Some(40))));
        assert!(
            matches!(o.next, Next::Verdict(HealthVerdict::Rejuvenate)),
            "{:?}",
            o.next
        );
    }

    // -- the custody daemon (docs/architecture/key-custody.md, step 4, item 6) ---------------------------------

    fn reading(custody: Custody) -> CertReading {
        CertReading {
            custody,
            ..certs(Some(10_000))
        }
    }

    #[test]
    fn a_daemon_holding_its_certificate_changes_nothing() {
        let mut m = connected(false);
        for t in 1..5 {
            let o = done(m.tick(T0 + 30 * t, &reading(Custody::Holds)));
            assert_eq!(o.write.unwrap().status, HealthStatus::Healthy);
        }
        assert_eq!(m.phase(), &RejuvenationPhase::Normal);
    }

    // A daemon restarted empty: the pod can make no new connection, ever, and its sessions may already be gone. The
    // agent rejuvenates at once, waiting for what issues its replacement, even with Cassini down.
    #[test]
    fn an_empty_daemon_starts_rejuvenation_at_once_even_with_cassini_down() {
        let mut m = connected(false);
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 1);
        done(m.tick(T0 + 10, &reading(Custody::Empty)));
        assert_eq!(m.phase(), &RejuvenationPhase::WaitingForDeps);
        assert!(matches!(
            m.tick(T0 + 20, &reading(Custody::Empty)),
            TickStep::NeedDeps
        ));
        m.deps_checked(T0 + 20, true, 0);
        let o = done(m.tick(T0 + 30, &reading(Custody::Empty)));
        assert!(
            matches!(o.next, Next::Verdict(HealthVerdict::Rejuvenate)),
            "{:?}",
            o.next
        );
    }

    // The hard deadlines still come first: Cassini down past its deadline while the agent waits to rejuvenate is the
    // deadline's verdict, an orderly end all the same.
    #[test]
    fn cassini_s_deadline_still_comes_first_while_the_agent_waits_to_rejuvenate() {
        let mut m = connected(false);
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 1);
        done(m.tick(T0 + 10, &reading(Custody::Empty)));
        let o = done(m.tick(T0 + 90, &reading(Custody::Empty)));
        assert!(
            matches!(&o.next, Next::Verdict(HealthVerdict::Unhealthy(r)) if r.contains("cassini")),
            "{:?}",
            o.next
        );
    }

    #[test]
    fn an_unreachable_daemon_is_borne_for_two_ticks_and_rejuvenates_on_the_third() {
        let unreachable = || reading(Custody::Unreachable("refused".to_string()));
        let mut m = connected(false);
        done(m.tick(T0 + 30, &unreachable()));
        done(m.tick(T0 + 60, &unreachable()));
        assert_eq!(m.phase(), &RejuvenationPhase::Normal, "borne twice");
        done(m.tick(T0 + 90, &unreachable()));
        assert_eq!(m.phase(), &RejuvenationPhase::WaitingForDeps);

        // Reached in between: the count starts again.
        let mut m = connected(false);
        done(m.tick(T0 + 30, &unreachable()));
        done(m.tick(T0 + 60, &unreachable()));
        done(m.tick(T0 + 90, &reading(Custody::Holds)));
        done(m.tick(T0 + 120, &unreachable()));
        done(m.tick(T0 + 150, &unreachable()));
        assert_eq!(m.phase(), &RejuvenationPhase::Normal);
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
            custody: crate::custody::Custody::NotUsed,
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
            custody: crate::custody::Custody::NotUsed,
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
            custody: crate::custody::Custody::NotUsed,
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

    // -- the Cassini deadline -------------------------------------------------

    fn cassini_verdict(step: TickStep) -> String {
        match done(step).next {
            Next::Verdict(HealthVerdict::Unhealthy(reason)) => reason,
            other => panic!("expected an Unhealthy verdict, got {other:?}"),
        }
    }

    #[test]
    fn cassini_never_connecting_gives_a_verdict_after_the_deadline() {
        let mut m = machine(false);
        assert_eq!(
            done(m.tick(T0 + 60, &certs(Some(3000)))).next,
            Next::Reschedule,
            "exactly at the deadline is still inside it"
        );
        let reason = cassini_verdict(m.tick(T0 + 61, &certs(Some(3000))));
        assert!(reason.contains("cassini unreachable for 61s"), "{reason}");
        assert!(reason.contains("deadline: 60s"), "{reason}");
    }

    #[test]
    fn a_disconnect_starts_the_clock_from_the_disconnect() {
        let mut m = connected(false);
        // Connected for a long time first: the outage is timed from the disconnect.
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 1000);
        assert_eq!(
            done(m.tick(T0 + 1060, &certs(Some(3000)))).next,
            Next::Reschedule
        );
        let reason = cassini_verdict(m.tick(T0 + 1061, &certs(Some(3000))));
        assert!(reason.contains("61s"), "{reason}");
    }

    #[test]
    fn reconnecting_resets_the_clock() {
        let mut m = connected(false);
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 100);
        m.on_message(&HealthCheckMessage::CassiniConnected, T0 + 150);
        assert_eq!(
            done(m.tick(T0 + 400, &certs(Some(3000)))).next,
            Next::Reschedule
        );
        // A later outage is timed afresh.
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 500);
        assert_eq!(
            done(m.tick(T0 + 560, &certs(Some(3000)))).next,
            Next::Reschedule
        );
        assert!(cassini_verdict(m.tick(T0 + 561, &certs(Some(3000)))).contains("61s"));
    }

    #[test]
    fn repeated_disconnects_do_not_restart_the_clock() {
        let mut m = connected(false);
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 100);
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 130);
        m.on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 159);
        assert!(
            cassini_verdict(m.tick(T0 + 161, &certs(Some(3000)))).contains("61s"),
            "timed from the first disconnect"
        );
    }

    #[test]
    fn the_cassini_verdict_writes_unhealthy_and_shutting_down() {
        let mut m = machine(false);
        let w = done(m.tick(T0 + 100, &certs(Some(3000)))).write.unwrap();
        assert_eq!(w.status, HealthStatus::Unhealthy);
        assert_eq!(w.phase, HealthPhase::ShuttingDown);
    }

    #[test]
    fn the_cassini_deadline_applies_in_every_phase() {
        for phase in [
            RejuvenationPhase::Normal,
            RejuvenationPhase::WaitingForDeps,
            RejuvenationPhase::Jittering {
                fire_at_secs: T0 + 10_000,
            },
        ] {
            let mut m = machine(false);
            m.phase = phase.clone();
            let reason = cassini_verdict(m.tick(T0 + 61, &certs(Some(200))));
            assert!(
                reason.contains("cassini unreachable"),
                "{phase:?}: {reason}"
            );
        }
    }

    #[test]
    fn a_hard_certificate_failure_is_reported_before_a_cassini_outage() {
        let mut m = machine(false);
        let bad = CertReading {
            hard: Err("expired".into()),
            min_ttl: None,
            custody: crate::custody::Custody::NotUsed,
        };
        assert_eq!(cassini_verdict(m.tick(T0 + 100, &bad)), "expired");
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
            custody: crate::custody::Custody::NotUsed,
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
            custody: crate::custody::Custody::NotUsed,
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
            custody: crate::custody::Custody::NotUsed,
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

    // -- topics the agent was refused -------------------------------------------

    fn refused(topic: &str) -> crate::RefusedTopic {
        crate::RefusedTopic {
            topic: topic.to_string(),
            access: "subscribe".to_string(),
            reason: format!("policy forbids this agent to subscribe on `{topic}`"),
        }
    }

    #[test]
    fn the_topics_an_agent_was_refused_are_in_every_write_after_and_change_nothing_else() {
        let mut m = connected(false);
        assert!(
            m.on_message(
                &HealthCheckMessage::TopicsRefused(vec![refused("news")]),
                T0 + 1
            )
            .is_none(),
            "a refusal is news for the next write, not a write of its own"
        );
        let w = done(m.tick(T0 + 30, &certs(None))).write.unwrap();
        assert_eq!(w.status, HealthStatus::Healthy);
        assert_eq!(w.refused_topics, [refused("news")]);
        // A disconnect's write carries them too.
        let w = m
            .on_message(&HealthCheckMessage::CassiniDisconnected, T0 + 31)
            .unwrap();
        assert_eq!(w.refused_topics, [refused("news")]);
        // Taken in the end: the next write has none.
        m.on_message(&HealthCheckMessage::TopicsRefused(vec![]), T0 + 32);
        m.on_message(&HealthCheckMessage::CassiniConnected, T0 + 33);
        let w = done(m.tick(T0 + 60, &certs(None))).write.unwrap();
        assert!(w.refused_topics.is_empty());
    }

    #[test]
    fn a_file_without_refused_topics_reads_as_none_and_none_are_not_written() {
        let old =
            r#"{"status":"healthy","timestamp":1,"cassini_connected":true,"graph_connected":true}"#;
        let state: HealthState = serde_json::from_str(old).unwrap();
        assert!(state.refused_topics.is_empty());
        let written = serde_json::to_string(&HealthState::healthy(true, true)).unwrap();
        assert!(!written.contains("refused_topics"), "{written}");
    }

    #[derive(Debug, Clone)]
    enum Event {
        Message(HealthCheckMessage),
        Tick,
        Refused(Vec<crate::RefusedTopic>),
    }

    fn events() -> impl proptest::strategy::Strategy<Value = Vec<(u64, Event)>> {
        use proptest::prelude::*;
        let event = prop_oneof![
            Just(Event::Message(HealthCheckMessage::CassiniConnected)),
            Just(Event::Message(HealthCheckMessage::CassiniDisconnected)),
            Just(Event::Message(HealthCheckMessage::GraphConnected)),
            Just(Event::Message(HealthCheckMessage::GraphDisconnected)),
            Just(Event::Message(HealthCheckMessage::GraphAvailable)),
            Just(Event::Message(HealthCheckMessage::GraphOpFailed(
                "boom".into()
            ))),
            Just(Event::Tick),
            proptest::collection::vec(
                proptest::sample::select(&["news", "weather", "sport"][..]),
                0..3
            )
            .prop_map(|topics| Event::Refused(topics.into_iter().map(refused).collect())),
        ];
        proptest::collection::vec((1u64..40, event), 0..40)
    }

    /// What a machine writes and decides for `events`, with the refusals taken out of each write; and, for each
    /// write, the refusals it carried beside the refusals last told before it.
    #[allow(clippy::type_complexity)]
    fn run(
        expects_graph: bool,
        events: &[(u64, Event)],
    ) -> (
        Vec<String>,
        Vec<(Vec<crate::RefusedTopic>, Vec<crate::RefusedTopic>)>,
    ) {
        run_machine(machine(expects_graph), events, false)
    }

    /// As [`run`], on the machine `m`; with `forget_cassini`, each write's Cassini session is left out, so two runs
    /// can be compared on what they judged alone.
    #[allow(clippy::type_complexity)]
    fn run_machine(
        mut m: HealthMachine,
        events: &[(u64, Event)],
        forget_cassini: bool,
    ) -> (
        Vec<String>,
        Vec<(Vec<crate::RefusedTopic>, Vec<crate::RefusedTopic>)>,
    ) {
        let mut now = T0;
        let mut said = Vec::new();
        let mut carried = Vec::new();
        let mut last: Vec<crate::RefusedTopic> = Vec::new();
        for (gap, event) in events {
            now += gap;
            let (write, next) = match event {
                Event::Message(message) => (m.on_message(message, now), None),
                Event::Refused(topics) => {
                    last = topics.clone();
                    (
                        m.on_message(&HealthCheckMessage::TopicsRefused(topics.clone()), now),
                        None,
                    )
                }
                Event::Tick => {
                    let outcome = match m.tick(now, &certs(None)) {
                        TickStep::Done(outcome) => outcome,
                        TickStep::NeedDeps => m.deps_checked(now, true, 0),
                    };
                    (outcome.write, Some(format!("{:?}", outcome.next)))
                }
            };
            if let Some(mut w) = write {
                if forget_cassini {
                    w.cassini_connected = false;
                }
                carried.push((std::mem::take(&mut w.refused_topics), last.clone()));
                said.push(format!("{w:?}"));
            }
            if let Some(next) = next {
                said.push(next);
            }
        }
        (said, carried)
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

        /// Being refused a topic never makes an agent unhealthy, nor changes anything else the health file or the
        /// verdict says: the same events with the refusals left out are written and decided alike. And every write
        /// carries the refusals last told.
        #[test]
        fn refusals_never_change_what_the_health_says(
            expects_graph in proptest::prelude::any::<bool>(),
            events in events(),
        ) {
            let without: Vec<(u64, Event)> = events
                .iter()
                .map(|(gap, e)| match e {
                    // The time still passes, without the refusal.
                    Event::Refused(_) => (*gap, Event::Message(HealthCheckMessage::Tick)),
                    other => (*gap, other.clone()),
                })
                .collect();
            let (with_said, carried) = run(expects_graph, &events);
            let (without_said, _) = run(expects_graph, &without);
            proptest::prop_assert_eq!(with_said, without_said);
            for (written, told) in carried {
                proptest::prop_assert_eq!(written, told);
            }
        }

        /// An agent that does not expect Cassini is judged as one that expects it and always has it, whatever comes
        /// and goes: every write says the same of its health, and every decision is the same. Only the session its
        /// file reports differs, so that is left out of the comparison.
        #[test]
        fn an_agent_that_does_not_expect_cassini_is_judged_as_if_cassini_were_always_up(
            expects_graph in proptest::prelude::any::<bool>(),
            events in events(),
        ) {
            let up: Vec<(u64, Event)> = events
                .iter()
                .map(|(gap, e)| match e {
                    // The time still passes, without the session changing.
                    Event::Message(
                        HealthCheckMessage::CassiniConnected | HealthCheckMessage::CassiniDisconnected,
                    ) => (*gap, Event::Message(HealthCheckMessage::Tick)),
                    other => (*gap, other.clone()),
                })
                .collect();
            let mut always_up = machine(expects_graph);
            always_up.on_message(&HealthCheckMessage::CassiniConnected, T0);
            let (independent_said, _) = run_machine(independent(expects_graph), &events, true);
            let (always_up_said, _) = run_machine(always_up, &up, true);
            proptest::prop_assert_eq!(independent_said, always_up_said);
        }
    }
}
