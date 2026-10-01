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

//! Connects to Cassini with the admin's own Polar client cert (mTLS, via
//! TCPClientConfig::new() -- the same env-var convention every other
//! Cassini-connected agent uses), publishes one signed admin action on the
//! requests topic, and waits for the credentials-agent's correlated reply.
//! A one-shot tool, not a long-running agent -- no supervisor, no
//! healthcheck, no graph controller.
//!
//! The reply is the only confirmation there is: the consumer is the gate, so
//! "published" means nothing until it says Recorded or Refused.

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::{ClientEvent, WireTraceCtx};
use credentials_agent_common::admin_action::{AdminActionReply, SignedAdminAction};
use credentials_agent_common::{
    CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC, CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC,
};
use polar::request_reply::Envelope;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

pub struct PublishSupervisor;

pub struct PublishSupervisorArgs {
    pub correlation_id: String,
    pub request_bytes: Vec<u8>,
    pub done: oneshot::Sender<Result<AdminActionReply, String>>,
}

pub struct PublishSupervisorState {
    client: ActorRef<TcpClientMessage>,
    correlation_id: String,
    request_bytes: Option<Vec<u8>>,
    done: Option<oneshot::Sender<Result<AdminActionReply, String>>>,
}

pub enum PublishMessage {
    ClientEvent { event: ClientEvent },
}

impl PublishSupervisorState {
    fn finish(&mut self, result: Result<AdminActionReply, String>) {
        if let Some(done) = self.done.take() {
            let _ = done.send(result);
        }
    }
}

#[async_trait]
impl Actor for PublishSupervisor {
    type Msg = PublishMessage;
    type State = PublishSupervisorState;
    type Arguments = PublishSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: PublishSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let events_output = Arc::new(OutputPort::default());
        events_output.subscribe(myself.clone(), |event| {
            Some(PublishMessage::ClientEvent { event })
        });

        let client_config = TCPClientConfig::new()?;

        let (client, _) = Actor::spawn_linked(
            Some("POLAR_ADMIN_GRANT_CLIENT".to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: client_config,
                registration_id: None,
                delivery: cassini_client::DeliveryMode::Broadcast(events_output),
            },
            myself.clone().into(),
        )
        .await?;

        Ok(PublishSupervisorState {
            client,
            correlation_id: args.correlation_id,
            request_bytes: Some(args.request_bytes),
            done: Some(args.done),
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let PublishMessage::ClientEvent { event } = message;
        match event {
            // Subscribe to replies first, then publish the request, so the
            // reply can't be produced before this client is listening.
            ClientEvent::Registered { .. } => {
                let Some(bytes) = state.request_bytes.take() else {
                    return Ok(());
                };
                if let Err(e) = state.client.send_message(TcpClientMessage::Subscribe {
                    topic: CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC.to_string(),
                    trace_ctx: WireTraceCtx::from_current_span(),
                }) {
                    state.finish(Err(format!("subscribe failed: {e}")));
                    return Ok(());
                }
                if let Err(e) = state.client.send_message(TcpClientMessage::publish(
                    CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC.to_string(),
                    bytes,
                    None,
                )) {
                    state.finish(Err(format!("publish failed: {e}")));
                }
            }
            ClientEvent::MessagePublished { topic, payload, .. }
                if topic == CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC =>
            {
                // Replies for other admins' requests share this topic;
                // only the one carrying our correlation id is ours.
                if let Ok(envelope) =
                    rkyv::from_bytes::<Envelope<AdminActionReply>, rkyv::rancor::Error>(&payload)
                    && envelope.correlation_id == state.correlation_id
                {
                    state.finish(Ok(envelope.payload));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Connects, publishes the signed action, and waits for the credentials
/// agent's reply to exactly this request.
pub async fn request_admin_action(
    signed: SignedAdminAction,
    timeout: Duration,
) -> Result<AdminActionReply, String> {
    let correlation_id = uuid::Uuid::new_v4().to_string();
    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: signed,
    };
    let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
        .map_err(|e| format!("serialization failed: {e}"))?
        .to_vec();

    let (done_tx, done_rx) = oneshot::channel();

    let (supervisor, handle) = Actor::spawn(
        None,
        PublishSupervisor,
        PublishSupervisorArgs {
            correlation_id,
            request_bytes,
            done: done_tx,
        },
    )
    .await
    .map_err(|e| format!("failed to spawn publish supervisor: {e}"))?;

    let outcome = match tokio::time::timeout(timeout, done_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("publish supervisor exited without reporting a result".to_string()),
        Err(_) => Err(format!(
            "no reply from the credentials agent within {}s",
            timeout.as_secs()
        )),
    };

    supervisor.stop(None);
    let _ = handle.await;
    outcome
}
