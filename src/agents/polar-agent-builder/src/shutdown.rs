//! How an agent ends: the decisions, without the actors.
//!
//! The InfrastructureSupervisor owns the shutdown pipeline, and every reason to end
//! an agent goes through one entry point: a health verdict, the unexpected death of
//! a child, a signal from Kubernetes. This module holds the rules those share, as
//! plain values and functions, so each can be tested without starting an actor:
//!
//! - [`ShutdownMachine`]: where the agent is in its shutdown, and what a second
//!   trigger does (nothing).
//! - [`on_child_death`]: what to do about each kind of child dying in each phase.
//! - [`budget`]: how long the domain supervisor has to drain before the process
//!   exits regardless.
//! - [`spawn_signal_listener`]: SIGTERM and SIGINT become a shutdown request.

use crate::infrastructure::InfrastructureSupervisorMessage;
use ractor::ActorRef;
use std::sync::Arc;
use std::time::Duration;

/// The deployment's shutdown budget in whole seconds. The catalog derives it, and
/// derives `terminationGracePeriodSeconds` from the same value.
pub const SHUTDOWN_BUDGET_ENV: &str = "POLAR_SHUTDOWN_BUDGET_SECS";

/// How long the domain supervisor gets to drain when nothing says otherwise.
pub const DEFAULT_SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);

/// Works out the shutdown budget.
///
/// The deployment's value wins when it is set, because it is also what sizes the
/// pod's termination grace period; otherwise the budget the agent's own code
/// declared; otherwise the default. A set value that is not a whole number of
/// seconds, at least 1, is an error.
pub fn budget(declared: Option<Duration>, env: Option<&str>) -> Result<Duration, String> {
    let fallback = declared.unwrap_or(DEFAULT_SHUTDOWN_BUDGET);
    match env.map(str::trim).filter(|raw| !raw.is_empty()) {
        None => Ok(fallback),
        Some(raw) => {
            let secs: u64 = raw.parse().map_err(|_| {
                format!(
                    "{SHUTDOWN_BUDGET_ENV} must be a whole number of seconds, at least 1, got '{raw}'"
                )
            })?;
            if secs < 1 {
                return Err(format!(
                    "{SHUTDOWN_BUDGET_ENV} must be a whole number of seconds, at least 1, got '{raw}'"
                ));
            }
            let from_env = Duration::from_secs(secs);
            if from_env < fallback {
                tracing::warn!(
                    "{SHUTDOWN_BUDGET_ENV} ({secs}s) is shorter than the {}s this agent \
                     declared it needs to drain; the deployment's value is used",
                    fallback.as_secs()
                );
            }
            Ok(from_env)
        }
    }
}

/// Ends the process. A value, so a test can watch for the call instead of dying.
pub(crate) type ProcessExit = Arc<dyn Fn(i32) + Send + Sync>;

pub(crate) fn real_process_exit() -> ProcessExit {
    Arc::new(|code| std::process::exit(code))
}

// ---------------------------------------------------------------------------
// The shutdown machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Phase {
    /// Nothing has asked the agent to end.
    Running,
    /// The domain supervisor has been told to stop its own work. Infrastructure
    /// stays up for it until it says it is done.
    Draining { reason: String },
    /// The domain supervisor is done; infrastructure is coming down.
    Stopping { reason: String },
}

#[derive(Debug)]
pub(crate) struct ShutdownMachine {
    phase: Phase,
}

