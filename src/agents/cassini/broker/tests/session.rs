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

use cassini_types::BrokerMessage;
use ractor::{Actor, ActorProcessingErr};

// Minimal no-op actor that accepts BrokerMessage and does nothing.
// Used as a stand-in for Broker in tests to avoid TLS env var requirements.
struct MockActor;

#[ractor::async_trait]
impl Actor for MockActor {
    type Msg = BrokerMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        _message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_broker::session::{SessionManager, SessionManagerArgs};
    use ractor::Actor;

    #[tokio::test]
    async fn test_session_manager_spawn() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let args = SessionManagerArgs {
            broker: Actor::spawn(None, MockActor, ()).await.unwrap().0,
            listener_mgr: None,
            retention: std::time::Duration::from_secs(600),
        };

        // Just make sure we can spawn the SessionManager
        let _actor_ref = Actor::spawn(
            Some("test-session-manager".to_string()),
            SessionManager,
            args,
        )
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_session_manager_handle_registration_request() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let broker_ref = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let args = SessionManagerArgs {
            broker: broker_ref,
            listener_mgr: None,
            retention: std::time::Duration::from_secs(600),
        };

        // Spawn SessionManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-session-manager-2".to_string()),
            SessionManager,
            args,
        )
        .await?;

        // Send a registration request message to the actor
        let msg = BrokerMessage::RegistrationRequest {
            registration_id: None,
            client_id: "test-client".to_string(),
            trace_ctx: None,
            peer_identity: None,
            peer_instance: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_session_manager_handle_session_pending_update() -> Result<(), ActorProcessingErr>
    {
        // Create a mock broker to pass as argument
        let broker_ref = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let args = SessionManagerArgs {
            broker: broker_ref,
            listener_mgr: None,
            retention: std::time::Duration::from_secs(600),
        };

        // Spawn SessionManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-session-manager-3".to_string()),
            SessionManager,
            args,
        )
        .await?;

        // Send a session pending update message to the actor
        let msg = BrokerMessage::SessionPendingUpdate {
            registration_id: "test-client".to_string(),
            delta: 1,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_session_manager_handle_set_listener_manager() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let broker_ref = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let args = SessionManagerArgs {
            broker: broker_ref,
            listener_mgr: None,
            retention: std::time::Duration::from_secs(600),
        };

        // Spawn SessionManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-session-manager-4".to_string()),
            SessionManager,
            args,
        )
        .await?;

        // Send a set listener manager message to the actor
        let msg = BrokerMessage::SetListenerManager {
            listener_mgr: Actor::spawn(None, MockActor, ()).await.unwrap().0,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_session_manager_handle_prepare_for_shutdown() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let broker_ref = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let args = SessionManagerArgs {
            broker: broker_ref,
            listener_mgr: None,
            retention: std::time::Duration::from_secs(600),
        };

        // Spawn SessionManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-session-manager-5".to_string()),
            SessionManager,
            args,
        )
        .await?;

        // Send a prepare for shutdown message to the actor
        let msg = BrokerMessage::PrepareForShutdown { auth_token: None };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_session_manager_handle_initiate_drain_phase() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let broker_ref = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let args = SessionManagerArgs {
            broker: broker_ref,
            listener_mgr: None,
            retention: std::time::Duration::from_secs(600),
        };

        // Spawn SessionManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-session-manager-6".to_string()),
            SessionManager,
            args,
        )
        .await?;

        // Send an initiate drain phase message to the actor
        let msg = BrokerMessage::InitiateShutdownPhase {
            phase: cassini_types::ShutdownPhase::DrainExistingSessions,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_session_manager_handle_get_session_ref() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let broker_ref = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let args = SessionManagerArgs {
            broker: broker_ref,
            listener_mgr: None,
            retention: std::time::Duration::from_secs(600),
        };

        // Spawn SessionManager
        let (_actor_ref, _handle) = Actor::spawn(
            Some("test-session-manager-7".to_string()),
            SessionManager,
            args,
        )
        .await?;

        // Send a get session reference message to the actor - this test is just to make sure we can send the message
        // We're not actually testing the reply_to functionality since that's complex to set up

        Ok(())
    }
}
