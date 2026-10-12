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
//! **Updates are signed.** The only way in from outside is [`ConfigurationHandle::submit`], which takes a
//! signed document and runs the checks in [`crate::signed`] first: who signed it, that it is for this
//! agent, and that the signer may make the change. Only a document that passes reaches the checks above.
//! An agent with no CA to verify against refuses every document. It does not depend on Cassini: an agent
//! runs on its environment alone until something submits an update.

use crate::signed::{Rejection, UpdateVerifier};
use polar_env::{Config, Resolved};
use polar_scheduler_common::ConfigurationOutcome;
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
    /// The document was not accepted: it is unsigned, signed by someone untrusted or not allowed to,
    /// altered, or for another agent. Nothing about its contents was acted on.
    Unverified(Rejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The names whose effective value changed (empty if the update changed nothing).
    Applied {
        changed: Vec<String>,
    },
    Refused(Refusal),
}

/// One update, as recorded. Carries names and reasons, never values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEvent {
    /// The version the update carried. Zero for a document refused before its payload could be
    /// trusted, since its claimed version means nothing then.
    pub version: u64,
    pub outcome: ApplyOutcome,
    /// Who signed it (username or identity, and certificate serial), when it was verified.
    pub signer: Option<String>,
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
    /// A signed document delivered on the agent's configuration topic: verified, then applied, with no
    /// reply. Handled in the order delivered; what became of it is in the history and the log.
    Deliver(serde_json::Value),
    /// A signed document from outside: verified, then applied.
    Submit {
        document: serde_json::Value,
        reply: RpcReplyPort<ApplyOutcome>,
    },
    Current(RpcReplyPort<Arc<Config>>),
    History(RpcReplyPort<Vec<ConfigEvent>>),
}

pub struct ConfigurationActor;

pub struct ConfigurationArgs {
    pub config: Config,
    /// How signed documents are checked. `None` refuses every document.
    pub verifier: Option<UpdateVerifier>,
    /// This agent's name, which its outcome events carry.
    pub agent: String,
    pub changes: Arc<OutputPort<ConfigChanged>>,
    /// Where each outcome is announced, for whoever puts it on the wire.
    pub outcomes: Arc<OutputPort<ConfigurationOutcome>>,
}

pub struct ConfigurationState {
    config: Config,
    verifier: Option<UpdateVerifier>,
    agent: String,
    current: Arc<Config>,
    last_version: u64,
    history: VecDeque<ConfigEvent>,
    changes: Arc<OutputPort<ConfigChanged>>,
    outcomes: Arc<OutputPort<ConfigurationOutcome>>,
    /// Outcomes announced so far.
    sequence: u64,
}

/// The id and version a document says it has, read without trusting any of it: only to label an outcome.
fn claimed(document: &serde_json::Value) -> (Option<String>, Option<u64>) {
    let payload = &document["payload"];
    (
        payload["id"].as_str().map(str::to_string),
        payload["version"].as_u64(),
    )
}

/// What an outcome is called in an event, and the reasons that go with it. No value appears in either:
/// the reasons are names and problems, which never repeat a secret.
fn describe_outcome(outcome: &ApplyOutcome) -> (&'static str, Vec<String>, Vec<String>) {
    use polar_scheduler_common::outcome::{APPLIED, INVALID, STALE, UNCHANGED, UNVERIFIED};
    match outcome {
        ApplyOutcome::Applied { changed } if changed.is_empty() => (UNCHANGED, vec![], vec![]),
        ApplyOutcome::Applied { changed } => (APPLIED, changed.clone(), vec![]),
        ApplyOutcome::Refused(Refusal::Stale { applied }) => (
            STALE,
            vec![],
            vec![format!("the version in force is {applied}")],
        ),
        ApplyOutcome::Refused(Refusal::Invalid(problems)) => (INVALID, vec![], problems.clone()),
        ApplyOutcome::Refused(Refusal::Unverified(rejection)) => {
            (UNVERIFIED, vec![], vec![rejection.to_string()])
        }
    }
}

