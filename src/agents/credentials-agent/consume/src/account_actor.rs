//! Wraps AccountHandler in an actor, mirroring AdminActionActor. As there,
//! there is no published_by: the claimant proves itself in the request.

use crate::account::AccountHandler;
use crate::supervisor::BROKER_CLIENT_NAME;
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_ACCOUNT_REPLIES_TOPIC;
use credentials_agent_common::account_claim::AccountRequest;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::error;

pub struct AccountActor;

pub struct AccountActorArgs {
    pub handler: AccountHandler,
}

pub struct AccountActorState {
    handler: AccountHandler,
}

#[async_trait]
impl Actor for AccountActor {
    type Msg = Envelope<AccountRequest>;
    type State = AccountActorState;
    type Arguments = AccountActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: AccountActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(AccountActorState {
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
                error!("Failed to serialize account reply: {e}");
                return Ok(());
            }
        };

        match where_is(BROKER_CLIENT_NAME.to_string()) {
            Some(client) => {
                if let Err(e) = client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_ACCOUNT_REPLIES_TOPIC.to_string(),
                    payload_bytes,
                    None,
                )) {
                    error!("Failed to publish account reply: {e}");
                }
            }
            None => {
                error!("Broker client not found while attempting to publish account reply");
            }
        }

        Ok(())
    }
}
