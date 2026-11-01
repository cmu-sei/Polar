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

//! An agent that refuses its own configuration at start, and stays up (docs/architecture/soft-state.md, "As built:
//! pulled health"). A problem a restart does not help (an address that is a placeholder) used to end the agent, and
//! the pod's rule restarted every container, in a loop. Instead the agent starts its infrastructure and nothing of its
//! domain, holds the problem ([`crate::HealthHandle::hold`]), answers health queries with it, rejuvenates as any
//! agent does, and ends only by its health actor's verdict or a signal.
//!
//! In `PolarAgent::start`: `return polar_agent_builder::held::stay_up(configured, NAME, HoldReason::Configuration,
//! why).await;`

use crate::{AgentBuilder, Configured, InfrastructureEvent, InfrastructureSupervisorMessage};
use cassini_types::ClientEvent;
use polar_health::HoldReason;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};

/// Runs the agent `service_name` with its infrastructure only, holding `reason` with `message`, until it is told to end.
/// Returns an error then, as every agent's `start` does: an agent that ends has failed.
pub async fn stay_up(
    configured: Configured,
    service_name: &str,
    reason: HoldReason,
    message: impl Into<String>,
) -> Result<(), crate::StartError> {
    let message = message.into();
    tracing::error!(
        "{service_name} cannot start its work, and stays up to say so (a restart would not help): {message}"
    );
    let (_supervisor, handle) = Actor::spawn(
        Some(format!("{service_name}.held")),
        HeldSupervisor,
        HeldArgs {
            configured,
            service_name: service_name.to_string(),
            reason,
            message,
        },
    )
    .await
    .map_err(|e| format!("the held supervisor did not start: {e}"))?;
    handle
        .await
        .map_err(|e| format!("the held supervisor's task failed: {e}"))?;
    Err(format!("{service_name} held a standing problem and has ended").into())
}

pub(crate) struct HeldArgs {
    configured: Configured,
    service_name: String,
    reason: HoldReason,
    message: String,
}

pub(crate) enum HeldMessage {
    Client(ClientEvent),
    PrepareShutdown,
}

pub(crate) struct HeldState {
    healthcheck: crate::HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
}

/// The domain supervisor of an agent that holds a standing problem: it has no domain, so it only tells the health
/// actor about Cassini, and lets the infrastructure go at once when told to end.
pub(crate) struct HeldSupervisor;

#[async_trait]
impl Actor for HeldSupervisor {
    type Msg = HeldMessage;
    type State = HeldState;
    type Arguments = HeldArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: HeldArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let resources = AgentBuilder::new(myself.clone(), args.service_name)
            .with_lifecycle_handler(|event| Some(HeldMessage::Client(event)))
            .into_configured(args.configured)
            .build()
            .await?;
        resources.healthcheck.hold(args.reason, args.message);
        resources
            .infrastructure_events
            .subscribe(myself, |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(HeldMessage::PrepareShutdown),
            });
        Ok(HeldState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        use crate::HealthClient;
        match message {
            HeldMessage::Client(ClientEvent::Registered { .. }) => {
                let _ = state.healthcheck.cassini_connected();
            }
            HeldMessage::Client(ClientEvent::TransportError { .. }) => {
                let _ = state.healthcheck.cassini_disconnected();
            }
            HeldMessage::Client(_) => {}
            HeldMessage::PrepareShutdown => {
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // The infrastructure is the one child; its ending is the end of the shutdown handshake, or a failure.
        if let SupervisionEvent::ActorTerminated(actor, ..)
        | SupervisionEvent::ActorFailed(actor, _) = event
            && actor.get_id() == state.infrastructure.get_id()
        {
            myself.stop(Some("the infrastructure has ended".to_string()));
        }
        Ok(())
    }
}