fn describe_signer(signer: &polar_signing::SignerInfo) -> String {
    format!(
        "{} (serial {})",
        signer
            .username
            .as_deref()
            .or(signer.identity.as_deref())
            .unwrap_or("unknown"),
        signer.serial
    )
}

impl ConfigurationState {
    /// A signed document: verified first, applied only if it passes.
    fn submit(&mut self, document: &serde_json::Value) -> ApplyOutcome {
        let Some(verifier) = &self.verifier else {
            return self.reject(claimed(document), Rejection::NotConfigured);
        };
        match verifier.verify(document) {
            Ok(verified) => {
                let signer = describe_signer(&verified.signer);
                info!(id = %verified.id, version = verified.version, %signer, "signed configuration verified");
                self.apply(
                    Some(verified.id),
                    verified.version,
                    &verified.changes,
                    Some(signer),
                )
            }
            Err(rejection) => self.reject(claimed(document), rejection),
        }
    }

    fn reject(
        &mut self,
        (id, version): (Option<String>, Option<u64>),
        rejection: Rejection,
    ) -> ApplyOutcome {
        warn!(%rejection, "signed configuration refused");
        let outcome = ApplyOutcome::Refused(Refusal::Unverified(rejection));
        self.record(id, version, 0, None, &outcome);
        outcome
    }

    /// Keeps the outcome in the history and announces it. `version` is what the history keeps; the
    /// event carries `claimed_version`, which is only meaningful for a verified document.
    fn record(
        &mut self,
        id: Option<String>,
        claimed_version: Option<u64>,
        version: u64,
        signer: Option<String>,
        outcome: &ApplyOutcome,
    ) {
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(ConfigEvent {
            version,
            outcome: outcome.clone(),
            signer: signer.clone(),
        });

        let (status, changed, detail) = describe_outcome(outcome);
        self.sequence += 1;
        self.outcomes.send(ConfigurationOutcome {
            agent: self.agent.clone(),
            id,
            version: claimed_version,
            status: status.to_string(),
            changed,
            detail,
            signer,
            at_unix_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            sequence: self.sequence,
        });
    }