impl ShutdownMachine {
    pub(crate) fn new() -> Self {
        Self {
            phase: Phase::Running,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.phase == Phase::Running
    }

    #[cfg(test)]
    pub(crate) fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Why the shutdown began, once it has.
    pub(crate) fn reason(&self) -> Option<&str> {
        match &self.phase {
            Phase::Running => None,
            Phase::Draining { reason } | Phase::Stopping { reason } => Some(reason),
        }
    }

    /// A reason to end has turned up. Returns whether this one started the
    /// shutdown; any later one finds it already under way and changes nothing.
    pub(crate) fn begin(&mut self, reason: &str) -> bool {
        if self.is_running() {
            self.phase = Phase::Draining {
                reason: reason.to_string(),
            };
            true
        } else {
            false
        }
    }

    /// The domain supervisor has finished. Returns whether infrastructure should
    /// now come down (false if it already is).
    pub(crate) fn proceed(&mut self) -> bool {
        match &self.phase {
            Phase::Stopping { .. } => false,
            Phase::Draining { reason } => {
                self.phase = Phase::Stopping {
                    reason: reason.clone(),
                };
                true
            }
            Phase::Running => {
                self.phase = Phase::Stopping {
                    reason: "the domain supervisor released the infrastructure unprompted"
                        .to_string(),
                };
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// What to do when a child dies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Child {
    Health,
    TcpClient,
    /// The router, the crypto actor, or anything else, by name.
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Death {
    /// It stopped, with whatever reason it gave.
    Terminated(Option<String>),
    /// It failed: an error or a panic.
    Failed(String),
}

impl Death {
    fn detail(&self) -> String {
        match self {
            Death::Terminated(Some(reason)) => reason.clone(),
            Death::Terminated(None) => "no reason given".to_string(),
            Death::Failed(error) => error.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Action {
    /// Expected: the children are being stopped as part of the shutdown.
    Ignore,
    /// The Cassini client stopped cleanly while the agent was running. Replace it.
    RespawnTcpClient,
    /// An orderly shutdown, for this reason.
    Shutdown(String),
}

/// What the supervisor does when one of its children ends.
///
/// While the agent is running, any child ending is a reason to shut down, except
/// the Cassini client stopping cleanly, which is replaced. Once the shutdown is
/// under way the children are being stopped on purpose, so nothing they do matters.
pub(crate) fn on_child_death(running: bool, child: &Child, death: &Death) -> Action {
    if !running {
        return Action::Ignore;
    }
    match (child, death) {
        (Child::TcpClient, Death::Terminated(_)) => Action::RespawnTcpClient,
        (Child::TcpClient, Death::Failed(error)) => {
            Action::Shutdown(format!("the Cassini client failed: {error}"))
        }
        (Child::Health, _) => Action::Shutdown(format!(
            "the health actor ended unexpectedly ({}); it reports a verdict and stays alive, \
             so this is a fault in the health actor itself",
            death.detail()
        )),
        (Child::Other(name), _) => {
            Action::Shutdown(format!("{name} ended unexpectedly ({})", death.detail()))
        }
    }
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// Turns SIGTERM and SIGINT into a request for an orderly shutdown.
///
/// Kubernetes sends SIGTERM for `kubectl delete`, a rollout, a node drain, and a
/// failed liveness probe. An agent that is PID 1 and has no handler ignores it, so
/// the container is killed outright when the grace period ends and nothing it
/// should have done on the way out gets done. A second signal while a shutdown is
/// already under way is logged and ignored.
pub(crate) fn spawn_signal_listener(supervisor: ActorRef<InfrastructureSupervisorMessage>) {
    use tokio::signal::unix::{SignalKind, signal};

    let (mut term, mut int) = match (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        (Ok(term), Ok(int)) => (term, int),
        (term, int) => {
            tracing::error!(
                "InfrastructureSupervisor: could not listen for SIGTERM/SIGINT (term: {:?}, \
                 int: {:?}); this pod will not shut down in an orderly way when Kubernetes \
                 asks it to",
                term.err(),
                int.err()
            );
            return;
        }
    };

    tokio::spawn(async move {
        loop {
            let name = tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = int.recv() => "SIGINT",
            };
            let sent = supervisor.cast(InfrastructureSupervisorMessage::BeginShutdown {
                reason: format!("received {name}"),
            });
            if sent.is_err() {
                // The supervisor is gone; nothing left to ask.
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- the budget -----------------------------------------------------------

    #[test]
    fn the_budget_defaults_to_twenty_seconds() {
        assert_eq!(budget(None, None).unwrap(), Duration::from_secs(20));
    }

    #[test]
    fn a_budget_the_agent_declares_replaces_the_default() {
        assert_eq!(
            budget(Some(Duration::from_secs(50)), None).unwrap(),
            Duration::from_secs(50)
        );
    }

    #[test]
    fn the_deployment_value_wins_over_the_declared_one_in_both_directions() {
        let declared = Some(Duration::from_secs(50));
        assert_eq!(
            budget(declared, Some("90")).unwrap(),
            Duration::from_secs(90)
        );
        assert_eq!(
            budget(declared, Some("30")).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(budget(None, Some("5")).unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn a_blank_deployment_value_means_nothing_was_set() {
        assert_eq!(budget(None, Some("  ")).unwrap(), DEFAULT_SHUTDOWN_BUDGET);
        assert_eq!(budget(None, Some("")).unwrap(), DEFAULT_SHUTDOWN_BUDGET);
    }

    #[test]
    fn a_bad_deployment_value_is_an_error_that_names_the_variable() {
        for bad in ["soon", "1.5", "-3", "0", "20s"] {
            let err = budget(None, Some(bad)).unwrap_err();
            assert!(err.contains(SHUTDOWN_BUDGET_ENV), "{bad}: {err}");
            assert!(err.contains(bad), "{bad}: {err}");
        }
    }

    // -- the machine ------------------------------------------------------------

    #[test]
    fn a_new_machine_is_running() {
        let m = ShutdownMachine::new();
        assert!(m.is_running());
        assert_eq!(m.reason(), None);
    }

    #[test]
    fn the_first_reason_starts_the_shutdown_and_later_ones_change_nothing() {
        let mut m = ShutdownMachine::new();
        assert!(m.begin("health verdict"));
        assert!(!m.is_running());
        assert_eq!(m.reason(), Some("health verdict"));

        assert!(
            !m.begin("received SIGTERM"),
            "a second trigger starts nothing"
        );
        assert_eq!(
            m.reason(),
            Some("health verdict"),
            "and keeps the first reason"
        );
        assert_eq!(
            m.phase(),
            &Phase::Draining {
                reason: "health verdict".into()
            }
        );
    }

    #[test]
    fn proceeding_moves_a_draining_machine_to_stopping_once() {
        let mut m = ShutdownMachine::new();
        m.begin("why");
        assert!(m.proceed());
        assert_eq!(
            m.phase(),
            &Phase::Stopping {
                reason: "why".into()
            }
        );
        assert!(
            !m.proceed(),
            "a second release does not stop everything twice"
        );
    }

    #[test]
    fn a_trigger_after_stopping_has_begun_changes_nothing() {
        let mut m = ShutdownMachine::new();
        m.begin("why");
        m.proceed();
        assert!(!m.begin("again"));
        assert_eq!(m.reason(), Some("why"));
    }

    #[test]
    fn an_unprompted_release_still_stops_and_says_so() {
        let mut m = ShutdownMachine::new();
        assert!(m.proceed());
        assert!(m.reason().unwrap().contains("unprompted"));
    }

    // -- child deaths -------------------------------------------------------------

    fn terminated() -> Death {
        Death::Terminated(Some("stopped".into()))
    }

    fn failed() -> Death {
        Death::Failed("boom".into())
    }

    fn shutdown_reason(action: Action) -> String {
        match action {
            Action::Shutdown(reason) => reason,
            other => panic!("expected a shutdown, got {other:?}"),
        }
    }

    #[test]
    fn nothing_a_child_does_matters_once_the_shutdown_is_under_way() {
        for child in [
            Child::Health,
            Child::TcpClient,
            Child::Other("router".into()),
        ] {
            for death in [terminated(), failed(), Death::Terminated(None)] {
                assert_eq!(
                    on_child_death(false, &child, &death),
                    Action::Ignore,
                    "{child:?} {death:?}"
                );
            }
        }
    }

    #[test]
    fn the_cassini_client_stopping_cleanly_is_replaced() {
        assert_eq!(
            on_child_death(true, &Child::TcpClient, &terminated()),
            Action::RespawnTcpClient
        );
        assert_eq!(
            on_child_death(true, &Child::TcpClient, &Death::Terminated(None)),
            Action::RespawnTcpClient
        );
    }

    #[test]
    fn the_cassini_client_failing_is_a_shutdown() {
        let reason = shutdown_reason(on_child_death(true, &Child::TcpClient, &failed()));
        assert!(reason.contains("Cassini client failed"), "{reason}");
        assert!(reason.contains("boom"), "{reason}");
    }

    #[test]
    fn the_health_actor_dying_is_a_shutdown_whichever_way_it_goes() {
        for death in [terminated(), failed(), Death::Terminated(None)] {
            let reason = shutdown_reason(on_child_death(true, &Child::Health, &death));
            assert!(reason.contains("health actor"), "{reason}");
        }
    }

    #[test]
    fn any_other_child_dying_is_a_shutdown_that_names_it() {
        for death in [terminated(), failed()] {
            let reason =
                shutdown_reason(on_child_death(true, &Child::Other("router".into()), &death));
            assert!(reason.contains("router"), "{reason}");
        }
    }

    #[test]
    fn a_missing_reason_is_said_plainly() {
        let reason = shutdown_reason(on_child_death(
            true,
            &Child::Other("crypto".into()),
            &Death::Terminated(None),
        ));
        assert!(reason.contains("no reason given"), "{reason}");
    }
}
