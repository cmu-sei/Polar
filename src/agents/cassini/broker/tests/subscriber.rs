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

use cassini_types::{BrokerMessage, DisconnectReason, ShutdownPhase};
use ractor::{Actor, ActorProcessingErr, ActorRef};

// Minimal no-op actor that accepts BrokerMessage and does nothing.
// Used as a stand-in for Broker and SessionManager in tests that
// don't need real broker behavior — avoids TLS env var requirements.
struct MockActor;

#[ractor::async_trait]
impl Actor for MockActor {
    type Msg = BrokerMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_broker::subscriber::{SubscriberManager, SubscriberManagerArgs};

    #[tokio::test]
    async fn test_subscriber_manager_spawn() -> Result<(), ActorProcessingErr> {
        // Create a mock session manager to pass as argument
        let args = SubscriberManagerArgs {
            session_mgr: create_mock_session_manager().await,
        };

        // Just make sure we can spawn the SubscriberManager
        let _actor_ref = Actor::spawn(
            Some("test-subscriber-manager".to_string()),
            SubscriberManager,
            args,
        )
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_subscriber_manager_initiate_shutdown_phase() -> Result<(), ActorProcessingErr> {
        // Create a mock session manager to pass as argument
        let args = SubscriberManagerArgs {
            session_mgr: create_mock_session_manager().await,
        };

        // Spawn SubscriberManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-subscriber-manager-2".to_string()),
            SubscriberManager,
            args,
        )
        .await?;

        // Send initiate shutdown phase message
        let msg = BrokerMessage::InitiateShutdownPhase {
            phase: ShutdownPhase::TerminateSubscribers,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_subscriber_manager_registration_request() -> Result<(), ActorProcessingErr> {
        // Create a mock session manager to pass as argument
        let args = SubscriberManagerArgs {
            session_mgr: create_mock_session_manager().await,
        };

        // Spawn SubscriberManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-subscriber-manager-3".to_string()),
            SubscriberManager,
            args,
        )
        .await?;

        // Send a registration request
        let msg = BrokerMessage::RegistrationRequest {
            registration_id: Some("test-client".to_string()),
            client_id: "test-client".to_string(),
            trace_ctx: None,
            peer_identity: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_subscriber_manager_create_subscriber() -> Result<(), ActorProcessingErr> {
        // Create a mock session manager to pass as argument
        let args = SubscriberManagerArgs {
            session_mgr: create_mock_session_manager().await,
        };

        // Spawn SubscriberManager
        let (_actor_ref, _handle) = Actor::spawn(
            Some("test-subscriber-manager-4".to_string()),
            SubscriberManager,
            args,
        )
        .await?;

        // This test is more about ensuring the message handling works without panicking
        // The actual subscriber creation would require a complex setup with topic manager

        Ok(())
    }

    #[tokio::test]
    async fn test_subscriber_manager_unsubscribe_request() -> Result<(), ActorProcessingErr> {
        // Create a mock session manager to pass as argument
        let args = SubscriberManagerArgs {
            session_mgr: create_mock_session_manager().await,
        };

        // Spawn SubscriberManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-subscriber-manager-5".to_string()),
            SubscriberManager,
            args,
        )
        .await?;

        // Send an unsubscribe request
        let msg = BrokerMessage::UnsubscribeRequest {
            registration_id: "test-client".to_string(),
            topic: "test-topic".to_string(),
            trace_ctx: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_subscriber_manager_disconnect_request() -> Result<(), ActorProcessingErr> {
        // Create a mock session manager to pass as argument
        let args = SubscriberManagerArgs {
            session_mgr: create_mock_session_manager().await,
        };

        // Spawn SubscriberManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-subscriber-manager-6".to_string()),
            SubscriberManager,
            args,
        )
        .await?;

        // Send a disconnect request
        let msg = BrokerMessage::DisconnectRequest {
            reason: DisconnectReason::RemoteClosed,
            client_id: "test-client".to_string(),
            registration_id: Some("test-client".to_string()),
            trace_ctx: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_subscriber_manager_timeout_message() -> Result<(), ActorProcessingErr> {
        // Create a mock session manager to pass as argument
        let args = SubscriberManagerArgs {
            session_mgr: create_mock_session_manager().await,
        };

        // Spawn SubscriberManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-subscriber-manager-7".to_string()),
            SubscriberManager,
            args,
        )
        .await?;

        // Send a timeout message
        let msg = BrokerMessage::TimeoutMessage {
            client_id: "test-client".to_string(),
            registration_id: "test-client".to_string(),
            error: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    async fn create_mock_session_manager() -> ActorRef<BrokerMessage> {
        let (actor_ref, _handle) = Actor::spawn(
            None, // anonymous — no registry collision
            MockActor,
            (),
        )
        .await
        .unwrap();
        actor_ref
    }
}
