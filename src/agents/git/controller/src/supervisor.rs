//! The git controller's supervisor, on the agent builder: health, the Cassini client and the shutdown
//! handshake come from `AgentBuilder`. The controller subscribes to nothing; its input is the `repositories`
//! runtime section, which the builder's configuration actor verifies and holds.
//!
//! It announces the whole set on `git.repo.config.events` when it starts, when the section changes, and on a
//! timer. Cassini keeps nothing for a subscriber that is not there yet, so an observer that starts later would
//! otherwise never hear; the observer ignores an announcement it has already acted on.

use crate::desired;
use crate::settings;
use cassini_types::ClientEvent;
use git_agent_common::{
    ConfigurationEvent, GIT_REPO_CONFIG_EVENTS, RepoObservationConfig, publish_event,
};
use polar::cassini::TcpClient;
use polar_agent_builder::{
    AgentBuilder, ConfigurationHandle, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use tracing::{debug, error, info, warn};

/// Nothing is held that needs draining, so this is the margin for the handshake alone.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

pub enum RootSupervisorMessage {
    /// Lifecycle events from the Cassini client.
    ClientEvent { event: ClientEvent },
    /// A runtime section was replaced.
    RuntimeChanged { section: String },
    /// Time to announce the set again.
    Republish,
    /// The infrastructure has decided the agent should end.
    PrepareShutdown,
}

/// Announces every configuration, one event each. One that cannot be published is logged and does not stop the
/// rest. Returns how many were published.
pub fn announce(client: &TcpClient, configs: &[RepoObservationConfig]) -> usize {
    let mut published = 0;
    for config in configs {
        let event = ConfigurationEvent {
            config: config.clone(),
        };
        match publish_event(client, GIT_REPO_CONFIG_EVENTS, &event) {
            Ok(()) => published += 1,
            Err(e) => warn!("could not announce {}: {e}", config.repo_id),
        }
    }
    published
}

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: TcpClient,
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    configuration: ConfigurationHandle,
    /// The repositories of the latest accepted version of the section.
    desired: Vec<RepoObservationConfig>,
    shutting_down: bool,
}

impl RootSupervisor {
    /// Reads the latest `repositories` section into the set to announce. A body that does not read keeps the
    /// set that was held: the builder judged it before accepting it, so this is a disagreement between the two,
    /// not an author's mistake.
    async fn load(state: &mut RootSupervisorState) -> bool {
        let value = match state.configuration.runtime(desired::SECTION_NAME).await {
            Ok(Some(value)) => value,
            Ok(None) => return false,
            Err(e) => {
                warn!("could not read the repositories section: {e}");
                return false;
            }
        };
        match desired::parse(&value.body) {
            Ok(configs) => {
                info!(
                    version = value.version,
                    repositories = configs.len(),
                    "repositories section loaded"
                );
                state.desired = configs;
                true
            }
            Err(problems) => {
                error!(
                    version = value.version,
                    "the repositories section does not read, so the earlier set stays: {}",
                    problems.join("; ")
                );
                false
            }
        }
    }
}

#[async_trait]
impl Actor for RootSupervisor {
    type Msg = RootSupervisorMessage;
    type State = RootSupervisorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        let resources = AgentBuilder::new(myself.clone(), crate::agent::AGENT_NAME)
            .with_lifecycle_handler(|event| Some(RootSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(RootSupervisorMessage::PrepareShutdown)
                }
            });
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(RootSupervisorMessage::RuntimeChanged {
                    section: changed.section,
                })
            });

        // A section already held (the agent started again in the same process, or the replay was quick) is
        // read first; the timer's first announcement follows it.
        myself.cast(RootSupervisorMessage::RuntimeChanged {
            section: desired::SECTION_NAME.to_string(),
        })?;
        myself.cast(RootSupervisorMessage::Republish)?;

        Ok(RootSupervisorState {
            tcp_client: resources.tcp_client,
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            configuration: resources.configuration,
            desired: Vec::new(),
            shutting_down: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            RootSupervisorMessage::ClientEvent { event } => match event {
                // Registered fires on every registration, a resume after a dropped connection included.
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            RootSupervisorMessage::RuntimeChanged { section } => {
                if section == desired::SECTION_NAME
                    && Self::load(state).await
                    && !state.shutting_down
                {
                    announce(&state.tcp_client, &state.desired);
                }
            }
            RootSupervisorMessage::Republish => {
                if state.shutting_down {
                    return Ok(());
                }
                announce(&state.tcp_client, &state.desired);
                let wait = match state.configuration.current().await {
                    Ok(config) => {
                        settings::read_interval(&config).unwrap_or(settings::republish_interval(60))
                    }
                    Err(e) => {
                        warn!("could not read the republish interval, using a minute: {e}");
                        settings::republish_interval(60)
                    }
                };
                drop(myself.send_after(wait, || RootSupervisorMessage::Republish));
            }
            RootSupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received -- nothing to drain");
                state.shutting_down = true;
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorStarted(actor) => debug!("{actor:?} started!"),
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                info!("{actor:?} terminated. {reason:?}");
                // The infrastructure is this supervisor's linked child from build(); its ending means the
                // shutdown handshake is done, or that it failed. Either way exit non-zero so the Job creates
                // a new pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure is down, exiting");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, reason) => {
                error!("{actor:?} failed! {reason:?}");
                if !state.shutting_down {
                    std::process::exit(1);
                }
            }
            _ => (),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_client::TcpClientMessage;
    use git_agent_common::ConfigurationEvent;
    use polar::graph::nodes::git::RepoId;
    use std::sync::{Arc, Mutex};

    struct Sink {
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for Sink {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    fn config(url: &str) -> RepoObservationConfig {
        RepoObservationConfig::new(
            RepoId::from_url(url),
            url.to_string(),
            vec!["origin".into()],
            None,
            Vec::new(),
        )
        .with_credential("gh")
    }

    #[tokio::test]
    async fn every_repository_is_announced_once_on_the_configuration_topic_in_order() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(
            None,
            Sink {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let client = TcpClient::from_actor_ref(sink);
        let configs = [
            config("https://github.com/o/a"),
            config("https://github.com/o/b"),
        ];

        assert_eq!(announce(&client, &configs), 2);
        for _ in 0..200 {
            if received.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        let mut urls = Vec::new();
        for message in received.iter() {
            let TcpClientMessage::Publish { topic, payload, .. } = message else {
                panic!("not a publish: {message:?}");
            };
            assert_eq!(topic, GIT_REPO_CONFIG_EVENTS);
            let mut aligned = rkyv::util::AlignedVec::<16>::new();
            aligned.extend_from_slice(payload);
            let event = rkyv::from_bytes::<ConfigurationEvent, rkyv::rancor::Error>(&aligned)
                .expect("an event the observer can decode");
            assert_eq!(event.config.credential.as_deref(), Some("gh"));
            urls.push(event.config.repo_url);
        }
        assert_eq!(urls, ["https://github.com/o/a", "https://github.com/o/b"]);
    }

    #[tokio::test]
    async fn an_empty_set_announces_nothing() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(
            None,
            Sink {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        assert_eq!(announce(&TcpClient::from_actor_ref(sink), &[]), 0);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(received.lock().unwrap().is_empty());
    }
}