    fn apply(
        &mut self,
        id: Option<String>,
        version: u64,
        changes: &[(String, String)],
        signer: Option<String>,
    ) -> ApplyOutcome {
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
        self.record(id, Some(version), version, signer, &outcome);
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
            verifier: args.verifier,
            agent: args.agent,
            last_version: 0,
            history: VecDeque::new(),
            changes: args.changes,
            outcomes: args.outcomes,
            sequence: 0,
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
                let outcome = state.apply(None, version, &changes, None);
                let _ = reply.send(outcome);
            }
            ConfigurationMessage::Deliver(document) => {
                state.submit(&document);
            }
            ConfigurationMessage::Submit { document, reply } => {
                let outcome = state.submit(&document);
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
    outcomes: Arc<OutputPort<ConfigurationOutcome>>,
}

impl ConfigurationHandle {
    /// Starts the actor on `config`, as a child of `supervisor`.
    pub async fn spawn(
        name: Option<String>,
        agent: impl Into<String>,
        config: Config,
        verifier: Option<UpdateVerifier>,
        supervisor: ractor::ActorCell,
    ) -> Result<Self, ActorProcessingErr> {
        let changes = Arc::new(OutputPort::default());
        let outcomes = Arc::new(OutputPort::default());
        let (actor, _) = Actor::spawn_linked(
            name,
            ConfigurationActor,
            ConfigurationArgs {
                config,
                verifier,
                agent: agent.into(),
                changes: changes.clone(),
                outcomes: outcomes.clone(),
            },
            supervisor,
        )
        .await
        .map_err(ActorProcessingErr::from)?;
        Ok(Self {
            actor,
            changes,
            outcomes,
        })
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

    /// Takes a signed document from outside: verifies it (see [`crate::signed`]), and applies it only
    /// if every check passes. This is the way an update gets in.
    pub async fn submit(
        &self,
        document: serde_json::Value,
    ) -> Result<ApplyOutcome, ConfigurationError> {
        self.call(|reply| ConfigurationMessage::Submit { document, reply })
            .await
    }

    /// Hands over a signed document without waiting for the outcome, in order with the others. The
    /// outcome is recorded in the history and the log.
    pub fn deliver(&self, document: serde_json::Value) {
        if let Err(e) = self.actor.cast(ConfigurationMessage::Deliver(document)) {
            warn!("the configuration actor did not take a delivered document: {e}");
        }
    }

    /// Applies an update without checking who sent it. Only for this crate's own tests: nothing
    /// outside may change the configuration without a verified signature.
    #[cfg(test)]
    pub(crate) async fn apply_unchecked(
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

    /// Where each outcome is announced: every document handled, applied or refused, with the reason.
    /// The infrastructure puts these on Cassini for the scheduler to record.
    pub fn outcomes(&self) -> Arc<OutputPort<ConfigurationOutcome>> {
        self.outcomes.clone()
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
            Ok(ractor::rpc::CallResult::Timeout) => {
                Err(ConfigurationError::Unavailable("timed out".to_string()))
            }
            Ok(ractor::rpc::CallResult::SenderError) => Err(ConfigurationError::Unavailable(
                "the actor stopped before it answered".to_string(),
            )),
            Err(e) => Err(ConfigurationError::Unavailable(e.to_string())),
        }
    }
}

/// The built-in subscription every agent has to its own configuration topic: the scheduler processor
/// publishes a signed document for `agent` there, and it goes to the verifying actor. A message that is not
/// a configuration notification, or is for another agent, is dropped; one that is for this agent is verified
/// by the actor like any other, whoever published it.
pub fn subscription(agent: &str, configuration: ConfigurationHandle) -> crate::Subscription {
    let agent = agent.to_string();
    crate::Subscription::new(
        polar_scheduler_common::configuration_topic(&agent),
        move |incoming| {
            let notification = match rkyv::from_bytes::<
                polar_scheduler_common::ConfigurationNotification,
                rkyv::rancor::Error,
            >(&incoming.payload)
            {
                Ok(notification) => notification,
                Err(e) => {
                    warn!("a message on the configuration topic is not a notification: {e}");
                    return;
                }
            };
            if notification.agent != agent {
                warn!(
                    "a configuration notification for `{}` reached `{agent}`; dropped",
                    notification.agent
                );
                return;
            }
            match serde_json::from_str::<serde_json::Value>(&notification.document_json) {
                Ok(document) => configuration.deliver(document),
                Err(e) => warn!("a delivered configuration document is not JSON: {e}"),
            }
        },
    )
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
    static TOKEN: Optional<String> = Optional::text("TOKEN", "an api token").secret().dynamic();
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
        spawn_handle(None).await
    }

