//! What to do about one agent, as a pure function.
//!
//! [`decide`] is given what is desired (the spec's hash, whether it is suspended), what
//! the cluster shows (the agent's pods, read as [`Instance`]s), what the operator
//! remembers (the status) and the time. It returns the actions to take and the status to
//! write. It never reaches for a clock or a cluster, so every rule is tested here.
//!
//! The rules:
//!
//! 1. **One instance, never two.** A new instance is started only when no pod of the
//!    agent exists at all, terminating ones included: two pods with one identity must
//!    never overlap, even during a drain.
//! 2. **Every ending is recorded once, then its pod is removed.** The ending stays in the
//!    status; the replacement waits for the pod to be gone (rule 1).
//! 3. **An ending soon after a start is a failure,** and failures in a row back off
//!    exponentially. An ending after a long run (a rejuvenation, a drain) is the contract
//!    working, and the replacement starts at once.
//! 4. **Too many failures in a row hold the agent** (`CrashLoop`) instead of retrying
//!    forever. Changing the spec releases it.
//! 5. **A new spec replaces the running instance:** it is told to stop (and drains), and
//!    the new one starts once it is gone.

use crate::crd::{Ending, Phase, PolarAgentStatus};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::time::Duration;

/// One pod of the agent, as the decision needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub pod: String,
    /// The hash of the spec the pod was made from (its label).
    pub spec_hash: Option<String>,
    /// Being deleted: still alive, perhaps draining, but on its way out.
    pub terminating: bool,
    pub state: InstanceState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstanceState {
    /// Not running yet: being scheduled, pulling, or stuck (the reason says).
    Waiting {
        reason: Option<String>,
    },
    Running,
    Ended(Ending),
}

/// The tunables of the rules above.
#[derive(Debug, Clone)]
pub struct Policy {
    /// An ending sooner than this after the start counts as a failure.
    pub quick_failure_secs: i64,
    /// The first backoff; each further failure in a row doubles it.
    pub base_backoff_secs: i64,
    pub max_backoff_secs: i64,
    /// Failures in a row before the agent is held.
    pub crash_loop_after: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            quick_failure_secs: 30,
            base_backoff_secs: 2,
            max_backoff_secs: 300,
            crash_loop_after: 5,
        }
    }
}

/// What the cluster should be asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start an instance in a pod of this name.
    Create { pod: String },
    /// Stop an instance (SIGTERM, then its grace period), or remove an ended one.
    Delete { pod: String, why: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// The status to write, before acting.
    pub status: PolarAgentStatus,
    /// When to look again even if nothing changes (a backoff running out).
    pub requeue_after: Option<Duration>,
}

/// The wanted state of one agent.
#[derive(Debug, Clone, Copy)]
pub struct Desired<'a> {
    pub name: &'a str,
    pub spec_hash: &'a str,
    pub suspend: bool,
}

