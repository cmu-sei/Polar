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

//! HealthHandle -- wraps a real, live polar_health::HealthCheckActor's
//! own ActorRef<HealthCheckMessage>, exposing its every, real, fire-
//! and-forget variant as a direct, ordinary method. Every real
//! HealthCheckMessage variant is genuinely fire-and-forget (a plain
//! cast, never a call) -- see that enum's own definition -- so, unlike
//! CryptoHandle, none of these methods need to be async at all.

use polar_health::HealthCheckMessage;
use ractor::ActorRef;

pub trait HealthClient: Send + Sync {
    fn cassini_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn cassini_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_available(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_op_failed(
        &self,
        reason: String,
    ) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
}

/// The real, live implementation of HealthClient -- a thin wrapper
/// around a real, already-spawned HealthCheckActor's own ActorRef.
#[derive(Clone)]
pub struct HealthHandle {
    actor: ActorRef<HealthCheckMessage>,
}

impl HealthHandle {
    pub fn new(actor: ActorRef<HealthCheckMessage>) -> Self {
        Self { actor }
    }

    /// The same, real, deliberate escape hatch every other handle in
    /// this crate offers -- see CryptoHandle::as_actor_ref's own doc
    /// comment for the full, shared reasoning.
    pub fn as_actor_ref(&self) -> ActorRef<HealthCheckMessage> {
        self.actor.clone()
    }
}

impl HealthClient for HealthHandle {
    fn cassini_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::CassiniConnected)
    }

    fn cassini_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::CassiniDisconnected)
    }

    fn graph_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphConnected)
    }

    fn graph_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphDisconnected)
    }

    fn graph_available(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphAvailable)
    }

    fn graph_op_failed(
        &self,
        reason: String,
    ) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphOpFailed(reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ractor::{Actor, ActorProcessingErr, async_trait};
    use std::sync::{Arc, Mutex};

    /// A minimal, real stand-in for HealthCheckActor -- genuinely
    /// receives HealthCheckMessage, just records what it got, since
    /// this test's own, real purpose is proving HealthHandle's own
    /// methods genuinely, correctly send the right, real variant --
    /// not re-testing HealthCheckActor's own, separate, real,
    /// internal logic.
    struct FakeHealthCheckActor {
        received: Arc<Mutex<Vec<HealthCheckMessage>>>,
    }

    #[async_trait]
    impl Actor for FakeHealthCheckActor {
        type Msg = HealthCheckMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    #[tokio::test]
    async fn every_real_method_sends_its_own_matching_real_variant() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (actor, _) = Actor::spawn(
            None,
            FakeHealthCheckActor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let handle = HealthHandle::new(actor);

        handle.cassini_connected().unwrap();
        handle.cassini_disconnected().unwrap();
        handle.graph_connected().unwrap();
        handle.graph_disconnected().unwrap();
        handle.graph_available().unwrap();
        handle
            .graph_op_failed("a real, genuine failure reason".to_string())
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let messages = received.lock().unwrap();
        assert_eq!(messages.len(), 6);
        assert!(matches!(messages[0], HealthCheckMessage::CassiniConnected));
        assert!(matches!(
            messages[1],
            HealthCheckMessage::CassiniDisconnected
        ));
        assert!(matches!(messages[2], HealthCheckMessage::GraphConnected));
        assert!(matches!(messages[3], HealthCheckMessage::GraphDisconnected));
        assert!(matches!(messages[4], HealthCheckMessage::GraphAvailable));
        match &messages[5] {
            HealthCheckMessage::GraphOpFailed(reason) => {
                assert_eq!(reason, "a real, genuine failure reason");
            }
            other => panic!("expected GraphOpFailed, got {other:?}"),
        }
    }
}
