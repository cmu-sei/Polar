//! The operator as a Cassini client: an actor that owns the broker connection and
//! runs the controller beside it.
//!
//! ```text
//! OperatorSupervisor
//!   ├── TcpClient        (Cassini; lifecycle events go out through it)
//!   └── controller task  (kube-rs; keeps the agents alive)
//! ```
//!
//! Keeping agents alive must not depend on the broker, so the controller starts at
//! once, not on registration. Each registration (the first, and every reconnect)
//! forgets which agents were declared, so each is declared again on its next pass
//! and the graph catches up with whatever was missed while the broker was away.
//! The client or the controller ending ends the operator, non-zero.

use crate::controller::{self, Declared};
use crate::events::Publisher;
use crate::lifecycle::Policy;
use cassini_types::ClientEvent;
use polar::cassini::{CassiniClient, TcpClient};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use std::sync::Arc;
use tracing::{error, info, warn};

pub struct OperatorSupervisor;

pub struct OperatorArgs {
    pub kube: kube::Client,
    pub policy: Policy,
}

pub struct OperatorState {
    declared: Declared,
}

pub enum OperatorMessage {
    Client(ClientEvent),
    ControllerStopped(String),
}

#[ractor::async_trait]
impl Actor for OperatorSupervisor {
    type Msg = OperatorMessage;
    type State = OperatorState;
    type Arguments = OperatorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: OperatorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let client: Arc<dyn CassiniClient> = Arc::new(
            TcpClient::spawn(crate::OPERATOR_NAME, myself.clone(), |event| {
                Some(OperatorMessage::Client(event))
            })
            .await?,
        );

        let declared = Declared::default();
        let publisher = Publisher::new(client);
        let task_declared = declared.clone();
        tokio::spawn(async move {
            let reason =
                match controller::run(args.kube, args.policy, publisher, task_declared).await {
                    Ok(()) => "the controller stopped".to_string(),
                    Err(e) => format!("the controller failed: {e:#}"),
                };
            let _ = myself.cast(OperatorMessage::ControllerStopped(reason));
        });

        Ok(OperatorState { declared })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            OperatorMessage::Client(ClientEvent::Registered { .. }) => {
                info!("registered with Cassini; every agent will be declared again");
                state.declared.forget();
            }
            OperatorMessage::Client(ClientEvent::TransportError { reason }) => {
                warn!("Cassini transport error (awaiting reconnect): {reason}");
            }
            OperatorMessage::Client(_) => {}
            OperatorMessage::ControllerStopped(reason) => {
                error!("{reason}");
                myself.stop(Some(reason));
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let reason = format!("{:?} stopped: {reason:?}", cell.get_name());
                error!("{reason}");
                myself.stop(Some(reason));
            }
            SupervisionEvent::ActorFailed(cell, e) => {
                let reason = format!("{:?} failed: {e}", cell.get_name());
                error!("{reason}");
                myself.stop(Some(reason));
            }
            _ => {}
        }
        Ok(())
    }
}
