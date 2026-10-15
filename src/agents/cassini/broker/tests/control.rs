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

use cassini_broker::control::{ControlManager, ControlManagerState};
use cassini_types::{BrokerMessage, ControlOp, ShutdownPhase};
use ractor::{Actor, ActorProcessingErr};
use tokio_util::sync::CancellationToken;

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
    use std::time::Duration;

    #[tokio::test]
    async fn test_control_manager_spawn() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: None,
            is_shutting_down: false,
            current_shutdown_phase: None,
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Just make sure we can spawn the ControlManager
        let _actor_ref = Actor::spawn(
            Some("test-control-manager".to_string()),
            ControlManager,
            args,
        )
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_control_manager_handle_prepare_for_shutdown() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: None,
            is_shutting_down: false,
            current_shutdown_phase: None,
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Spawn ControlManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-control-manager-2".to_string()),
            ControlManager,
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
    async fn test_control_manager_handle_shutdown_phase_complete() -> Result<(), ActorProcessingErr>
    {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: None,
            is_shutting_down: false,
            current_shutdown_phase: Some(ShutdownPhase::StopAcceptingNewConnections),
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Spawn ControlManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-control-manager-3".to_string()),
            ControlManager,
            args,
        )
        .await?;

        // Send a shutdown phase complete message to the actor
        let msg = BrokerMessage::ShutdownPhaseComplete {
            phase: ShutdownPhase::StopAcceptingNewConnections,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_control_manager_handle_initiate_shutdown_phase() -> Result<(), ActorProcessingErr>
    {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: None,
            is_shutting_down: false,
            current_shutdown_phase: Some(ShutdownPhase::StopAcceptingNewConnections),
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Spawn ControlManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-control-manager-4".to_string()),
            ControlManager,
            args,
        )
        .await?;

        // Send an initiate shutdown phase message to the actor
        let msg = BrokerMessage::InitiateShutdownPhase {
            phase: ShutdownPhase::StopAcceptingNewConnections,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_control_manager_handle_control_request_ping() -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: None,
            is_shutting_down: false,
            current_shutdown_phase: None,
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Spawn ControlManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-control-manager-5".to_string()),
            ControlManager,
            args,
        )
        .await?;

        // Send a control request with ping operation to the actor
        let msg = BrokerMessage::ControlRequest {
            registration_id: "test-client".to_string(),
            op: ControlOp::Ping,
            trace_ctx: None,
            reply_to: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_control_manager_handle_control_request_list_sessions()
    -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: None,
            is_shutting_down: false,
            current_shutdown_phase: None,
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Spawn ControlManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-control-manager-6".to_string()),
            ControlManager,
            args,
        )
        .await?;

        // Send a control request with list sessions operation to the actor
        let msg = BrokerMessage::ControlRequest {
            registration_id: "test-client".to_string(),
            op: ControlOp::ListSessions,
            trace_ctx: None,
            reply_to: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_control_manager_handle_control_request_list_topics()
    -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: None,
            is_shutting_down: false,
            current_shutdown_phase: None,
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Spawn ControlManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-control-manager-7".to_string()),
            ControlManager,
            args,
        )
        .await?;

        // Send a control request with list topics operation to the actor
        let msg = BrokerMessage::ControlRequest {
            registration_id: "test-client".to_string(),
            op: ControlOp::ListTopics,
            trace_ctx: None,
            reply_to: None,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_control_manager_handle_prepare_for_shutdown_with_token()
    -> Result<(), ActorProcessingErr> {
        // Create a mock broker to pass as argument
        let listener_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let session_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let topic_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;
        let subscriber_mgr = Actor::spawn(None, MockActor, ()).await.unwrap().0;

        let args = ControlManagerState {
            listener_mgr,
            session_mgr,
            topic_mgr,
            subscriber_mgr,
            shutdown_auth_token: Some("test-token".to_string()),
            is_shutting_down: false,
            current_shutdown_phase: None,
            phase_completed: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
        };

        // Spawn ControlManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-control-manager-8".to_string()),
            ControlManager,
            args,
        )
        .await?;

        // Send a prepare for shutdown message with token to the actor
        let msg = BrokerMessage::PrepareForShutdown {
            auth_token: Some("test-token".to_string()),
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }
}
