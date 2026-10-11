//! The ConfigurationActor: the runtime shell around the layered [`polar_env::Config`].
//!
//! It owns the agent's configuration, takes versioned overrides, and tells subscribers when
//! values change. Design: `docs/architecture/configuration.md`, section 5.
//!
//! What it does, and what it deliberately leaves to others:
//!
//! - **Transactional.** An override is parsed by the variable's own declaration and every
//!   section's relations are re-checked on the result. If anything is wrong nothing changes and
//!   the refusal names the variable. Only `Dynamic` variables accept overrides, so the agent's
//!   code, not the repository, decides what may change at runtime.
//! - **Monotonic.** Each update carries a version that must be higher than the last one
//!   applied, so an old update cannot be replayed over a newer one. A refused update does not
//!   advance the version, so a corrected one can be sent again under the same number.
//! - **Notified by message.** Subscribers receive [`ConfigChanged`] on an output port and read
//!   [`ConfigurationHandle::current`] again. No closure of theirs runs inside this actor.
//! - **Recorded.** Every applied and refused update is kept (the latest [`HISTORY`]) and logged,
//!   with its version. Values are never recorded: a secret's value appears nowhere.
//!
//! It does not decide who may send an update. Authentication of the publisher, and the policy
//! that says which publisher may set which variable, sit in front of [`ConfigurationHandle::apply`]
//! (signed configuration, `docs/architecture/configuration.md` section 6). It does not depend on
//! Cassini: an agent runs on its environment alone until something applies an update.

use polar_env::{Config, Resolved};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// How many of the latest updates are kept for [`ConfigurationHandle::history`].
pub const HISTORY: usize = 100;

/// How long a call to the actor may take before it is reported as failed.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Why an update was not applied. Nothing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The version was not higher than the last one applied: a replay, or an old update arriving
    /// late.
    Stale { applied: u64 },
    /// The update would put the agent in a state its startup check refuses, or names a
    /// variable that does not exist or may not change. Every problem, not the first.
    Invalid(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The names whose effective value changed (empty if the update changed nothing).
    Applied { changed: Vec<String> },
    Refused(Refusal),
}

/// One update, as recorded. Carries names and reasons, never values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEvent {
    pub version: u64,
    pub outcome: ApplyOutcome,
}

/// Sent on the output port when an update changed at least one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChanged {
    pub version: u64,
    pub changed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationError {
    /// The actor did not answer.
    Unavailable(String),
}

impl std::fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigurationError::Unavailable(why) => {
                write!(f, "the configuration actor did not answer: {why}")
            }
        }
    }
}

impl std::error::Error for ConfigurationError {}

pub enum ConfigurationMessage {
    Apply {
        version: u64,
        changes: Vec<(String, String)>,
        reply: RpcReplyPort<ApplyOutcome>,
    },
    Current(RpcReplyPort<Arc<Config>>),
    History(RpcReplyPort<Vec<ConfigEvent>>),
}

pub struct ConfigurationActor;

pub struct ConfigurationArgs {
    pub config: Config,
    pub changes: Arc<OutputPort<ConfigChanged>>,
}

pub struct ConfigurationState {
    config: Config,
    current: Arc<Config>,
    last_version: u64,
    history: VecDeque<ConfigEvent>,
    changes: Arc<OutputPort<ConfigChanged>>,
}

impl ConfigurationState {
    fn apply(&mut self, version: u64, changes: &[(String, String)]) -> ApplyOutcome {
        let outcome = if version <= self.last_version {
            ApplyOutcome::Refused(Refusal::Stale {
                applied: self.last_version,
            })
        } else {
            let pairs: Vec<(&str, &str)> = changes
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            match self.config.apply_overrides(&pairs) {
                Ok(changed) => {
                    self.last_version = version;
                    ApplyOutcome::Applied {
                        changed: changed.into_iter().map(str::to_string).collect(),
                    }
                }
                Err(problems) => ApplyOutcome::Refused(Refusal::Invalid(problems)),
            }
        };

        match &outcome {
            ApplyOutcome::Applied { changed } => {
                info!(version, ?changed, "configuration update applied");
                if !changed.is_empty() {
                    self.current = Arc::new(self.config.clone());
                    self.changes.send(ConfigChanged {
                        version,
                        changed: changed.clone(),
                    });
                }
            }
            ApplyOutcome::Refused(why) => {
                warn!(version, ?why, "configuration update refused");
            }
        }
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(ConfigEvent {
            version,
            outcome: outcome.clone(),
        });
        outcome
    }
}

