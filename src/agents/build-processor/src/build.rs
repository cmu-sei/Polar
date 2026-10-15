/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

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
            | ProvenanceEvent::ExecutionCancelled { .. } => {
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
