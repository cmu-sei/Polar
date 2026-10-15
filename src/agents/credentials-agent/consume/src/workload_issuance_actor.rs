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
