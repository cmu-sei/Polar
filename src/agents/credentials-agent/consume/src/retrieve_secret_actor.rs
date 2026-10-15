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

//! Wraps RetrieveSecretHandler in its own actor, mirroring
//! StoreSecretActor's own shape exactly -- for the identical reason:
//! this handler genuinely needs published_by (broker-verified,
//! carried alongside the envelope by
//! ConsumerSupervisor::dispatch_retrieve_secret_request, mirroring
//! dispatch_store_secret_request's own doc comment for why this
//! can't go through the shared Supervisor trait's ordinary
//! deserialize_and_dispatch).

use crate::retrieve_secret::RetrieveSecretHandler;
use crate::supervisor::BROKER_CLIENT_NAME;
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC;
use credentials_agent_common::retrieve_secret::RetrieveSecretRequest;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::error;

pub struct RetrieveSecretActor;

pub struct RetrieveSecretActorArgs {
    pub handler: RetrieveSecretHandler,
}

pub struct RetrieveSecretActorState {
    handler: RetrieveSecretHandler,
}

/// The envelope, bundled with the broker-verified published_by this
/// dispatch path exists specifically to carry alongside it.
pub struct RetrieveSecretDispatch {
    pub envelope: Envelope<RetrieveSecretRequest>,
    pub published_by: Option<String>,
}

#[async_trait]
impl Actor for RetrieveSecretActor {
    type Msg = RetrieveSecretDispatch;
    type State = RetrieveSecretActorState;
    type Arguments = RetrieveSecretActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: RetrieveSecretActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(RetrieveSecretActorState {
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
                error!("Failed to serialize retrieve-secret reply: {e}");
                return Ok(());
            }
        };

        match where_is(BROKER_CLIENT_NAME.to_string()) {
            Some(client) => {
                if let Err(e) = client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC.to_string(),
                    payload_bytes,
                    None,
                )) {
                    error!("Failed to publish retrieve-secret reply: {e}");
                }
            }
            None => {
                error!("Broker client not found while attempting to publish retrieve-secret reply");
            }
        }

        Ok(())
    }
}
