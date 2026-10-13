//! Wraps WorkloadIssuanceHandler in its own actor, like IssuanceActor does for the unsealed route: the dispatch is
//! a plain synchronous function and this is where the async work happens and the reply goes back to Cassini,
//! tagged with the request's correlation id.
//!
//! The handler is kept in the actor's state because it owns the open sessions, which are mutable and belong to
//! exactly one place.

use crate::supervisor::BROKER_CLIENT_NAME;
use crate::workload_issuance::WorkloadIssuanceHandler;
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC;
use credentials_agent_common::workload_issuance::WorkloadIssuanceRequest;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::time::Instant;
use tracing::error;

pub struct WorkloadIssuanceActor;

pub struct WorkloadIssuanceActorArgs {
    pub handler: WorkloadIssuanceHandler,
}

pub struct WorkloadIssuanceActorState {
    handler: WorkloadIssuanceHandler,
}

#[async_trait]
impl Actor for WorkloadIssuanceActor {
    type Msg = Envelope<WorkloadIssuanceRequest>;
    type State = WorkloadIssuanceActorState;
    type Arguments = WorkloadIssuanceActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: WorkloadIssuanceActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(WorkloadIssuanceActorState {
            handler: args.handler,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let correlation_id = message.correlation_id.clone();
        let reply = state.handler.handle(message.payload, Instant::now()).await;

        let envelope = Envelope {
            correlation_id,
            payload: reply,
        };
        let payload_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                error!("Failed to serialize workload issuance reply: {e}");
                return Ok(());
            }
        };

        match where_is(BROKER_CLIENT_NAME.to_string()) {
            Some(client) => {
                if let Err(e) = client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC.to_string(),
                    payload_bytes,
                    None,
                )) {
                    error!("Failed to publish workload issuance reply: {e}");
                }
            }
            None => {
                error!(
                    "Broker client not found while attempting to publish workload issuance reply"
                );
            }
        }
        Ok(())
    }
}
