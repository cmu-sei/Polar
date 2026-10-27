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

//! A pod's whole life, as the kubelet sees it.
//!
//! The pieces are tested one by one elsewhere. These tests put the real
//! [`HealthMachine`] and the real probe next to a model of the kubelet and run them
//! against each other for every phase the two can be in relative to each other, to
//! answer the two questions the design rests on:
//!
//! - Is an agent that is rejuvenating, or draining after a verdict, ever killed by
//!   the liveness probe before it has finished? It must not be.
//! - Is an agent whose actor has stopped reporting killed by the probe, and how
//!   soon? It must be, and within a known bound.
//!
//! Everything runs in virtual seconds, so thousands of pods take a moment.

use crate::certs::CertReading;
use crate::machine::{HealthMachine, Next, TickStep};
use crate::probe::judge;
use crate::{HealthCheckMessage, HealthConfig, HealthVerdict};

/// The kubelet's liveness probe settings, as the catalog sets them for every
/// agent (`infra/config/kinds.nuon`: `probe`).
const PROBE_INITIAL_DELAY: u64 = 150;
const PROBE_PERIOD: u64 = 30;
const PROBE_FAILURE_THRESHOLD: u32 = 2;

/// How old the probe lets the health file get (`POLAR_HEALTH_STALE_SECS`).
const STALE: u64 = 90;
const TICK: u64 = 30;
const EXPIRY: i64 = 60;
const REJUVENATION: i64 = 300;
/// The longest drain any agent declares a budget for (the credentials consumer's).
const DRAIN: u64 = 60;
/// An arbitrary Unix time for second zero of the pod.
const BASE: u64 = 1_700_000_000;

fn config() -> HealthConfig {
    HealthConfig {
        expects_graph: false,
        expects_cassini: true,
        rejuvenation_threshold_secs: REJUVENATION,
        expiry_threshold_secs: EXPIRY,
        tick_secs: TICK,
        graph_deadline_secs: 120,
        cassini_deadline_secs: 60,
        probe_stale_secs: STALE,
        file_path: String::new(),
        cert_paths: vec![],
        dep_cert_endpoints: vec![],
        custody_socket: None,
    }
}

#[derive(Debug, PartialEq)]
enum End {
    /// The drain finished and the agent exited on its own.
    Exited,
    /// The kubelet killed the pod at this second.
    Killed(u64),
    /// Nothing happened to it before the end of the simulated time.
    Survived,
}

#[derive(Debug)]
struct Run {
    end: End,
    /// When and what the actor decided, if it did.
    verdict: Option<(u64, HealthVerdict)>,
    /// When the actor last wrote the file.
    last_write: Option<u64>,
    /// The oldest the file ever looked to a probe that found it.
    oldest_seen: u64,
}

/// What happens to the certificate and the actor during the run.
struct Scenario {
    /// Seconds into the pod's life that the actor starts ticking (0..30): the
    /// phase of the actor's ticks against the kubelet's probes.
    actor_start: u64,
    /// The jitter the machine draws when its dependencies are healthy.
    jitter: u64,
    /// Seconds of life the agent's certificate has when the pod starts.
    cert_life: u64,
    /// The actor stops ticking, and writing, at this second: a wedged process.
    wedge_at: Option<u64>,
    /// How long to simulate if nothing ends the pod first.
    horizon: u64,
}

impl Scenario {
    fn rejuvenation(actor_start: u64, jitter: u64) -> Self {
        Self {
            actor_start,
            jitter,
            cert_life: 400,
            wedge_at: None,
            horizon: 2_000,
        }
    }
}