#[ractor::async_trait]
impl Actor for ConfigurationActor {
    type Msg = ConfigurationMessage;
    type State = ConfigurationState;
    type Arguments = ConfigurationArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: ConfigurationArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(ConfigurationState {
            current: Arc::new(args.config.clone()),
            config: args.config,
            last_version: 0,
            history: VecDeque::new(),
            changes: args.changes,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ConfigurationMessage::Apply {
                version,
                changes,
                reply,
            } => {
                let outcome = state.apply(version, &changes);
                let _ = reply.send(outcome);
            }
            ConfigurationMessage::Current(reply) => {
                let _ = reply.send(state.current.clone());
            }
            ConfigurationMessage::History(reply) => {
                let _ = reply.send(state.history.iter().cloned().collect());
            }
        }
        Ok(())
    }
}

/// How the rest of an agent uses the ConfigurationActor, without touching ractor.
#[derive(Clone)]
pub struct ConfigurationHandle {
    actor: ActorRef<ConfigurationMessage>,
    changes: Arc<OutputPort<ConfigChanged>>,
}

impl ConfigurationHandle {
    /// Starts the actor on `config`, as a child of `supervisor`.
    pub async fn spawn(
        name: Option<String>,
        config: Config,
        supervisor: ractor::ActorCell,
    ) -> Result<Self, ActorProcessingErr> {
        let changes = Arc::new(OutputPort::default());
        let (actor, _) = Actor::spawn_linked(
            name,
            ConfigurationActor,
            ConfigurationArgs {
                config,
                changes: changes.clone(),
            },
            supervisor,
        )
        .await
        .map_err(ActorProcessingErr::from)?;
        Ok(Self { actor, changes })
    }

    /// The configuration as it stands now, with every override applied. Read again after a
    /// [`ConfigChanged`].
    pub async fn current(&self) -> Result<Arc<Config>, ConfigurationError> {
        self.call(ConfigurationMessage::Current).await
    }

    /// What every declared variable resolves to now and where it came from (values of secrets
    /// are hidden).
    pub async fn report(&self) -> Result<Vec<Resolved>, ConfigurationError> {
        Ok(self.current().await?.report())
    }

    /// Applies an update. A blank value removes that variable's override.
    pub async fn apply(
        &self,
        version: u64,
        changes: Vec<(String, String)>,
    ) -> Result<ApplyOutcome, ConfigurationError> {
        self.call(|reply| ConfigurationMessage::Apply {
            version,
            changes,
            reply,
        })
        .await
    }

    /// The latest updates, applied and refused, oldest first.
    pub async fn history(&self) -> Result<Vec<ConfigEvent>, ConfigurationError> {
        self.call(ConfigurationMessage::History).await
    }

    /// Where changes are announced. Subscribe with a mapping into your own message type, as
    /// for the other ports; then read [`current`](Self::current).
    pub fn changes(&self) -> Arc<OutputPort<ConfigChanged>> {
        self.changes.clone()
    }