    async fn spawn_handle(
        verifier: Option<UpdateVerifier>,
    ) -> (ConfigurationHandle, Arc<Mutex<Vec<ConfigChanged>>>) {
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
        let handle = ConfigurationHandle::spawn(None, AGENT, config(), verifier, parent.get_cell())
            .await
            .unwrap();

        // A subscriber, as a domain actor would be: it receives a message per change.
        struct Sink(Arc<Mutex<Vec<ConfigChanged>>>);
        #[ractor::async_trait]
        impl Actor for Sink {
            type Msg = ConfigChanged;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _: ActorRef<Self::Msg>,
                _: (),
            ) -> Result<(), ActorProcessingErr> {
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

        let outcome = config
            .apply_unchecked(1, change("HIGH", "20"))
            .await
            .unwrap();
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
        let outcome = config
            .apply_unchecked(1, change("HOST", "elsewhere"))
            .await
            .unwrap();
        let ApplyOutcome::Refused(Refusal::Invalid(problems)) = outcome else {
            panic!("expected a refusal");
        };
        assert!(problems[0].contains("HOST"), "{problems:?}");
        let outcome = config
            .apply_unchecked(1, change("NOPE", "1"))
            .await
            .unwrap();
        let ApplyOutcome::Refused(Refusal::Invalid(problems)) = outcome else {
            panic!("expected a refusal");
        };
        assert!(problems[0].contains("NOPE"), "{problems:?}");

        assert_eq!(config.current().await.unwrap().read(&HOST).unwrap(), "h");
        settle().await;
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_update_the_startup_check_would_refuse_is_refused_whole() {
        let (config, _) = handle().await;
        // LOW would be above HIGH, and the first change alone would be fine.
        let outcome = config
            .apply_unchecked(
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
        assert_eq!(
            (current.read(&LOW).unwrap(), current.read(&HIGH).unwrap()),
            (1, 10)
        );
    }

    #[tokio::test]
    async fn an_old_or_repeated_version_cannot_be_replayed_over_a_newer_one() {
        let (config, _) = handle().await;
        config
            .apply_unchecked(5, change("HIGH", "20"))
            .await
            .unwrap();
        config
            .apply_unchecked(6, change("HIGH", "30"))
            .await
            .unwrap();

        for stale in [6, 5, 1] {
            let outcome = config
                .apply_unchecked(stale, change("HIGH", "99"))
                .await
                .unwrap();
            assert_eq!(
                outcome,
                ApplyOutcome::Refused(Refusal::Stale { applied: 6 }),
                "version {stale}"
            );
        }
        assert_eq!(config.current().await.unwrap().read(&HIGH).unwrap(), 30);
        // Version zero is never valid: nothing has been applied at zero.
        assert!(matches!(
            config
                .apply_unchecked(0, change("HIGH", "40"))
                .await
                .unwrap(),
            ApplyOutcome::Refused(Refusal::Stale { .. })
        ));
    }

    #[tokio::test]
    async fn a_refused_update_does_not_use_up_its_version() {
        let (config, _) = handle().await;
        let bad = config
            .apply_unchecked(1, change("HIGH", "not a number"))
            .await
            .unwrap();
        assert!(matches!(bad, ApplyOutcome::Refused(Refusal::Invalid(_))));
        // The corrected update can be sent again under the same number.
        let good = config
            .apply_unchecked(1, change("HIGH", "12"))
            .await
            .unwrap();
        assert!(matches!(good, ApplyOutcome::Applied { .. }));
    }

    #[tokio::test]
    async fn a_blank_value_returns_the_variable_to_what_it_was() {
        let (config, seen) = handle().await;
        config
            .apply_unchecked(1, change("HIGH", "20"))
            .await
            .unwrap();
        config.apply_unchecked(2, change("HIGH", "")).await.unwrap();
        assert_eq!(config.current().await.unwrap().read(&HIGH).unwrap(), 10);
        settle().await;
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_update_that_changes_nothing_advances_the_version_but_tells_no_one() {
        let (config, seen) = handle().await;
        let outcome = config
            .apply_unchecked(1, change("HIGH", "10"))
            .await
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Applied { changed: vec![] });
        settle().await;
        assert!(seen.lock().unwrap().is_empty());
        assert!(matches!(
            config
                .apply_unchecked(1, change("HIGH", "11"))
                .await
                .unwrap(),
            ApplyOutcome::Refused(Refusal::Stale { applied: 1 })
        ));
    }

    #[tokio::test]
    async fn the_history_records_every_update_and_never_a_secret() {
        let (config, _) = handle().await;
        config
            .apply_unchecked(1, change("TOKEN", "hunter2-the-secret-value"))
            .await
            .unwrap();
        config
            .apply_unchecked(1, change("HIGH", "20"))
            .await
            .unwrap(); // stale
        config
            .apply_unchecked(2, change("LOW", "99"))
            .await
            .unwrap(); // invalid: LOW above HIGH

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
            config
                .current()
                .await
                .unwrap()
                .read(&TOKEN)
                .unwrap()
                .as_deref(),
            Some("hunter2-the-secret-value")
        );
    }

    #[tokio::test]
    async fn the_history_keeps_only_the_latest() {
        let (config, _) = handle().await;
        for version in 1..=(HISTORY as u64 + 20) {
            config
                .apply_unchecked(version, change("HIGH", &(10 + version).to_string()))
                .await
                .unwrap();
        }
        let history = config.history().await.unwrap();
        assert_eq!(history.len(), HISTORY);
        assert_eq!(history.first().unwrap().version, 21);
        assert_eq!(history.last().unwrap().version, HISTORY as u64 + 20);
    }

    // -- signed documents -------------------------------------------------------------------

    use polar_signing::Trust;
    use polar_signing::testing::{self as certs, Spec, now, root, signer};
    use serde_json::{Value, json};

    const AGENT: &str = "hello-agent";

    struct Signed {
        ca: certs::Authority,
    }

    impl Signed {
        fn new() -> Self {
            Self {
                ca: root("Polar Internal CA"),
            }
        }

        fn verifier(&self) -> UpdateVerifier {
            UpdateVerifier::new(
                Trust::from_pem(std::slice::from_ref(&self.ca.pem)).unwrap(),
                AGENT,
            )
            .with_clock(now)
        }

        fn update(&self, spec: Spec, version: u64, agent: &str, changes: Value) -> Value {
            let s = signer(&self.ca, spec);
            polar_signing::sign(
                json!({"type": "configuration", "id": "u", "target": {"agent": agent}, "version": version, "changes": changes}),
                &[s.cert_pem],
                &s.key_pem,
            )
            .unwrap()
            .to_value()
        }

        fn high(&self, version: u64, value: &str) -> Value {
            self.update(
                Spec::default(),
                version,
                AGENT,
                json!([{"name": "HIGH", "value": value}]),
            )
        }
    }

    async fn high(config: &ConfigurationHandle) -> u64 {
        config.current().await.unwrap().read(&HIGH).unwrap()
    }

    #[tokio::test]
    async fn a_signed_update_is_applied_and_the_history_says_who_signed_it() {
        let signed = Signed::new();
        let (config, seen) = spawn_handle(Some(signed.verifier())).await;
        let outcome = config.submit(signed.high(1, "20")).await.unwrap();
        assert_eq!(
            outcome,
            ApplyOutcome::Applied {
                changed: vec!["HIGH".to_string()]
            }
        );
        assert_eq!(high(&config).await, 20);
        settle().await;
        assert_eq!(seen.lock().unwrap().len(), 1);

        let history = config.history().await.unwrap();
        assert_eq!(history[0].version, 1);
        let who = history[0].signer.as_deref().unwrap();
        assert!(who.contains("alice") && who.contains("serial"), "{who}");
    }

    #[tokio::test]
    async fn an_unsigned_update_is_refused_and_the_value_stays() {
        let signed = Signed::new();
        let (config, seen) = spawn_handle(Some(signed.verifier())).await;
        // What an unsigned update would look like: the payload, bare.
        let bare = json!({"type": "configuration", "id": "u", "target": {"agent": AGENT}, "version": 1,
                          "changes": [{"name": "HIGH", "value": "20"}]});
        let outcome = config.submit(bare).await.unwrap();
        assert!(matches!(
            outcome,
            ApplyOutcome::Refused(Refusal::Unverified(Rejection::Layout(_)))
        ));
        assert_eq!(high(&config).await, 10);
        settle().await;
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_wrongly_signed_update_is_refused_and_the_value_stays() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;

        // Altered after signing.
        let mut tampered = signed.high(1, "20");
        tampered["payload"]["changes"][0]["value"] = json!("50");
        assert!(matches!(
            config.submit(tampered).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Unverified(Rejection::Signature(_)))
        ));

        // Signed by a CA this agent does not trust.
        let rogue = Signed::new();
        assert!(matches!(
            config.submit(rogue.high(1, "20")).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Unverified(Rejection::Signature(_)))
        ));
        assert_eq!(high(&config).await, 10);
    }

