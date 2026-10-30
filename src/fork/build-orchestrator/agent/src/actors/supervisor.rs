use std::sync::Arc;

use crate::actors::build_job::{BuildJobActor, BuildJobArguments};
use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
use crate::client::StorageClient;
use crate::config::RepoMapping;
use crate::mappings;
use cassini_types::ClientEvent;
use git_agent_common::GitRepositoryMessage;
use orchestrator_core::{
    backend::BuildBackend,
    types::{BuildRequest, SourceRef, SourceRefError, subjects::BUILD_REQUESTS_TOPIC},
};
use polar::RkyvError;
use polar::cassini::CassiniClient;
use polar::graph::nodes::git::RepoId;
use polar::topics::GIT_REPOSITORY_EVENTS;
use polar_agent_builder::{
    AgentBuilder, ConfigurationHandle, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rkyv::from_bytes;
use tracing::{error, info, warn};

use chrono::Utc;
use std::collections::HashMap;
use uuid::Uuid;

const REGISTRY_ACTOR_NAME: &str = "build-registry";

/// What the supervisor is told: the broker's events, and the builder's.
#[derive(Debug)]
pub enum SupervisorMessage {
    ClientEvent { event: ClientEvent },
    /// A message on a topic the orchestrator subscribed to, as the router delivers it.
    Incoming { topic: String, payload: Vec<u8> },
    /// The builder has begun an orderly shutdown.
    PrepareShutdown,
    /// A runtime section was replaced.
    RuntimeChanged { section: String },
}

#[derive(Debug, thiserror::Error)]
pub enum EventConversionError {
    #[error(transparent)]
    InvalidSource(#[from] SourceRefError),
}

/// Arguments for supervisor initialization.
pub struct SupervisorArguments {
    pub backend: Arc<dyn BuildBackend>,
    pub config: Arc<crate::config::OrchestratorConfig>,
    pub configured: Configured,
}

pub struct OrchestratorSupervisor;

pub struct SupervisorState {
    backend: Arc<dyn BuildBackend>,
    registry: ActorRef<RegistryMessage>,
    config: Arc<crate::config::OrchestratorConfig>,
    tcp_client: Arc<dyn CassiniClient>,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    configuration: ConfigurationHandle,
    healthcheck: HealthHandle,
    /// The repositories built: the configuration file's list until the `mappings` section arrives, and then
    /// the section's, replaced whole each time.
    mappings: Vec<RepoMapping>,
    draining: bool,
}

impl OrchestratorSupervisor {
    /// Convert a git observer `RefUpdated` into a BuildRequest, or None when
    /// the repo has no mapping in config.
    ///
    /// The observer identifies repos by `RepoId`, which cannot be turned back
    /// into a clone URL. The repo mappings hold the real URLs, and a git build
    /// needs a mapped pipeline image anyway, so a mapping is both the URL
    /// source and the filter: observed repos without one are not built.
    ///
    /// The git scheduler derives `RepoId` from un-normalized URLs, so a mapping
    /// matches whether its URL is written raw or normalized.
    pub fn build_request_from_ref_update(
        config: &crate::config::OrchestratorConfig,
        repo: &RepoId,
        ref_name: &str,
        new_commit: &str,
        observed_at: &str,
    ) -> Result<Option<BuildRequest>, EventConversionError> {
        let Some(mapping) = config.repo_mappings.iter().find(|m| {
            RepoId::from_url(&m.repo_url) == *repo
                || RepoId::from_url(&RepoId::normalize_repo_url(&m.repo_url)) == *repo
        }) else {
            return Ok(None);
        };

        // Validate the commit SHA up front. It is passed straight to git
        // checkout inside the init container, and a bad SHA failing the
        // clone rather than the submission is a confusing failure mode.
        let source = SourceRef::Git {
            url: mapping.repo_url.clone(),
            commit_sha: new_commit.to_string(),
        };
        source.validate()?;

        let metadata = HashMap::from([
            ("git_ref".to_string(), ref_name.to_string()),
            ("observed_at".to_string(), observed_at.to_string()),
        ]);

        Ok(Some(BuildRequest {
            build_id: Uuid::new_v4(),
            source,
            pipeline_image: None,
            requested_by: "polar.git.observer".to_string(),
            requested_at: Utc::now(),
            metadata,
            target_registry: "some.registry".into(), // todo: remove, vestigitial
            command: mapping
                .command
                .clone()
                .unwrap_or_else(|| config.backend.command.clone()),
        }))
    }

    /// Route an inbound message by topic.
    ///
    /// - GIT_REPOSITORY_EVENTS: rkyv `GitRepositoryMessage` from the git
    ///   observer; `RefUpdated` on a mapped repo becomes a git build.
    /// - BUILD_REQUESTS_TOPIC: a JSON `BuildRequest`, e.g. from the analysis
    ///   scheduler. Validated in handle_build_requested like any other.
    async fn deserialize_and_dispatch(
        &self,
        myself: ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        topic: String,
        payload: Vec<u8>,
    ) -> Result<(), ActorProcessingErr> {
        let request = match topic.as_str() {
            GIT_REPOSITORY_EVENTS => {
                match from_bytes::<GitRepositoryMessage, RkyvError>(&payload)? {
                    GitRepositoryMessage::RefUpdated {
                        repo,
                        ref_name,
                        new,
                        observed_at,
                        ..
                    } => match Self::build_request_from_ref_update(
                        &Self::current_config(state),
                        &repo,
                        &ref_name,
                        &new,
                        &observed_at,
                    ) {
                        Ok(Some(request)) => request,
                        Ok(None) => {
                            tracing::debug!(repo = ?repo, "ref update on unmapped repo — not building");
                            return Ok(());
                        }
                        Err(e) => {
                            warn!(error = %e, "ignored malformed RefUpdated");
                            return Ok(());
                        }
                    },
                    // Commits are built when a ref moves to them, not on discovery.
                    GitRepositoryMessage::CommitDiscovered { .. } => return Ok(()),
                }
            }
            BUILD_REQUESTS_TOPIC => serde_json::from_slice::<BuildRequest>(&payload)?,
            other => {
                warn!(topic = %other, "message on unexpected topic — ignoring");
                return Ok(());
            }
        };

        // TODO: deduplicate requests (e.g. a ref observed twice) before building.
        if let Err(e) = Self::handle_build_requested(self, myself, state, request).await {
            tracing::error!(error = %e, "failed to handle build request");
        }
        Ok(())
    }
}

#[ractor::async_trait]
impl Actor for OrchestratorSupervisor {
    type Msg = SupervisorMessage;
    type State = SupervisorState;
    type Arguments = SupervisorArguments;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("OrchestratorSupervisor starting");

        // The broker connection, health, certificates and shutdown are the builder's. The orchestrator holds
        // no graph connection and encrypts nothing: Cassini is its only dependency.
        // The router only delivers topics registered with the builder; a topic subscribed to by hand is dropped.
        let mut builder = AgentBuilder::new(myself.clone(), crate::agent::AGENT_NAME);
        for topic in [GIT_REPOSITORY_EVENTS, BUILD_REQUESTS_TOPIC] {
            let deliver_to = myself.clone();
            builder = builder.with_subscription(Subscription::new(topic, move |incoming| {
                if let Err(e) = deliver_to.cast(SupervisorMessage::Incoming {
                    topic: incoming.topic,
                    payload: incoming.payload,
                }) {
                    warn!("failed to hand a message to the supervisor: {e}");
                }
            }));
        }
        let resources = builder
            .with_lifecycle_handler(|event| Some(SupervisorMessage::ClientEvent { event }))
            .into_configured(args.configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(SupervisorMessage::PrepareShutdown),
            });
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(SupervisorMessage::RuntimeChanged {
                    section: changed.section,
                })
            });
        // A section already held (the replay may have been quicker than this subscription) is read first.
        myself.cast(SupervisorMessage::RuntimeChanged {
            section: mappings::SECTION_NAME.to_string(),
        })?;

        // Spawn the registry as a child of this supervisor.
        let (registry, _) = Actor::spawn_linked(
            Some(REGISTRY_ACTOR_NAME.to_string()),
            BuildRegistryActor,
            (),
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(format!("failed to spawn registry actor: {e}")))?;

        tracing::info!("BuildRegistryActor spawned");

        let mappings = args.config.repo_mappings.clone();
        Ok(SupervisorState {
            backend: args.backend,
            registry,
            config: args.config,
            tcp_client: Arc::new(resources.tcp_client),
            infrastructure: resources.infrastructure,
            configuration: resources.configuration,
            healthcheck: resources.healthcheck,
            mappings,
            draining: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    info!("Orchestrator successfully initialized");
                    let _ = state.healthcheck.cassini_connected();
                }
                // Non-fatal: in-flight builds keep polling the backend while the builder's client
                // reconnects. Whether an outage is long enough to restart is the builder's decision.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            SupervisorMessage::Incoming { topic, payload } => {
                if state.draining {
                    // Builds requested during the drain are dropped, not started.
                    warn!(
                        "draining -- logging message on topic '{topic}' \
                         ({} bytes) instead of dispatching; will be lost on exit",
                        payload.len()
                    );
                } else if let Err(e) =
                    Self::deserialize_and_dispatch(&self, myself, state, topic, payload).await
                {
                    error!("Failed to handle new message. {e}");
                }
            }
            SupervisorMessage::RuntimeChanged { section } => {
                if section == mappings::SECTION_NAME {
                    Self::load_mappings(state).await;
                }
            }
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received: no new builds are taken; jobs already submitted keep running");
                state.draining = true;
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
        event: ractor::SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            ractor::SupervisionEvent::ActorTerminated(actor, _, reason) => {
                tracing::info!(
                    actor = ?actor.get_name(),
                    reason = ?reason,
                    "child actor terminated"
                );
                // The infrastructure is this supervisor's linked child from build(): its ending means the
                // shutdown handshake is done, or that it failed. Either way exit non-zero, so the operator
                // starts a new pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure is down, exiting");
                    std::process::exit(1);
                }
            }
            ractor::SupervisionEvent::ActorFailed(actor, error) => {
                let name = actor.get_name().unwrap_or_default();
                if name == REGISTRY_ACTOR_NAME || actor.get_id() == state.infrastructure.get_id() {
                    // Without these the orchestrator can't do its job: no build state, or no broker.
                    // Exit and let the operator replace it, rather than run deaf and mute.
                    tracing::error!(actor = %name, error = %error, "essential child actor failed -- exiting");
                    if !state.draining {
                        std::process::exit(1);
                    }
                } else if name.starts_with("build-job-") {
                    // A build job actor only fails on a bug or a backend error it didn't handle. Its
                    // registry record keeps its last state; nothing marks it failed.
                    tracing::error!(actor = %name, error = %error, "build job actor failed; its registry record keeps its last state");
                } else {
                    tracing::error!(actor = %name, error = %error, "child actor failed");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl OrchestratorSupervisor {
    /// Reads the latest `mappings` section. A body that does not read keeps the set that was held: the
    /// builder judged it before accepting it, so this is a disagreement between the two, not an author's
    /// mistake.
    async fn load_mappings(state: &mut SupervisorState) {
        let value = match state.configuration.runtime(mappings::SECTION_NAME).await {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(e) => {
                warn!("could not read the mappings section: {e}");
                return;
            }
        };
        match mappings::parse(&value.body) {
            Ok(list) => {
                info!(version = value.version, repositories = list.len(), "mappings section loaded");
                state.mappings = list;
            }
            Err(problems) => error!(
                version = value.version,
                "the mappings section does not read, so the earlier set stays: {}",
                problems.join("; ")
            ),
        }
    }

    /// The configuration as it stands now: the file's, with the repositories the latest section named.
    fn current_config(state: &SupervisorState) -> Arc<crate::config::OrchestratorConfig> {
        Arc::new(crate::config::OrchestratorConfig {
            repo_mappings: state.mappings.clone(),
            ..(*state.config).clone()
        })
    }

    async fn handle_build_requested(
        &self,
        myself: ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        request: BuildRequest,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = request.build_id.clone();
        tracing::info!(
            build_id = %build_id,
            source = %request.source.locator(),
            revision = %request.source.revision(),
            requested_by = %request.requested_by,
            "received build request"
        );

        // Requests may come from anywhere on the broker, not only from
        // build_request_from_event — validate at this boundary too.
        if let Err(e) = request.validate() {
            tracing::warn!(build_id = %build_id, error = %e, "rejected invalid build request");
            return Ok(());
        }

        // Dependency analysis is not ported: its events and graph nodes are not in the fork's library yet.
        if matches!(request.source, SourceRef::RegistryArchive { .. }) {
            tracing::warn!(build_id = %build_id, "rejected an archive build: analysis jobs are not supported on this build");
            return Ok(());
        }

        // Insert into the registry before spawning the actor so there is never
        // a window where the actor exists but has no registry record.
        state
            .registry
            .send_message(RegistryMessage::Insert(request.clone()))?;

        let actor_name = format!("build-job-{}", build_id);

        let client = StorageClient::new(&state.config.storage);

        let args = BuildJobArguments {
            request,
            backend: Arc::clone(&state.backend),
            registry: state.registry.clone(),
            publisher: Arc::clone(&state.tcp_client),
            config: Self::current_config(state),
            storage: Arc::new(client),
        };

        let (_, _handle) =
            Actor::spawn_linked(Some(actor_name), BuildJobActor, args, myself.get_cell())
                .await
                .map_err(|e| {
                    ActorProcessingErr::from(format!(
                        "failed to spawn BuildJobActor for {build_id}: {e}"
                    ))
                })?;

        tracing::info!(build_id = %build_id, "BuildJobActor spawned");
        Ok(())
    }
}
