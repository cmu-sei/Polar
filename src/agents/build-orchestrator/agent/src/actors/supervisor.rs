use std::sync::Arc;

use crate::actors::build_job::{BuildJobActor, BuildJobArguments};
use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
use crate::client::StorageClient;
use cassini_types::ClientEvent;
use orchestrator_core::{backend::BuildBackend, types::BuildRequest};
use polar::cassini::{CassiniClient, TcpClient};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{GitRepositoryUpdatedEvent, RkyvError, SupervisorMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use rkyv::from_bytes;
use tracing::{error, info, warn};

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use uuid::Uuid;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const BUILD_REGISTRY_ACTOR_NAME: &str = "build-registry";
const DRAIN_WINDOW_SECS: u64 = 5;

#[derive(Debug, thiserror::Error)]
pub enum EventConversionError {
    #[error("event has neither http_url nor ssh_url")]
    NoCloneUrl,

    #[error("commit_sha is not a valid 40-character hex SHA: {0}")]
    InvalidCommitSha(String),

    #[error("event_id is empty")]
    EmptyEventId,
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
    /// Convert an inbound GitRepositoryUpdatedEvent into a BuildRequest
    /// suitable for submission to the OrchestratorSupervisor.
    ///
    /// This is the boundary between Polar's event schema and Cyclops's internal
    /// domain types. Validation happens here — anything that reaches the
    /// supervisor is guaranteed to be well-formed.
    ///
    /// URL preference: http_url is preferred over ssh_url. The clone init
    /// container currently only supports HTTP token auth. When the identity
    /// root agent adds SSH credential issuance, this preference should be
    /// made configurable per repo mapping.
    pub fn build_request_from_event(
        state: &mut SupervisorState,
        event: &GitRepositoryUpdatedEvent,
    ) -> Result<BuildRequest, EventConversionError> {
        if event.event_id.trim().is_empty() {
            return Err(EventConversionError::EmptyEventId);
        }

        if !Self::is_valid_sha(&event.commit_sha) {
            return Err(EventConversionError::InvalidCommitSha(
                event.commit_sha.clone(),
            ));
        }

        let repo_url = event
            .http_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .or_else(|| event.ssh_url.as_deref().filter(|u| !u.is_empty()))
            .ok_or(EventConversionError::NoCloneUrl)?
            .to_string();

        let mut metadata = HashMap::new();
        metadata.insert("event_id".to_string(), event.event_id.clone());
        if let Some(ref git_ref) = event.git_ref {
            metadata.insert("git_ref".to_string(), git_ref.clone());
        }
        if let Some(ref branch) = event.default_branch {
            metadata.insert("default_branch".to_string(), branch.clone());
        }
        if let Some(ref name) = event.repository_name {
            metadata.insert("repository_name".to_string(), name.clone());
        }
        if let Some(ref author) = event.author {
            metadata.insert("author".to_string(), author.clone());
        }

        Ok(BuildRequest {
            build_id: Uuid::new_v4(),
            repo_url,
            commit_sha: event.commit_sha.clone(),
            requested_by: event
                .author
                .clone()
                .unwrap_or_else(|| "polar-observer".to_string()),
            requested_at: DateTime::from_timestamp(event.observed_at, 0).unwrap_or_else(Utc::now),
            metadata,
            target_registry: "some.registry".into(), // todo: remove, vestigitial
            command: state.config.backend.command.clone(),
        })
    }

    fn is_valid_sha(sha: &str) -> bool {
        sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())
    }

    async fn deserialize_and_dispatch(
        &self,
        myself: ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        _topic: String,
        payload: Vec<u8>,
    ) -> Result<(), ActorProcessingErr> {
        let event = from_bytes::<GitRepositoryUpdatedEvent, RkyvError>(&payload)?;

        match Self::build_request_from_event(state, &event) {
            Ok(request) => {
                if let Err(e) = Self::handle_build_requested(&self, myself, state, request).await {
                    tracing::error!(error = %e, "failed to handle build request");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Ignored malformed GitRepositoryUpdatedEvent");
            }
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

        // No graph controller is spawned in this supervisor. If
        // BuildJobActor touches Neo4j independently (unconfirmed --
        // build_job.rs not reviewed as part of this pass), graph
        // availability tracking would need to be added there separately,
        // same limitation as jira-processor's children.
        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
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
        .map_err(|e| ActorProcessingErr::from(e))?;

        let tcp_client =
            TcpClient::spawn("polar.cyclops.supervisor.tcp", myself.clone(), |event| {
                Some(SupervisorMessage::ClientEvent { event })
            })
            .await?;

        let (registry, _) = Actor::spawn_linked(
            Some(BUILD_REGISTRY_ACTOR_NAME.to_string()),
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
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            SupervisorMessage::GraphSignal(_) => {}
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    info!("Orchestrator successfully initialized");
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                        return Ok(());
                    }
                    if let Err(e) =
                        Self::deserialize_and_dispatch(&self, myself, state, topic, payload).await
                    {
                        error!("Failed to handle new message. {e}");
                    }
                }
                // BUG FIX: was `myself.stop(Some(reason))` -- silent exit-0,
                // Job marks Completed, no restart, no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => (),
            },
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
                let actor_name = actor.get_name().unwrap_or_default();
                tracing::info!(
                    actor = ?actor_name,
                    reason = ?reason,
                    "child actor terminated"
                );
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            ractor::SupervisionEvent::ActorFailed(actor, error) => {
                let actor_name = actor.get_name().unwrap_or_default();
                tracing::error!(
                    actor = ?actor_name,
                    error = %error,
                    "child actor failed"
                );

                // BUG FIX: the comment here previously stated "the registry
                // actor dying is a fatal condition for the supervisor," but
                // nothing in the code actually checked which actor failed
                // or acted on that -- every failure, including the
                // registry's, was just logged. Now the documented intent
                // is actually enforced: registry (or healthcheck) failure
                // drains and exits; individual build job actor failures
                // remain expected/non-fatal, matching the original comment.
                if actor_name == HEALTHCHECK_ACTOR_NAME || actor_name == BUILD_REGISTRY_ACTOR_NAME {
                    if !state.draining {
                        state.draining = true;
                        warn!("entering drain window before exit ({actor_name} failed)");
                        let _ = myself
                            .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                                SupervisorMessage::ForceExit
                            })
                            .await;
                    }
                } else {
                    tracing::info!(
                        actor = ?actor_name,
                        "build job actor failure is expected; build marked failed via registry, no supervisor action"
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl OrchestratorSupervisor {
    async fn handle_build_requested(
        &self,
        myself: ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        request: BuildRequest,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = request.build_id.clone();
        tracing::info!(
            build_id = %build_id,
            repo = %request.repo_url,
            sha = %request.commit_sha,
            requested_by = %request.requested_by,
            "received build request"
        );

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
