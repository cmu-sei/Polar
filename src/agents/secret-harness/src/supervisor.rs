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

//! Supervisor for the harness: the builder gives it the health check, the Cassini client and the crypto
//! actor, and it runs the scenario for its role under them. Shutdown follows the other builder agents:
//! on `PrepareShutdown` the scenario stops and the infrastructure is released, whose end exits non-zero.

use crate::scenario;
use crate::settings::HarnessSettings;
use cassini_types::ClientEvent;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::time::Duration;
use tracing::{error, info, warn};

pub const SERVICE_NAME: &str = "secret-harness";

/// Nothing here has anything to finish before it stops.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

pub enum HarnessSupervisorMessage {
    ClientEvent { event: ClientEvent },
    PrepareShutdown,
}

pub struct HarnessSupervisor;

pub struct HarnessSupervisorState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    scenario: tokio::task::JoinHandle<()>,
}

#[async_trait]
impl Actor for HarnessSupervisor {
    type Msg = HarnessSupervisorMessage;
    type State = HarnessSupervisorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("{myself:?} starting");
        let settings = HarnessSettings::read(configured.config())?;
        let resources = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_encryption()
            .with_lifecycle_handler(|event| Some(HarnessSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(HarnessSupervisorMessage::PrepareShutdown)
                }
            });
        let crypto = resources.crypto.ok_or_else(|| {
            ActorProcessingErr::from(
                "with_encryption() was called but the builder returned no CryptoHandle",
            )
        })?;

        info!(
            "{SERVICE_NAME} playing {} for secret `{}`, first round in {:?}, then every {:?}",
            settings.role.name(),
            settings.secret_name,
            settings.start_delay,
            settings.period
        );
        let scenario =
            tokio::spawn(
                async move { scenario::run(&settings, &crypto, |line| info!("{line}")).await },
            );

        Ok(HarnessSupervisorState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            scenario,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            HarnessSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => {}
            },
            HarnessSupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received: stopping the scenario before infrastructure");
                state.scenario.abort();
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
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure shutdown complete ({reason:?}), exiting");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Child actor {actor:?} failed: {err}; exiting");
                std::process::exit(1);
            }
            _ => {}
        }
        Ok(())
    }
}