    #[tokio::test]
    async fn an_update_from_a_signer_who_may_not_is_refused_and_the_value_stays() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let doc = signed.update(
            Spec {
                capabilities: vec!["c:grant"],
                ..Spec::default()
            },
            1,
            AGENT,
            json!([{"name": "HIGH", "value": "20"}]),
        );
        assert!(matches!(
            config.submit(doc).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Unverified(Rejection::Unauthorized(_)))
        ));
        assert_eq!(high(&config).await, 10);
    }

    #[tokio::test]
    async fn a_valid_old_or_repeated_document_cannot_be_replayed() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let five = signed.high(5, "20");
        config.submit(five.clone()).await.unwrap();
        config.submit(signed.high(6, "30")).await.unwrap();
        assert_eq!(high(&config).await, 30);

        // The very same signed document again, and an older one: both verify, and both are stale.
        for replay in [five, signed.high(3, "99")] {
            assert_eq!(
                config.submit(replay).await.unwrap(),
                ApplyOutcome::Refused(Refusal::Stale { applied: 6 })
            );
        }
        assert_eq!(high(&config).await, 30);
    }

    #[tokio::test]
    async fn a_document_signed_for_another_agent_is_refused() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let doc = signed.update(
            Spec::default(),
            1,
            "another-agent",
            json!([{"name": "HIGH", "value": "20"}]),
        );
        assert!(matches!(
            config.submit(doc).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Unverified(Rejection::WrongTarget { .. }))
        ));
        assert_eq!(high(&config).await, 10);
    }

    #[tokio::test]
    async fn an_agent_with_no_ca_to_verify_against_refuses_everything() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(None).await;
        assert_eq!(
            config.submit(signed.high(1, "20")).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Unverified(Rejection::NotConfigured))
        );
        assert_eq!(high(&config).await, 10);
    }

    #[tokio::test]
    async fn a_signature_does_not_let_a_signer_change_what_the_agent_does_not_allow() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        // HOST is Static, LOW above HIGH breaks a relation: a valid signature is not a way round either.
        let doc = signed.update(
            Spec::default(),
            1,
            AGENT,
            json!([{"name": "HOST", "value": "elsewhere"}]),
        );
        assert!(matches!(
            config.submit(doc).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Invalid(_))
        ));
        let doc = signed.update(
            Spec::default(),
            2,
            AGENT,
            json!([{"name": "LOW", "value": "99"}]),
        );
        assert!(matches!(
            config.submit(doc).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Invalid(_))
        ));
        assert_eq!(config.current().await.unwrap().read(&HOST).unwrap(), "h");
        // Refused for its contents, but it was verified, so the history says who sent it.
        let history = config.history().await.unwrap();
        assert!(history.iter().all(|e| e.signer.is_some()));
    }

    #[tokio::test]
    async fn refusals_and_the_history_never_hold_a_secret_value() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let secret = "hunter2-the-secret-value";

        // Applied: the value is in force and recorded nowhere.
        let applied = signed.update(
            Spec::default(),
            1,
            AGENT,
            json!([{"name": "TOKEN", "value": secret}]),
        );
        config.submit(applied).await.unwrap();
        // Refused after tampering, and refused for the wrong agent: neither repeats it.
        let mut tampered = signed.update(
            Spec::default(),
            2,
            AGENT,
            json!([{"name": "TOKEN", "value": secret}]),
        );
        tampered["payload"]["version"] = json!(9);
        let outcomes = vec![
            config.submit(tampered).await.unwrap(),
            config
                .submit(signed.update(
                    Spec::default(),
                    3,
                    "another-agent",
                    json!([{"name": "TOKEN", "value": secret}]),
                ))
                .await
                .unwrap(),
        ];
        let everything = format!(
            "{outcomes:?}{:?}{:?}",
            config.history().await.unwrap(),
            config.report().await.unwrap()
        );
        assert!(!everything.contains("hunter2"), "{everything}");
        assert_eq!(
            config
                .current()
                .await
                .unwrap()
                .read(&TOKEN)
                .unwrap()
                .as_deref(),
            Some(secret)
        );
    }

    // -- delivery on the agent's configuration topic ----------------------------------------

    fn notification(agent: &str, document: &Value) -> polar_router::IncomingMessage {
        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(
            &polar_scheduler_common::ConfigurationNotification {
                agent: agent.to_string(),
                document_json: document.to_string(),
            },
        )
        .unwrap()
        .to_vec();
        polar_router::IncomingMessage {
            topic: polar_scheduler_common::configuration_topic(agent),
            payload,
            published_by: None,
            trace_ctx: None,
        }
    }

    #[tokio::test]
    async fn a_document_delivered_on_the_topic_is_verified_and_applied_in_order() {
        let signed = Signed::new();
        let (config, seen) = spawn_handle(Some(signed.verifier())).await;
        let sub = subscription(AGENT, config.clone());
        assert_eq!(sub.topic, "agent.hello-agent.configuration");

        // Delivered back to back: both are applied, the later last.
        (sub.deliver)(notification(AGENT, &signed.high(1, "20")));
        (sub.deliver)(notification(AGENT, &signed.high(2, "30")));
        settle().await;
        assert_eq!(high(&config).await, 30);
        assert_eq!(seen.lock().unwrap().len(), 2);
        let history = config.history().await.unwrap();
        assert_eq!(
            history.iter().map(|e| e.version).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(history.iter().all(|e| e.signer.is_some()));
    }

    #[tokio::test]
    async fn what_arrives_on_the_topic_is_verified_whoever_published_it() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let sub = subscription(AGENT, config.clone());

        // Unsigned, and signed by a CA this agent does not trust: delivered, then refused.
        let unsigned = json!({"payload": {"type": "configuration", "id": "u", "target": {"agent": AGENT}, "version": 1,
                              "changes": [{"name": "HIGH", "value": "99"}]}, "signature": {}});
        (sub.deliver)(notification(AGENT, &unsigned));
        (sub.deliver)(notification(AGENT, &Signed::new().high(2, "99")));
        settle().await;
        assert_eq!(high(&config).await, 10);
        let history = config.history().await.unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .all(|e| matches!(e.outcome, ApplyOutcome::Refused(Refusal::Unverified(_))))
        );
    }

    #[tokio::test]
    async fn a_notification_for_another_agent_or_that_is_not_one_is_dropped() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let sub = subscription(AGENT, config.clone());

        (sub.deliver)(notification("another-agent", &signed.high(1, "20")));
        (sub.deliver)(polar_router::IncomingMessage {
            topic: sub.topic.clone(),
            payload: vec![1, 2, 3],
            published_by: None,
            trace_ctx: None,
        });
        // A notification whose document is not JSON.
        let mut bad = notification(AGENT, &json!({}));
        bad.payload = rkyv::to_bytes::<rkyv::rancor::Error>(
            &polar_scheduler_common::ConfigurationNotification {
                agent: AGENT.to_string(),
                document_json: "not json".to_string(),
            },
        )
        .unwrap()
        .to_vec();
        (sub.deliver)(bad);
        settle().await;
        // None of them reached the actor, so nothing is recorded and the value is as it was.
        assert!(config.history().await.unwrap().is_empty());
        assert_eq!(high(&config).await, 10);
    }

    // -- outcomes -----------------------------------------------------------------------------

    struct OutcomeSink(Arc<Mutex<Vec<ConfigurationOutcome>>>);

    #[ractor::async_trait]
    impl Actor for OutcomeSink {
        type Msg = ConfigurationOutcome;
        type State = Arc<Mutex<Vec<ConfigurationOutcome>>>;
        type Arguments = ();
        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(self.0.clone())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            state.lock().unwrap().push(message);
            Ok(())
        }
    }

    async fn watch(config: &ConfigurationHandle) -> Arc<Mutex<Vec<ConfigurationOutcome>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(None, OutcomeSink(seen.clone()), ())
            .await
            .unwrap();
        config.outcomes().subscribe(sink, Some);
        seen
    }

    fn statuses(seen: &Arc<Mutex<Vec<ConfigurationOutcome>>>) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|o| o.status.clone())
            .collect()
    }

    #[tokio::test]
    async fn every_document_is_announced_with_what_became_of_it() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let seen = watch(&config).await;

        config.submit(signed.high(1, "20")).await.unwrap(); // applied
        config.submit(signed.high(2, "20")).await.unwrap(); // verified, changes nothing
        config.submit(signed.high(2, "30")).await.unwrap(); // not newer
        config
            .submit(signed.update(
                Spec::default(),
                3,
                AGENT,
                json!([{"name": "HOST", "value": "elsewhere"}]),
            ))
            .await
            .unwrap(); // static
        let mut tampered = signed.high(4, "40");
        tampered["payload"]["version"] = json!(9);
        config.submit(tampered).await.unwrap(); // altered
        settle().await;

        assert_eq!(
            statuses(&seen),
            ["applied", "unchanged", "stale", "invalid", "unverified"]
        );
        let all = seen.lock().unwrap().clone();

        // Applied: names what changed, says who signed it, and what it claimed.
        assert_eq!(all[0].agent, AGENT);
        assert_eq!(all[0].id.as_deref(), Some("u"));
        assert_eq!(all[0].version, Some(1));
        assert_eq!(all[0].changed, vec!["HIGH".to_string()]);
        assert!(all[0].signer.as_deref().unwrap().contains("alice"));
        assert!(all[0].at_unix_secs > 1_700_000_000);

        // Refused for what it asks: the reason names the variable.
        assert!(
            all[3].detail.iter().any(|d| d.contains("HOST")),
            "{:?}",
            all[3].detail
        );
        assert!(all[3].signer.is_some());

        // Not verified: no signer, and the reason says why. What it claimed is only a label.
        assert!(all[4].signer.is_none());
        assert_eq!(all[4].version, Some(9));
        assert!(!all[4].detail.is_empty());

        // Counted, so two in the same second are two events.
        assert_eq!(
            all.iter().map(|o| o.sequence).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
    }

    #[tokio::test]
    async fn a_document_delivered_on_the_topic_is_announced_too() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let seen = watch(&config).await;
        let sub = subscription(AGENT, config.clone());
        (sub.deliver)(notification(AGENT, &signed.high(1, "20")));
        // Garbage and unsigned: the unsigned one is announced, the garbage never reached the actor.
        (sub.deliver)(polar_router::IncomingMessage {
            topic: sub.topic.clone(),
            payload: vec![1, 2],
            published_by: None,
            trace_ctx: None,
        });
        (sub.deliver)(notification(
            AGENT,
            &json!({"payload": {"id": "x"}, "signature": {}}),
        ));
        settle().await;
        assert_eq!(statuses(&seen), ["applied", "unverified"]);
        assert_eq!(seen.lock().unwrap()[1].id.as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn an_agent_with_no_ca_announces_that_it_refused_everything() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(None).await;
        let seen = watch(&config).await;
        config.submit(signed.high(1, "20")).await.unwrap();
        settle().await;
        assert_eq!(statuses(&seen), ["unverified"]);
        assert!(seen.lock().unwrap()[0].detail[0].contains("no CA"));
    }

    #[tokio::test]
    async fn no_outcome_ever_holds_a_secret_value() {
        let signed = Signed::new();
        let (config, _) = spawn_handle(Some(signed.verifier())).await;
        let seen = watch(&config).await;
        let secret = "hunter2-the-secret-value";

        // Applied, refused after tampering, refused for another agent, and refused for a bad
        // companion value in the same document: none repeats the secret.
        config
            .submit(signed.update(
                Spec::default(),
                1,
                AGENT,
                json!([{"name": "TOKEN", "value": secret}]),
            ))
            .await
            .unwrap();
        let mut tampered = signed.update(
            Spec::default(),
            2,
            AGENT,
            json!([{"name": "TOKEN", "value": secret}]),
        );
        tampered["payload"]["version"] = json!(9);
        config.submit(tampered).await.unwrap();
        config
            .submit(signed.update(
                Spec::default(),
                3,
                "another-agent",
                json!([{"name": "TOKEN", "value": secret}]),
            ))
            .await
            .unwrap();
        config
            .submit(signed.update(
                Spec::default(),
                4,
                AGENT,
                json!([{"name": "TOKEN", "value": secret}, {"name": "LOW", "value": "99"}]),
            ))
            .await
            .unwrap();
        settle().await;
        let everything = format!("{:?}", seen.lock().unwrap());
        assert!(!everything.contains("hunter2"), "{everything}");
        assert_eq!(seen.lock().unwrap().len(), 4);
    }
}
