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

//! Wraps IssuanceHandler in its own actor -- deserialize_and_dispatch
//! is a plain, synchronous function (matching every agent's Supervisor
//! trait signature) and can't itself await the real async work
//! (introspection, cert-issuer's two HTTP calls, ledger RPCs) an
//! issuance request needs. A dedicated actor's handle() method is
//! async by construction (ractor's Actor trait), so this is where
//! that work actually happens -- one request processed, then its
//! reply published back to Cassini, tagged with the same
//! correlation_id, per cassini-request-reply-protocol.md.

use crate::issuance::IssuanceHandler;
use crate::supervisor::BROKER_CLIENT_NAME;
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_ISSUANCE_REPLIES_TOPIC;
use credentials_agent_common::issuance::IssuanceRequest;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::error;

pub struct IssuanceActor;

pub struct IssuanceActorArgs {
    pub handler: IssuanceHandler,
}

pub struct IssuanceActorState {
    handler: IssuanceHandler,
}

#[async_trait]
impl Actor for IssuanceActor {
    type Msg = Envelope<IssuanceRequest>;
    type State = IssuanceActorState;
    type Arguments = IssuanceActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: IssuanceActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(IssuanceActorState {
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
                error!("Failed to serialize issuance reply: {e}");
                return Ok(());
            }
        };

        match where_is(BROKER_CLIENT_NAME.to_string()) {
            Some(client) => {
                if let Err(e) = client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_ISSUANCE_REPLIES_TOPIC.to_string(),
                    payload_bytes,
                    None,
                )) {
                    error!("Failed to publish issuance reply: {e}");
                }
            }
            None => {
                error!("Broker client not found while attempting to publish issuance reply");
            }
        }

        Ok(())
    }
}
