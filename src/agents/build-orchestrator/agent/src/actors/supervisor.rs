use std::sync::Arc;

use crate::actors::build_job::{BuildJobActor, BuildJobArguments};
use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
use crate::client::StorageClient;
use cassini_types::ClientEvent;
use orchestrator_core::{backend::BuildBackend, types::BuildRequest};
use polar::cassini::{CassiniClient, TcpClient};
use polar::{GitRepositoryUpdatedEvent, RkyvError, SupervisorMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rkyv::from_bytes;
use tracing::{error, info};

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use uuid::Uuid;

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
        event: &GitRepositoryUpdatedEvent,
    ) -> Result<BuildRequest, EventConversionError> {
        // Validate event_id first — a missing ID means we can't deduplicate.
        if event.event_id.trim().is_empty() {
            return Err(EventConversionError::EmptyEventId);
        }

        // Validate commit SHA — must be exactly 40 lowercase hex chars.
        // The orchestrator passes this directly to git checkout inside the
        // init container. A bad SHA fails the clone, not the submission,
        // which produces a confusing failure mode.
        if !Self::is_valid_sha(&event.commit_sha) {
            return Err(EventConversionError::InvalidCommitSha(
                event.commit_sha.clone(),
            ));
        }

        // Resolve clone URL — prefer HTTP, fall back to SSH.
        let repo_url = event
            .http_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .or_else(|| event.ssh_url.as_deref().filter(|u| !u.is_empty()))
            .ok_or(EventConversionError::NoCloneUrl)?
            .to_string();

        // Forward informational fields as metadata so Polar can correlate
        // build records with branch activity without the orchestrator needing
        // to understand the semantics of each field.
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
        // TODO: Deserialize repository updated event.
        // let ev: CyclopsEvent = serde_json::from_slice(&payload)?;
        let event = from_bytes::<GitRepositoryUpdatedEvent, RkyvError>(&payload)?;

        // TODO: check if we're already handling this event, if we are, just ignore it
        // if !self.deduplicator.check_and_record(&event.event_id) {
        //     tracing::debug!(event_id = %event.event_id, "duplicate event dropped");
        //     return Ok(());
        // }

        match Self::build_request_from_event(&event) {
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

        // spawn a cassini client
        let tcp_client =
            TcpClient::spawn("polar.cyclops.supervisor.tcp", myself.clone(), |event| {
                Some(SupervisorMessage::ClientEvent { event })
            })
            .await?;

        //
        //
        // Spawn the registry as a child of this supervisor.
        let (registry, _) = Actor::spawn_linked(
            Some("build-registry".to_string()),
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
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if let Err(e) =
                        Self::deserialize_and_dispatch(&self, myself, state, topic, payload).await
                    {
                        error!("Failed to handle new message. {e}");
                        // TODO: handle
                    }
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error occurred! {reason}");
                    myself.stop(Some(reason))
                }
                _ => (),
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: ractor::SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            ractor::SupervisionEvent::ActorTerminated(actor, _, reason) => {
                tracing::info!(
                    actor = ?actor.get_name(),
                    reason = ?reason,
                    "child actor terminated"
                );
            }
            ractor::SupervisionEvent::ActorFailed(actor, error) => {
                tracing::error!(
                    actor = ?actor.get_name(),
                    error = %error,
                    "child actor failed — build job will be marked failed via registry"
                );
                // The registry actor dying is a fatal condition for the supervisor.
                // Individual build job actor failures are expected (they stop themselves
                // on terminal states) and do not require supervisor intervention.
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