/// Runs the actor's ticks and the kubelet's probes against each other.
fn simulate(scenario: &Scenario) -> Run {
    let mut machine = HealthMachine::new(&config(), BASE + scenario.actor_start);
    machine.on_message(
        &HealthCheckMessage::CassiniConnected,
        BASE + scenario.actor_start,
    );

    let mut file: Option<String> = None;
    let mut last_write = None;
    let mut verdict = None;
    let mut exits_at: Option<u64> = None;

    let mut next_tick = scenario.actor_start + TICK;
    let mut next_probe = PROBE_INITIAL_DELAY;
    let mut failures = 0;
    let mut oldest_seen = 0;

    loop {
        let t = next_tick.min(next_probe);

        if let Some(done) = exits_at
            && t > done
        {
            return Run {
                end: End::Exited,
                verdict,
                last_write,
                oldest_seen,
            };
        }
        if t > scenario.horizon {
            return Run {
                end: End::Survived,
                verdict,
                last_write,
                oldest_seen,
            };
        }

        if next_probe <= next_tick {
            let passed = match &file {
                None => false,
                Some(contents) => {
                    let report = judge(contents, BASE + t, STALE);
                    let age = (BASE + t).saturating_sub(last_write.map_or(0, |w| BASE + w));
                    oldest_seen = oldest_seen.max(age);
                    report.ok
                }
            };
            failures = if passed { 0 } else { failures + 1 };
            if failures >= PROBE_FAILURE_THRESHOLD {
                return Run {
                    end: End::Killed(t),
                    verdict,
                    last_write,
                    oldest_seen,
                };
            }
            next_probe += PROBE_PERIOD;
        } else {
            if scenario.wedge_at.is_some_and(|w| t >= w) {
                // Nothing in the process runs any more.
                next_tick = u64::MAX;
                continue;
            }
            let now = BASE + t;
            let ttl = scenario.cert_life as i64 - t as i64;
            let reading = CertReading {
                hard: if ttl < EXPIRY {
                    Err(format!("the certificate has {ttl}s left"))
                } else {
                    Ok(())
                },
                min_ttl: Some(ttl),
                custody: crate::custody::Custody::NotUsed,
            };
            let outcome = match machine.tick(now, &reading) {
                TickStep::Done(outcome) => outcome,
                TickStep::NeedDeps => machine.deps_checked(now, true, scenario.jitter),
            };
            if let Some(written) = outcome.write {
                file = Some(serde_json::to_string(&written).unwrap());
                last_write = Some(t);
            }
            if let Next::Verdict(v) = outcome.next {
                verdict = Some((t, v));
                exits_at = Some(t + DRAIN);
            }
            next_tick += TICK;
        }
    }
}

#[test]
fn a_rejuvenating_agent_is_never_killed_for_any_jitter_or_probe_phase() {
    let mut runs = 0;
    let mut longest_wait = 0;
    for actor_start in 0..TICK {
        for jitter in 0..crate::DEFAULT_JITTER_WINDOW_SECS {
            let run = simulate(&Scenario::rejuvenation(actor_start, jitter));
            runs += 1;

            assert_eq!(
                run.end,
                End::Exited,
                "killed with actor_start={actor_start} jitter={jitter}: {run:?}"
            );
            // It is a rejuvenation, not a failure: the certificate had not reached
            // the hard floor by the time the verdict fell.
            assert!(
                matches!(run.verdict, Some((_, HealthVerdict::Rejuvenate))),
                "actor_start={actor_start} jitter={jitter}: {run:?}"
            );
            longest_wait = longest_wait.max(run.oldest_seen);
        }
    }
    assert_eq!(runs, 30 * 180);
    // The file was never allowed to get anywhere near the probe's limit.
    assert!(
        longest_wait <= TICK,
        "a probe found the file {longest_wait}s old, with ticks every {TICK}s"
    );
}

#[test]
fn the_worst_case_rejuvenation_still_fits_before_the_hard_floor() {
    // The consistency check in HealthConfig says a rejuvenation needs two ticks plus
    // the jitter window, and there is exactly that much room by default. Find the
    // slowest one in the sweep and see how close it comes.
    let mut least_ttl_left = i64::MAX;
    for actor_start in 0..TICK {
        for jitter in 0..crate::DEFAULT_JITTER_WINDOW_SECS {
            let run = simulate(&Scenario::rejuvenation(actor_start, jitter));
            let (at, _) = run.verdict.expect("a verdict");
            least_ttl_left = least_ttl_left.min(400 - at as i64);
        }
    }
    assert!(
        least_ttl_left >= EXPIRY,
        "a rejuvenation verdict came with only {least_ttl_left}s of certificate left"
    );
    assert!(
        least_ttl_left <= EXPIRY + 2,
        "the sweep never came close to the limit ({least_ttl_left}s left); is it still \
         exercising the worst case?"
    );
}