pub fn decide(
    desired: Desired<'_>,
    instances: &[Instance],
    remembered: &PolarAgentStatus,
    now: DateTime<Utc>,
    policy: &Policy,
) -> Plan {
    let mut status = remembered.clone();
    let mut actions = Vec::new();
    status.spec_hash = Some(desired.spec_hash.to_string());

    // Rule 4's release: failures counted against another spec do not hold this one.
    if status
        .failing_hash
        .as_deref()
        .is_some_and(|h| h != desired.spec_hash)
    {
        status.consecutive_failures = 0;
        status.failing_hash = None;
        status.next_start_after = None;
    }

    // Rule 2: record each ending once, oldest first, and remove its pod.
    let mut ended: Vec<(&Instance, &Ending)> = instances
        .iter()
        .filter_map(|i| match &i.state {
            InstanceState::Ended(e) => Some((i, e)),
            _ => None,
        })
        .collect();
    ended.sort_by(|a, b| a.1.finished_at.cmp(&b.1.finished_at));
    for (instance, ending) in ended {
        let recorded = status
            .last_ending
            .as_ref()
            .is_some_and(|e| e.pod == instance.pod);
        if !recorded {
            record(&mut status, instance, ending, desired.spec_hash, policy);
        }
        if !instance.terminating {
            actions.push(Action::Delete {
                pod: instance.pod.clone(),
                why: "ended".into(),
            });
        }
    }

    // Rules 1 and 5: a live instance is kept, or told to stop; never joined by another.
    let mut live: Vec<&Instance> = instances
        .iter()
        .filter(|i| !i.terminating && !matches!(i.state, InstanceState::Ended(_)))
        .collect();
    live.sort_by(|a, b| a.pod.cmp(&b.pod));
    if let Some(current) = live.first() {
        // There should only ever be one; anything beyond the first is stopped.
        for extra in &live[1..] {
            actions.push(Action::Delete {
                pod: extra.pod.clone(),
                why: "a second instance of one agent".into(),
            });
        }
        status.pod = Some(current.pod.clone());
        if desired.suspend {
            actions.push(Action::Delete {
                pod: current.pod.clone(),
                why: "suspended".into(),
            });
            status.phase = Phase::Suspended;
            status.message = Some(format!("stopping {}", current.pod));
        } else if current.spec_hash.as_deref() != Some(desired.spec_hash) {
            actions.push(Action::Delete {
                pod: current.pod.clone(),
                why: "the spec changed".into(),
            });
            status.phase = Phase::Replacing;
            status.message = Some(format!(
                "stopping {} so that an instance of the new spec can start",
                current.pod
            ));
        } else {
            match &current.state {
                InstanceState::Waiting { reason } => {
                    status.phase = Phase::Starting;
                    status.message = reason.clone();
                }
                _ => {
                    status.phase = Phase::Running;
                    status.message = None;
                }
            }
        }
        return Plan {
            actions,
            status,
            requeue_after: None,
        };
    }

    // Rule 1: pods on their way out still hold the identity; wait for them.
    if !instances.is_empty() {
        let leaving: Vec<&str> = instances.iter().map(|i| i.pod.as_str()).collect();
        status.pod = None;
        status.phase = if desired.suspend {
            Phase::Suspended
        } else {
            Phase::Ending
        };
        status.message = Some(format!("waiting for {} to be gone", leaving.join(", ")));
        return Plan {
            actions,
            status,
            // The pod's deletion wakes the controller; this is only a safety net.
            requeue_after: Some(Duration::from_secs(10)),
        };
    }

    // No pod at all: start one, unless held, suspended or backing off.
    status.pod = None;
    if desired.suspend {
        status.phase = Phase::Suspended;
        status.message = None;
        return Plan {
            actions,
            status,
            requeue_after: None,
        };
    }
    if status.consecutive_failures >= policy.crash_loop_after {
        status.phase = Phase::CrashLoop;
        status.message = Some(format!(
            "held after {} endings in a row within {}s of starting; change the spec to release it",
            status.consecutive_failures, policy.quick_failure_secs
        ));
        return Plan {
            actions,
            status,
            requeue_after: None,
        };
    }
    if let Some(after) = status.next_start_after.as_deref().and_then(parse_time)
        && after > now
    {
        let wait = (after - now).to_std().unwrap_or_default();
        status.phase = Phase::BackingOff;
        status.message = Some(format!(
            "failure {} in a row; next start in {}s",
            status.consecutive_failures,
            wait.as_secs().max(1)
        ));
        return Plan {
            actions,
            status,
            requeue_after: Some(wait),
        };
    }

    status.instances_started += 1;
    let pod = format!("{}-{}", desired.name, status.instances_started);
    status.pod = Some(pod.clone());
    status.phase = Phase::Starting;
    status.message = None;
    status.next_start_after = None;
    actions.push(Action::Create { pod });
    Plan {
        actions,
        status,
        requeue_after: None,
    }
}

