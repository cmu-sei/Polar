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

//! Connects to Cassini using the admin's own Polar client cert (mTLS,
//! via TCPClientConfig::new() -- same env-var convention every other
//! Cassini-connected agent already uses), publishes exactly one
//! CredentialsEvent, and signals completion. A one-shot tool, not a
//! long-running agent -- no supervisor, no healthcheck, no graph
//! controller.

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use credentials_agent_common::events::CredentialsEvent;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
use std::sync::Arc;
use tokio::sync::oneshot;

pub struct PublishSupervisor;

pub struct PublishSupervisorArgs {
    pub event: CredentialsEvent,
    pub topic: String,
    pub done: oneshot::Sender<Result<(), String>>,
}

pub struct PublishSupervisorState {
    client: ActorRef<TcpClientMessage>,
    event: Option<CredentialsEvent>,
    topic: String,
    done: Option<oneshot::Sender<Result<(), String>>>,
}

pub enum PublishMessage {
    ClientEvent { event: ClientEvent },
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
            event: Some(args.event),
            topic: args.topic,
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
        if let ClientEvent::Registered { .. } = event
            && let Some(event) = state.event.take()
        {
            let result = match rkyv::to_bytes::<rkyv::rancor::Error>(&event) {
                Ok(bytes) => state
                    .client
                    .send_message(TcpClientMessage::publish(
                        state.topic.clone(),
                        bytes.to_vec(),
                        None,
                    ))
                    .map_err(|e| e.to_string()),
                Err(e) => Err(format!("serialization failed: {e}")),
            };
            if let Some(done) = state.done.take() {
                let _ = done.send(result);
            }
        }
        Ok(())
    }
}

/// Connects, publishes `event` to `topic`, and waits for confirmation
/// that the publish was actually enqueued -- not confirmation of
/// delivery, which `TcpClientMessage::Publish` doesn't provide at
/// all. Callers should allow a brief additional delay after this
/// returns before the process exits, so the underlying TCP client has
/// a real chance to flush the bytes over the wire.
pub async fn publish_event(event: CredentialsEvent, topic: String) -> Result<(), String> {
    let (done_tx, done_rx) = oneshot::channel();

    let (_supervisor, _) = Actor::spawn(
        None,
        PublishSupervisor,
        PublishSupervisorArgs {
            event,
            topic,
            done: done_tx,
        },
    )
    .await
    .map_err(|e| format!("failed to spawn publish supervisor: {e}"))?;

    match tokio::time::timeout(std::time::Duration::from_secs(30), done_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("publish supervisor exited without reporting a result".to_string()),
        Err(_) => Err("timed out waiting to register with Cassini".to_string()),
    }
}
