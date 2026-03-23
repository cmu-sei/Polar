use std::sync::Arc;

use orchestrator_core::{backend::BuildBackend, types::BuildRequest};
use ractor::{Actor, ActorProcessingErr, ActorRef};

use crate::actors::build_job::{BuildJobActor, BuildJobArguments};
use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
use crate::cassini::CassiniPublisher;
use crate::client::StorageClient;

/// Messages the OrchestratorSupervisor accepts.
pub enum SupervisorMessage {
    /// A new BuildRequest has arrived from the Cassini consumer.
    BuildRequested(BuildRequest),
}

/// Arguments for supervisor initialization.
pub struct SupervisorArguments {
    pub backend: Arc<dyn BuildBackend>,
    pub publisher: Arc<dyn CassiniPublisher>,
    pub config: Arc<crate::config::OrchestratorConfig>,
}

pub struct OrchestratorSupervisor;

pub struct SupervisorState {
    backend: Arc<dyn BuildBackend>,
    publisher: Arc<dyn CassiniPublisher>,
    registry: ActorRef<RegistryMessage>,
    config: Arc<crate::config::OrchestratorConfig>,
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
            publisher: args.publisher,
            registry,
            config: args.config,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::BuildRequested(request) => {
                self.handle_build_requested(myself, state, request).await?;
            }
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
        let build_id = request.build_id;
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
            publisher: Arc::clone(&state.publisher),
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