#[test]
fn an_agent_draining_after_an_unhealthy_verdict_is_not_killed_either() {
    // The certificate runs out under the agent: its jitter (179 s) is longer than
    // the 140 s the certificate has before the hard floor, so the actor gives an
    // Unhealthy verdict instead of rejuvenating. It writes unhealthy for the whole
    // drain, and the probe must let it finish.
    for actor_start in 0..TICK {
        let run = simulate(&Scenario {
            actor_start,
            jitter: 179,
            cert_life: 200,
            wedge_at: None,
            horizon: 2_000,
        });
        assert!(
            matches!(run.verdict, Some((_, HealthVerdict::Unhealthy(_)))),
            "actor_start={actor_start}: {run:?}"
        );
        assert_eq!(run.end, End::Exited, "actor_start={actor_start}: {run:?}");
    }
}

#[test]
fn a_wedged_agent_is_killed_within_a_known_bound_and_a_draining_one_ends_itself_first() {
    // The actor stops writing at some moment, whatever phase it is in. Nothing in
    // the process runs to do anything about it, so the probe has to.
    //
    // The file must first go stale (older than 90 s), which the first probe to see
    // it fails; then a second consecutive failure, 30 s later, kills the pod. The
    // first failing probe is at most one period after the file turned stale.
    //
    // One case is different, by design: if the actor wedges *after* its verdict, the
    // drain is bounded (60 s here, and every agent's budget is shorter than the
    // probe's staleness limit plus its patience), so the agent's own watchdog ends
    // the process before the kubelet would. The probe never gets to act.
    let longest = STALE + 1 + PROBE_PERIOD + (PROBE_FAILURE_THRESHOLD as u64 - 1) * PROBE_PERIOD;
    let mut killed = 0;
    let mut ended_itself = 0;

    for actor_start in (0..TICK).step_by(3) {
        for wedge_at in (60..420).step_by(7) {
            let run = simulate(&Scenario {
                actor_start,
                jitter: 90,
                cert_life: 400,
                wedge_at: Some(wedge_at),
                horizon: 2_000,
            });
            match run.end {
                End::Killed(killed_at) => {
                    let last_write = run.last_write.expect("it wrote before it wedged");
                    let after = killed_at - last_write;
                    assert!(
                        after > STALE,
                        "killed {after}s after the last write, before it could be stale: {run:?}"
                    );
                    assert!(
                        after <= longest,
                        "killed {after}s after the last write, later than the {longest}s bound: \
                         {run:?}"
                    );
                    assert!(
                        run.verdict.is_none(),
                        "wedged before it could decide: {run:?}"
                    );
                    killed += 1;
                }
                End::Exited => {
                    // Only an agent that had already decided can have exited.
                    assert!(run.verdict.is_some(), "{run:?}");
                    ended_itself += 1;
                }
                End::Survived => panic!("a wedged agent was never killed: {run:?}"),
            }
        }
    }
    assert!(
        killed > 0,
        "the sweep never wedged the actor before its verdict"
    );
    assert!(
        ended_itself > 0,
        "the sweep never wedged the actor during its drain"
    );
}

#[test]
fn a_healthy_agent_that_never_rejuvenates_is_never_killed() {
    // A long-lived certificate: the whole run is normal operation, for an hour.
    for actor_start in 0..TICK {
        let run = simulate(&Scenario {
            actor_start,
            jitter: 0,
            cert_life: 100_000,
            wedge_at: None,
            horizon: 3_600,
        });
        assert_eq!(run.end, End::Survived, "actor_start={actor_start}: {run:?}");
        assert!(run.verdict.is_none());
        assert!(run.oldest_seen <= TICK, "{run:?}");
    }
}
