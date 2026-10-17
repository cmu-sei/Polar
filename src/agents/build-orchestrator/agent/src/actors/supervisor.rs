use std::sync::Arc;

use crate::actors::build_job::{BuildJobActor, BuildJobArguments};
use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
use crate::client::StorageClient;
use cassini_types::ClientEvent;
use git_agent_common::GitRepositoryMessage;
use orchestrator_core::{
    backend::BuildBackend,
    types::{BuildRequest, SourceRef, SourceRefError, subjects::BUILD_REQUESTS_TOPIC},
};
use polar::cassini::{CassiniClient, SubscribeRequest, TcpClient};
use polar::graph::nodes::git::RepoId;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::topics::GIT_REPOSITORY_EVENTS;
use polar::{RkyvError, SupervisorMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use rkyv::from_bytes;
use tracing::{error, info, warn};

use chrono::Utc;
use std::collections::HashMap;
use uuid::Uuid;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

/// Service name the broker client is spawned under. `TcpClient::spawn` names
/// the client actor `<service>.tcp`; see `BROKER_CLIENT_ACTOR`.
const BROKER_CLIENT_SERVICE: &str = "polar.cyclops.supervisor.tcp";
const REGISTRY_ACTOR_NAME: &str = "build-registry";

/// The broker client's actor name, as `TcpClient::spawn` derives it.
fn broker_client_actor() -> String {
    format!("{BROKER_CLIENT_SERVICE}.tcp")
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
}

pub struct OrchestratorSupervisor;

pub struct SupervisorState {
    backend: Arc<dyn BuildBackend>,
    registry: ActorRef<RegistryMessage>,
    config: Arc<crate::config::OrchestratorConfig>,
    tcp_client: Arc<dyn CassiniClient>,
    healthcheck: ActorRef<HealthCheckMessage>,
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
            command: config.backend.command.clone(),
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
                        &state.config,
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

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // The orchestrator holds no graph connection; Cassini is its only
        // certificate-bearing dependency.
        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(ActorProcessingErr::from)?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: false,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // spawn a cassini client
        let tcp_client = TcpClient::spawn(BROKER_CLIENT_SERVICE, myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        })
        .await?;

        //
        //
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

        Ok(SupervisorState {
            backend: args.backend,
            registry,
            config: args.config,
            tcp_client: Arc::new(tcp_client),
            healthcheck,
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
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    for topic in [GIT_REPOSITORY_EVENTS, BUILD_REQUESTS_TOPIC] {
                        state.tcp_client.subscribe(SubscribeRequest {
                            topic: topic.to_string(),
                            trace_ctx: None,
                        })?;
                        info!("subscribed to {topic}");
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        // Builds requested during the drain window are dropped,
                        // not started — a job spawned now would be killed on exit.
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                    } else if let Err(e) =
                        Self::deserialize_and_dispatch(&self, myself, state, topic, payload).await
                    {
                        error!("Failed to handle new message. {e}");
                        // TODO: handle
                    }
                }
                // Non-fatal: in-flight builds keep polling the backend while the
                // client reconnects. The healthcheck decides if the outage is
                // long enough to warrant a restart.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => (),
            },
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            // No graph controller in this agent.
            SupervisorMessage::GraphSignal(_) => {}
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
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
                if actor.get_name().as_deref() == Some(HEALTHCHECK_ACTOR_NAME) {
                    Self::begin_drain(&myself, state);
                }
            }
            ractor::SupervisionEvent::ActorFailed(actor, error) => {
                let name = actor.get_name().unwrap_or_default();
                if name == HEALTHCHECK_ACTOR_NAME
                    || name == REGISTRY_ACTOR_NAME
                    || name == broker_client_actor()
                {
                    // Without any of these the orchestrator can't do its job:
                    // no health reporting, no build state, or (broker client)
                    // no requests in and no events out. The broker client
                    // gives up for good after its connect retries, so exit and
                    // let the platform restart the orchestrator, as the other
                    // agents do, rather than keep running deaf and mute.
                    tracing::error!(actor = %name, error = %error, "essential child actor failed — exiting");
                    Self::begin_drain(&myself, state);
                } else if name.starts_with("build-job-") {
                    // A build job actor only fails on a bug or a backend error
                    // it didn't handle. Its registry record keeps its last
                    // state; nothing marks it failed.
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
    /// Stop accepting new builds and schedule a ForceExit. Called when the
    /// healthcheck actor stops (time to rejuvenate) or an essential child
    /// actor fails.
    fn begin_drain(myself: &ActorRef<SupervisorMessage>, state: &mut SupervisorState) {
        if state.draining {
            return;
        }
        state.draining = true;
        info!("entering drain window before exit");
        myself.send_after(
            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
            || SupervisorMessage::ForceExit,
        );
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
            config: Arc::clone(&state.config),
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
