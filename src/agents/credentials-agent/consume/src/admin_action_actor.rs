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
