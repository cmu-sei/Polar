use crate::projection::project_event;
use polar::{ProvenanceEvent, graph::controller::GraphController};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::{debug, error, warn};
/// Actor that owns build execution lifecycle projection.
///
/// Receives [`ProvenanceEvent`] variants for the execution lifecycle —
/// `ExecutionStarted`, `StageStarted`, `StageCompleted`, `ExecutionCompleted`,
/// `ExecutionFailed`, `ExecutionCancelled`, `VulnerabilityFound` — and projects
/// them into graph operations via [`project_event`].
///
/// Non-lifecycle variants are explicitly ignored with a warning rather than
/// a wildcard arm, so the compiler forces a conscious decision when new
/// variants are added to [`ProvenanceEvent`].
///
/// Owned by [`BuildProcessorSupervisor`]. The supervisor forwards lifecycle
/// events here via `send_message` — it does not call `project_event` directly.
pub struct BuildActor;

pub struct BuildActorState {
    /// Graph controller for Neo4j writes.
    /// Owned by the supervisor's actor tree — this is a reference, not
    /// the authoritative handle.
    pub graph_controller: GraphController,
}

pub struct BuildActorArgs {
    pub graph_controller: GraphController,
}

#[async_trait]
impl Actor for BuildActor {
    type Msg = ProvenanceEvent;
    type State = BuildActorState;
    type Arguments = BuildActorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: BuildActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        Ok(BuildActorState {
            graph_controller: args.graph_controller,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match &message {
            // ── Execution lifecycle ────────────────────────────────────────────
            // All of these carry build_id on the variant directly.
            // project_event destructures them and writes the appropriate
            // graph operations via the graph controller.
            ProvenanceEvent::ExecutionStarted { .. }
            | ProvenanceEvent::StageStarted { .. }
            | ProvenanceEvent::StageCompleted { .. }
            | ProvenanceEvent::ExecutionCompleted { .. }
            | ProvenanceEvent::ExecutionFailed { .. }
            | ProvenanceEvent::ExecutionCancelled { .. }
            | ProvenanceEvent::VulnerabilityFound { .. } => {
                if let Err(e) = project_event(&message, &state.graph_controller) {
                    error!(error = %e, "graph projection failed");
                }
            }

            // ── Non-lifecycle variants ─────────────────────────────────────────
            // These should never reach this actor — the supervisor routes them
            // to the linker. Warn rather than panic so a misconfigured dispatch
            // doesn't take down the actor tree.
            other => {
                warn!(
                    variant = ?std::mem::discriminant(other),
                    "BuildActor received non-lifecycle event — check supervisor dispatch routing"
                );
            }
        }

        Ok(())
    }
}
