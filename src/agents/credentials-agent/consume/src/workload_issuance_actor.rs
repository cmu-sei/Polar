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
//! The handler is kept in the actor's state because it owns the open sessions and the key their offers are signed
//! with, which are mutable and belong to exactly one place. The actor renews that key's certificate: when it starts,
//! at half the certificate's life, and shortly after a try that failed.

use crate::supervisor::BROKER_CLIENT_NAME;
use crate::workload_issuance::{SIGNER_RETRY, WorkloadIssuanceHandler};
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC;
use credentials_agent_common::workload_issuance::WorkloadIssuanceRequest;
use polar::request_reply::Envelope;
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::time::Instant;
use tracing::{error, info, warn};

pub struct WorkloadIssuanceActor;

pub enum WorkloadIssuanceMsg {
    /// A workload's request, from Cassini.
    Request(Envelope<WorkloadIssuanceRequest>),
    /// Get a new signing certificate for session offers.
    RenewSigner,
}

pub struct WorkloadIssuanceActorArgs {
    pub handler: WorkloadIssuanceHandler,
}

pub struct WorkloadIssuanceActorState {
    handler: WorkloadIssuanceHandler,
}

#[async_trait]
impl Actor for WorkloadIssuanceActor {
    type Msg = WorkloadIssuanceMsg;
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

    /// The first signing certificate is asked for before any request is taken, so the first offer is not refused
    /// for want of one that was on its way.
    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        renew_signer(&myself, state).await;
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let message = match message {
            WorkloadIssuanceMsg::Request(envelope) => envelope,
            WorkloadIssuanceMsg::RenewSigner => {
                renew_signer(&myself, state).await;
                return Ok(());
            }
        };
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

/// Renews the signing certificate, and says when to do it next.
async fn renew_signer(
    myself: &ActorRef<WorkloadIssuanceMsg>,
    state: &mut WorkloadIssuanceActorState,
) {
    let next = match state.handler.renew_signer().await {
        Ok(next) => {
            info!("session offers are signed with a new signing certificate; renewing in {next:?}");
            next
        }
        Err(e) => {
            warn!("{e}; trying again in {SIGNER_RETRY:?}");
            SIGNER_RETRY
        }
    };
    myself.send_after(next, || WorkloadIssuanceMsg::RenewSigner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issuance::tests::{handler_for, spawn_test_ledger};
    use crate::workload_issuance::tests::OWN_IDENTITY;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn a_signing_certificate_that_could_not_be_had_is_asked_for_again() {
        let cert_issuer = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .mount(&cert_issuer)
            .await;
        let handler = WorkloadIssuanceHandler::new(
            handler_for(&cert_issuer, spawn_test_ledger().await).await,
            OWN_IDENTITY.to_string(),
        );
        let (actor, _) = Actor::spawn(
            None,
            WorkloadIssuanceActor,
            WorkloadIssuanceActorArgs { handler },
        )
        .await
        .unwrap();

        // Once when it starts, and again a retry interval later.
        tokio::time::sleep(SIGNER_RETRY + std::time::Duration::from_secs(1)).await;
        let asked = cert_issuer.received_requests().await.unwrap().len();
        assert!(asked >= 2, "asked {asked} times");
        actor.stop_and_wait(None, None).await.unwrap();
    }
}
