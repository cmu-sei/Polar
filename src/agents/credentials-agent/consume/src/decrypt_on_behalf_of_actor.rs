/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! Wraps DecryptOnBehalfOfHandler in its own actor, mirroring
//! RetrieveSecretActor's own shape exactly -- for the identical
//! reason: this handler genuinely needs published_by (broker-
//! verified, carried alongside the envelope by
//! ConsumerSupervisor::dispatch_decrypt_on_behalf_of_request,
//! mirroring dispatch_retrieve_secret_request's own doc comment for
//! why this can't go through the shared Supervisor trait's ordinary
//! deserialize_and_dispatch).

use crate::decrypt_on_behalf_of::DecryptOnBehalfOfHandler;
use crate::supervisor::BROKER_CLIENT_NAME;
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_DECRYPT_ON_BEHALF_OF_REPLIES_TOPIC;
use credentials_agent_common::decrypt_on_behalf_of::DecryptOnBehalfOfRequest;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::error;

pub struct DecryptOnBehalfOfActor;

pub struct DecryptOnBehalfOfActorArgs {
    pub handler: DecryptOnBehalfOfHandler,
}

pub struct DecryptOnBehalfOfActorState {
    handler: DecryptOnBehalfOfHandler,
}

/// The envelope, bundled with the broker-verified published_by this
/// dispatch path exists specifically to carry alongside it.
pub struct DecryptOnBehalfOfDispatch {
    pub envelope: Envelope<DecryptOnBehalfOfRequest>,
    pub published_by: Option<String>,
}

#[async_trait]
impl Actor for DecryptOnBehalfOfActor {
    type Msg = DecryptOnBehalfOfDispatch;
    type State = DecryptOnBehalfOfActorState;
    type Arguments = DecryptOnBehalfOfActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: DecryptOnBehalfOfActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(DecryptOnBehalfOfActorState {
            handler: args.handler,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let correlation_id = message.envelope.correlation_id.clone();
        let reply = state
            .handler
            .handle(message.envelope.payload, message.published_by)
            .await;

        let envelope = Envelope {
            correlation_id,
            payload: reply,
        };

        let payload_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                error!("Failed to serialize decrypt-on-behalf-of reply: {e}");
                return Ok(());
            }
        };

        match where_is(BROKER_CLIENT_NAME.to_string()) {
            Some(client) => {
                if let Err(e) = client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_DECRYPT_ON_BEHALF_OF_REPLIES_TOPIC.to_string(),
                    payload_bytes,
                    None,
                )) {
                    error!("Failed to publish decrypt-on-behalf-of reply: {e}");
                }
            }
            None => {
                error!(
                    "Broker client not found while attempting to publish decrypt-on-behalf-of reply"
                );
            }
        }

        Ok(())
    }
}