/// Rule 3: an ending either continues a run of failures or ends it.
fn record(
    status: &mut PolarAgentStatus,
    instance: &Instance,
    ending: &Ending,
    desired_hash: &str,
    policy: &Policy,
) {
    let of_this_spec = instance.spec_hash.as_deref() == Some(desired_hash);
    if ending.ran_secs < policy.quick_failure_secs && of_this_spec {
        status.consecutive_failures += 1;
        status.failing_hash = Some(desired_hash.to_string());
        let doublings = status.consecutive_failures.saturating_sub(1).min(20);
        let backoff = policy
            .base_backoff_secs
            .saturating_mul(1 << doublings)
            .min(policy.max_backoff_secs);
        let from = parse_time(&ending.finished_at).unwrap_or_else(Utc::now);
        status.next_start_after = Some((from + ChronoDuration::seconds(backoff)).to_rfc3339());
    } else {
        status.consecutive_failures = 0;
        status.failing_hash = None;
        status.next_start_after = None;
    }
    status.last_ending = Some(ending.clone());
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::EndingKind;

    const HASH: &str = "aaaa";
    const NEW_HASH: &str = "bbbb";

    fn now() -> DateTime<Utc> {
        parse_time("2026-10-18T12:00:00Z").unwrap()
    }

    fn desired() -> Desired<'static> {
        Desired {
            name: "demo",
            spec_hash: HASH,
            suspend: false,
        }
    }

    fn instance(pod: &str, hash: &str, state: InstanceState) -> Instance {
        Instance {
            pod: pod.into(),
            spec_hash: Some(hash.into()),
            terminating: false,
            state,
        }
    }

    fn ended(pod: &str, ran_secs: i64, finished_at: &str) -> InstanceState {
        InstanceState::Ended(Ending {
            pod: pod.into(),
            pod_uid: Some(format!("uid-{pod}")),
            kind: EndingKind::Exited,
            exit_code: 1,
            reason: "Error".into(),
            message: Some("rejuvenating".into()),
            ran_secs,
            finished_at: finished_at.into(),
        })
    }

    fn creates(plan: &Plan) -> Vec<&str> {
        plan.actions
            .iter()
            .filter_map(|a| match a {
                Action::Create { pod } => Some(pod.as_str()),
                _ => None,
            })
            .collect()
    }

    fn deletes(plan: &Plan) -> Vec<&str> {
        plan.actions
            .iter()
            .filter_map(|a| match a {
                Action::Delete { pod, .. } => Some(pod.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_new_agent_gets_its_first_instance() {
        let plan = decide(
            desired(),
            &[],
            &Default::default(),
            now(),
            &Policy::default(),
        );
        assert_eq!(creates(&plan), ["demo-1"]);
        assert_eq!(plan.status.phase, Phase::Starting);
        assert_eq!(plan.status.instances_started, 1);
    }

    #[test]
    fn a_running_instance_of_the_spec_is_left_alone() {
        let pods = [instance("demo-1", HASH, InstanceState::Running)];
        let plan = decide(
            desired(),
            &pods,
            &Default::default(),
            now(),
            &Policy::default(),
        );
        assert!(plan.actions.is_empty());
        assert_eq!(plan.status.phase, Phase::Running);
    }

    #[test]
    fn a_waiting_instance_says_why() {
        let reason = Some("ImagePullBackOff".to_string());
        let pods = [instance(
            "demo-1",
            HASH,
            InstanceState::Waiting {
                reason: reason.clone(),
            },
        )];
        let plan = decide(
            desired(),
            &pods,
            &Default::default(),
            now(),
            &Policy::default(),
        );
        assert!(plan.actions.is_empty());
        assert_eq!(plan.status.phase, Phase::Starting);
        assert_eq!(plan.status.message, reason);
    }

    #[test]
    fn a_new_spec_stops_the_old_instance_and_starts_nothing_yet() {
        let pods = [instance("demo-1", HASH, InstanceState::Running)];
        let new = Desired {
            spec_hash: NEW_HASH,
            ..desired()
        };
        let plan = decide(new, &pods, &Default::default(), now(), &Policy::default());
        assert_eq!(deletes(&plan), ["demo-1"]);
        assert!(creates(&plan).is_empty());
        assert_eq!(plan.status.phase, Phase::Replacing);
    }

    #[test]
    fn no_instance_starts_while_another_is_terminating() {
        let mut leaving = instance("demo-1", HASH, InstanceState::Running);
        leaving.terminating = true;
        let plan = decide(
            desired(),
            &[leaving],
            &Default::default(),
            now(),
            &Policy::default(),
        );
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert_eq!(plan.status.phase, Phase::Ending);
    }

    #[test]
    fn an_ending_after_a_long_run_is_replaced_at_once() {
        let pods = [instance(
            "demo-1",
            HASH,
            ended("demo-1", 3600, "2026-10-18T11:59:59Z"),
        )];
        let started = PolarAgentStatus {
            instances_started: 1,
            ..Default::default()
        };
        let first = decide(desired(), &pods, &started, now(), &Policy::default());
        assert_eq!(deletes(&first), ["demo-1"]);
        assert!(creates(&first).is_empty(), "the old pod is still there");
        assert_eq!(first.status.consecutive_failures, 0);
        assert_eq!(first.status.last_ending.as_ref().unwrap().pod, "demo-1");

        let second = decide(desired(), &[], &first.status, now(), &Policy::default());
        assert_eq!(creates(&second), ["demo-2"]);
    }

    #[test]
    fn a_quick_ending_backs_off_before_the_next_start() {
        let pods = [instance(
            "demo-1",
            HASH,
            ended("demo-1", 2, "2026-10-18T12:00:00Z"),
        )];
        let first = decide(
            desired(),
            &pods,
            &Default::default(),
            now(),
            &Policy::default(),
        );
        assert_eq!(first.status.consecutive_failures, 1);

        let waiting = decide(desired(), &[], &first.status, now(), &Policy::default());
        assert!(waiting.actions.is_empty());
        assert_eq!(waiting.status.phase, Phase::BackingOff);
        assert_eq!(waiting.requeue_after, Some(Duration::from_secs(2)));

        let later = now() + ChronoDuration::seconds(3);
        let after = decide(desired(), &[], &first.status, later, &Policy::default());
        assert_eq!(creates(&after), ["demo-1"]);
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        let policy = Policy {
            crash_loop_after: 100,
            ..Policy::default()
        };
        let mut status = PolarAgentStatus::default();
        let mut waits = Vec::new();
        for n in 1..=10 {
            let pod = format!("demo-{n}");
            let pods = [instance(&pod, HASH, ended(&pod, 1, "2026-10-18T12:00:00Z"))];
            status = decide(desired(), &pods, &status, now(), &policy).status;
            let after = parse_time(status.next_start_after.as_deref().unwrap()).unwrap();
            waits.push((after - now()).num_seconds());
        }
        assert_eq!(waits, [2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
    }

    #[test]
    fn too_many_quick_endings_hold_the_agent_until_the_spec_changes() {
        let policy = Policy::default();
        let status = PolarAgentStatus {
            consecutive_failures: policy.crash_loop_after,
            failing_hash: Some(HASH.into()),
            ..Default::default()
        };
        let held = decide(desired(), &[], &status, now(), &policy);
        assert!(held.actions.is_empty());
        assert_eq!(held.status.phase, Phase::CrashLoop);
        assert_eq!(held.requeue_after, None);

        let fixed = Desired {
            spec_hash: NEW_HASH,
            ..desired()
        };
        let released = decide(fixed, &[], &status, now(), &policy);
        assert_eq!(creates(&released).len(), 1);
        assert_eq!(released.status.consecutive_failures, 0);
    }

    #[test]
    fn an_ending_seen_twice_is_counted_once() {
        let pods = [instance(
            "demo-1",
            HASH,
            ended("demo-1", 1, "2026-10-18T12:00:00Z"),
        )];
        let first = decide(
            desired(),
            &pods,
            &Default::default(),
            now(),
            &Policy::default(),
        );
        let mut leaving = pods[0].clone();
        leaving.terminating = true;
        let again = decide(
            desired(),
            &[leaving],
            &first.status,
            now(),
            &Policy::default(),
        );
        assert_eq!(again.status.consecutive_failures, 1);
        assert!(again.actions.is_empty(), "already being deleted");
    }

    #[test]
    fn a_quick_ending_of_an_old_spec_is_not_a_failure_of_the_new_one() {
        let pods = [instance(
            "demo-1",
            HASH,
            ended("demo-1", 1, "2026-10-18T12:00:00Z"),
        )];
        let new = Desired {
            spec_hash: NEW_HASH,
            ..desired()
        };
        let plan = decide(new, &pods, &Default::default(), now(), &Policy::default());
        assert_eq!(plan.status.consecutive_failures, 0);
    }

    #[test]
    fn suspending_stops_the_instance_and_starts_nothing() {
        let suspended = Desired {
            suspend: true,
            ..desired()
        };
        let pods = [instance("demo-1", HASH, InstanceState::Running)];
        let stopping = decide(
            suspended,
            &pods,
            &Default::default(),
            now(),
            &Policy::default(),
        );
        assert_eq!(deletes(&stopping), ["demo-1"]);
        assert_eq!(stopping.status.phase, Phase::Suspended);

        let idle = decide(suspended, &[], &stopping.status, now(), &Policy::default());
        assert!(idle.actions.is_empty());
        assert_eq!(idle.status.phase, Phase::Suspended);
    }

    #[test]
    fn a_second_live_instance_is_stopped() {
        let pods = [
            instance("demo-1", HASH, InstanceState::Running),
            instance("demo-2", HASH, InstanceState::Running),
        ];
        let plan = decide(
            desired(),
            &pods,
            &Default::default(),
            now(),
            &Policy::default(),
        );
        assert_eq!(deletes(&plan), ["demo-2"]);
        assert!(creates(&plan).is_empty());
    }
}
