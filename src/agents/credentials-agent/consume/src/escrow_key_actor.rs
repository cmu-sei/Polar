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

//! Wraps EscrowKeyHandler in its own actor, mirroring
//! IdentityNormalizationActor's exact shape. EscrowKeyHandler::handle
//! is itself synchronous (no cert-issuer call, no I/O -- just reading
//! an already-loaded, in-memory string), but this actor's own
//! handle() is still async, per the Actor trait's own requirement;
//! the synchronous handler call inside it needs no .await.

use crate::escrow_key::EscrowKeyHandler;
use crate::supervisor::BROKER_CLIENT_NAME;
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC;
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::error;

pub struct EscrowKeyActor;

pub struct EscrowKeyActorArgs {
    pub handler: EscrowKeyHandler,
}

pub struct EscrowKeyActorState {
    handler: EscrowKeyHandler,
}

#[async_trait]
impl Actor for EscrowKeyActor {
    type Msg = Envelope<EscrowKeyRequest>;
    type State = EscrowKeyActorState;
    type Arguments = EscrowKeyActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: EscrowKeyActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(EscrowKeyActorState {
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
        let reply = state.handler.handle(message.payload);

        let envelope = Envelope {
            correlation_id,
            payload: reply,
        };

        let payload_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                error!("Failed to serialize escrow-key reply: {e}");
                return Ok(());
            }
        };

        match where_is(BROKER_CLIENT_NAME.to_string()) {
            Some(client) => {
                if let Err(e) = client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC.to_string(),
                    payload_bytes,
                    None,
                )) {
                    error!("Failed to publish escrow-key reply: {e}");
                }
            }
            None => {
                error!("Broker client not found while attempting to publish escrow-key reply");
            }
        }

        Ok(())
    }
}