    async fn call<T: Send + 'static>(
        &self,
        build: impl FnOnce(RpcReplyPort<T>) -> ConfigurationMessage,
    ) -> Result<T, ConfigurationError> {
        match self.actor.call(build, Some(CALL_TIMEOUT)).await {
            Ok(ractor::rpc::CallResult::Success(value)) => Ok(value),
            Ok(ractor::rpc::CallResult::Timeout) => Err(ConfigurationError::Unavailable(
                "timed out".to_string(),
            )),
            Ok(ractor::rpc::CallResult::SenderError) => Err(ConfigurationError::Unavailable(
                "the actor stopped before it answered".to_string(),
            )),
            Err(e) => Err(ConfigurationError::Unavailable(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_env::{Defaulted, Described, Optional, Required, Section};
    use std::collections::HashMap;
    use std::sync::Mutex;

    static HOST: Required<String> = Required::text("HOST", "the host to talk to");
    static LOW: Defaulted<u64> = Defaulted::number("LOW", "lower bound", "1").dynamic();
    static HIGH: Defaulted<u64> = Defaulted::number("HIGH", "upper bound", "10").dynamic();
    static TOKEN: Optional<String> = Optional::text("TOKEN", "an api token")
        .secret()
        .dynamic();
    static VARS: &[&dyn Described] = &[&HOST, &LOW, &HIGH, &TOKEN];

    fn bounds_in_order(env: &dyn polar_env::Env) -> Vec<String> {
        let (low, high) = (LOW.read(env).unwrap(), HIGH.read(env).unwrap());
        if low >= high {
            vec![format!("LOW ({low}) must be below HIGH ({high})")]
        } else {
            vec![]
        }
    }

    static SECTION: Section = Section {
        name: "example",
        vars: VARS,
        relations: bounds_in_order,
    };

    fn config() -> Config {
        let env: HashMap<String, String> = [("HOST".to_string(), "h".to_string())].into();
        Config::builder(&env).register(&SECTION).build()
    }

    async fn handle() -> (ConfigurationHandle, Arc<Mutex<Vec<ConfigChanged>>>) {
        // A parent for the actor to be linked to.
        struct Parent;
        #[ractor::async_trait]
        impl Actor for Parent {
            type Msg = ();
            type State = ();
            type Arguments = ();
            async fn pre_start(&self, _: ActorRef<()>, _: ()) -> Result<(), ActorProcessingErr> {
                Ok(())
            }
        }
        let (parent, _) = Actor::spawn(None, Parent, ()).await.unwrap();
        let handle = ConfigurationHandle::spawn(None, config(), parent.get_cell())
            .await
            .unwrap();

        // A subscriber, as a domain actor would be: it receives a message per change.
        struct Sink(Arc<Mutex<Vec<ConfigChanged>>>);
        #[ractor::async_trait]
        impl Actor for Sink {
            type Msg = ConfigChanged;
            type State = ();
            type Arguments = ();
            async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
                Ok(())
            }
            async fn handle(
                &self,
                _: ActorRef<Self::Msg>,
                message: Self::Msg,
                _: &mut (),
            ) -> Result<(), ActorProcessingErr> {
                self.0.lock().unwrap().push(message);
                Ok(())
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(None, Sink(seen.clone()), ()).await.unwrap();
        handle.changes().subscribe(sink, Some);
        (handle, seen)
    }

    fn change(name: &str, value: &str) -> Vec<(String, String)> {
        vec![(name.to_string(), value.to_string())]
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn a_dynamic_variable_changes_and_subscribers_are_told_once() {
        let (config, seen) = handle().await;
        assert_eq!(config.current().await.unwrap().read(&HIGH).unwrap(), 10);

        let outcome = config.apply(1, change("HIGH", "20")).await.unwrap();
        assert_eq!(
            outcome,
            ApplyOutcome::Applied {
                changed: vec!["HIGH".to_string()]
            }
        );
        assert_eq!(config.current().await.unwrap().read(&HIGH).unwrap(), 20);

        settle().await;
        assert_eq!(
            *seen.lock().unwrap(),
            [ConfigChanged {
                version: 1,
                changed: vec!["HIGH".to_string()]
            }]
        );

        // The report says where the value came from.
        let report = config.report().await.unwrap();
        let high = report.iter().find(|r| r.name == "HIGH").unwrap();
        assert_eq!(high.source, polar_env::Source::Override);
    }

    #[tokio::test]
    async fn a_static_or_unknown_variable_is_refused_by_name_and_nothing_changes() {
        let (config, seen) = handle().await;
        let outcome = config.apply(1, change("HOST", "elsewhere")).await.unwrap();
        let ApplyOutcome::Refused(Refusal::Invalid(problems)) = outcome else {
            panic!("expected a refusal");
        };
        assert!(problems[0].contains("HOST"), "{problems:?}");
        let outcome = config.apply(1, change("NOPE", "1")).await.unwrap();
        let ApplyOutcome::Refused(Refusal::Invalid(problems)) = outcome else {
            panic!("expected a refusal");
        };
        assert!(problems[0].contains("NOPE"), "{problems:?}");

        assert_eq!(
            config.current().await.unwrap().read(&HOST).unwrap(),
            "h"
        );
        settle().await;
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_update_the_startup_check_would_refuse_is_refused_whole() {
        let (config, _) = handle().await;
        // LOW would be above HIGH, and the first change alone would be fine.
        let outcome = config
            .apply(
                1,
                vec![
                    ("HIGH".to_string(), "50".to_string()),
                    ("LOW".to_string(), "60".to_string()),
                ],
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ApplyOutcome::Refused(Refusal::Invalid(_))
        ));
        let current = config.current().await.unwrap();
        assert_eq!((current.read(&LOW).unwrap(), current.read(&HIGH).unwrap()), (1, 10));
    }

    #[tokio::test]
    async fn an_old_or_repeated_version_cannot_be_replayed_over_a_newer_one() {
        let (config, _) = handle().await;
        config.apply(5, change("HIGH", "20")).await.unwrap();
        config.apply(6, change("HIGH", "30")).await.unwrap();

        for stale in [6, 5, 1] {
            let outcome = config.apply(stale, change("HIGH", "99")).await.unwrap();
            assert_eq!(
                outcome,
                ApplyOutcome::Refused(Refusal::Stale { applied: 6 }),
                "version {stale}"
            );
        }
        assert_eq!(config.current().await.unwrap().read(&HIGH).unwrap(), 30);
        // Version zero is never valid: nothing has been applied at zero.
        assert!(matches!(
            config.apply(0, change("HIGH", "40")).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Stale { .. })
        ));
    }

    #[tokio::test]
    async fn a_refused_update_does_not_use_up_its_version() {
        let (config, _) = handle().await;
        let bad = config.apply(1, change("HIGH", "not a number")).await.unwrap();
        assert!(matches!(bad, ApplyOutcome::Refused(Refusal::Invalid(_))));
        // The corrected update can be sent again under the same number.
        let good = config.apply(1, change("HIGH", "12")).await.unwrap();
        assert!(matches!(good, ApplyOutcome::Applied { .. }));
    }

    #[tokio::test]
    async fn a_blank_value_returns_the_variable_to_what_it_was() {
        let (config, seen) = handle().await;
        config.apply(1, change("HIGH", "20")).await.unwrap();
        config.apply(2, change("HIGH", "")).await.unwrap();
        assert_eq!(config.current().await.unwrap().read(&HIGH).unwrap(), 10);
        settle().await;
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_update_that_changes_nothing_advances_the_version_but_tells_no_one() {
        let (config, seen) = handle().await;
        let outcome = config.apply(1, change("HIGH", "10")).await.unwrap();
        assert_eq!(outcome, ApplyOutcome::Applied { changed: vec![] });
        settle().await;
        assert!(seen.lock().unwrap().is_empty());
        assert!(matches!(
            config.apply(1, change("HIGH", "11")).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Stale { applied: 1 })
        ));
    }

    #[tokio::test]
    async fn the_history_records_every_update_and_never_a_secret() {
        let (config, _) = handle().await;
        config
            .apply(1, change("TOKEN", "hunter2-the-secret-value"))
            .await
            .unwrap();
        config.apply(1, change("HIGH", "20")).await.unwrap(); // stale
        config.apply(2, change("LOW", "99")).await.unwrap(); // invalid: LOW above HIGH

        let history = config.history().await.unwrap();
        assert_eq!(history.len(), 3);
        assert!(matches!(history[0].outcome, ApplyOutcome::Applied { .. }));
        assert!(matches!(
            history[1].outcome,
            ApplyOutcome::Refused(Refusal::Stale { .. })
        ));
        assert!(matches!(
            history[2].outcome,
            ApplyOutcome::Refused(Refusal::Invalid(_))
        ));
        let everything = format!("{history:?}{:?}", config.report().await.unwrap());
        assert!(!everything.contains("hunter2"), "{everything}");

        // And the secret is still in force.
        assert_eq!(
            config.current().await.unwrap().read(&TOKEN).unwrap().as_deref(),
            Some("hunter2-the-secret-value")
        );
    }

    #[tokio::test]
    async fn the_history_keeps_only_the_latest() {
        let (config, _) = handle().await;
        for version in 1..=(HISTORY as u64 + 20) {
            config
                .apply(version, change("HIGH", &(10 + version).to_string()))
                .await
                .unwrap();
        }
        let history = config.history().await.unwrap();
        assert_eq!(history.len(), HISTORY);
        assert_eq!(history.first().unwrap().version, 21);
        assert_eq!(history.last().unwrap().version, HISTORY as u64 + 20);
    }
}
