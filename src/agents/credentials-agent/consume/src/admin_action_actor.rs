//! Wraps AdminActionHandler in an actor, mirroring StoreSecretActor. There is
//! deliberately no published_by here: authorship comes from the signature.

use crate::admin_action::AdminActionHandler;
use crate::supervisor::BROKER_CLIENT_NAME;
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC;
use credentials_agent_common::admin_action::SignedAdminAction;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::error;

pub struct AdminActionActor;

pub struct AdminActionActorArgs {
    pub handler: AdminActionHandler,
}

pub struct AdminActionActorState {
    handler: AdminActionHandler,
}

#[async_trait]
impl Actor for AdminActionActor {
    type Msg = Envelope<SignedAdminAction>;
    type State = AdminActionActorState;
    type Arguments = AdminActionActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: AdminActionActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(AdminActionActorState {
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
        let reply = state.handler.handle(message.payload).await;

        let envelope = Envelope {
            correlation_id,
            payload: reply,
        };

        let payload_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                error!("Failed to serialize admin-action reply: {e}");
                return Ok(());
            }
        };

        match where_is(BROKER_CLIENT_NAME.to_string()) {
            Some(client) => {
                if let Err(e) = client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC.to_string(),
                    payload_bytes,
                    None,
                )) {
                    error!("Failed to publish admin-action reply: {e}");
                }
            }
            None => {
                error!("Broker client not found while attempting to publish admin-action reply");
            }
        }

        Ok(())
    }
}
