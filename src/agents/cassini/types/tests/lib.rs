//! Unit tests for cassini-types lib.rs types
//! Tests constructors, conversions, and error paths

use cassini_types::{
    BrokerMessage, ClientEvent, ClientMessage, ControlError, ControlOp, ControlResult,
    DisconnectReason, SessionDetails, SessionMap, SessionSummary, ShutdownPhase,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[test]
fn test_session_details_constructor() {
    let details = SessionDetails {
        registration_id: "test-id".to_string(),
        subscriptions: HashSet::from(["topic1".to_string(), "topic2".to_string()]),
    };

    assert_eq!(details.registration_id, "test-id");
    assert_eq!(details.subscriptions.len(), 2);
}

#[test]
fn test_session_details_clone() {
    let details = SessionDetails {
        registration_id: "test-id".to_string(),
        subscriptions: HashSet::from(["topic1".to_string()]),
    };

    let cloned = details.clone();
    assert_eq!(details.registration_id, cloned.registration_id);
    assert_eq!(details.subscriptions, cloned.subscriptions);
}

#[test]
fn test_disconnect_reason_variants() {
    let remote = DisconnectReason::RemoteClosed;
    let transport = DisconnectReason::TransportError("test error".to_string());

    assert_eq!(format!("{:?}", remote), "RemoteClosed");
    assert_eq!(format!("{:?}", transport), "TransportError(\"test error\")");
}

#[test]
fn test_shutdown_phase_variants() {
    assert_eq!(format!("{:?}", ShutdownPhase::StopAcceptingNewConnections),
               "StopAcceptingNewConnections");
    assert_eq!(format!("{:?}", ShutdownPhase::DrainExistingSessions),
               "DrainExistingSessions");
    assert_eq!(format!("{:?}", ShutdownPhase::FlushTopicQueues),
               "FlushTopicQueues");
    assert_eq!(format!("{:?}", ShutdownPhase::TerminateSubscribers),
               "TerminateSubscribers");
    assert_eq!(format!("{:?}", ShutdownPhase::TerminateListeners),
               "TerminateListeners");
}

#[test]
fn test_control_op_variants() {
    assert_eq!(format!("{:?}", ControlOp::GetSessionInfo { registration_id: "id".to_string() }),
               "GetSessionInfo { registration_id: \"id\" }");
    assert_eq!(format!("{:?}", ControlOp::DisconnectSession { registration_id: "id".to_string() }),
               "DisconnectSession { registration_id: \"id\" }");
    assert_eq!(format!("{:?}", ControlOp::ListSessions), "ListSessions");
    assert_eq!(format!("{:?}", ControlOp::ListTopics), "ListTopics");
    assert_eq!(format!("{:?}", ControlOp::ListSubscribers { topic: "t".to_string() }),
               "ListSubscribers { topic: \"t\" }");
    assert_eq!(format!("{:?}", ControlOp::PrepareForShutdown { auth_token: "token".to_string() }),
               "PrepareForShutdown { auth_token: \"token\" }");
    assert_eq!(format!("{:?}", ControlOp::GetBrokerStats), "GetBrokerStats");
    assert_eq!(format!("{:?}", ControlOp::ShutdownBroker { graceful: true }),
               "ShutdownBroker { graceful: true }");
    assert_eq!(format!("{:?}", ControlOp::Ping), "Ping");
}

#[test]
fn test_control_result_variants() {
    let details = SessionDetails {
        registration_id: "id".to_string(),
        subscriptions: HashSet::new(),
    };

    let session_info = format!("{:?}", ControlResult::SessionInfo(details.clone()));
    assert!(session_info.contains("SessionInfo"));
    assert!(session_info.contains("registration_id"));
    assert_eq!(format!("{:?}", ControlResult::SessionList(HashMap::new())),
               "SessionList({})");
    assert_eq!(format!("{:?}", ControlResult::SubscriberList(vec!["t1".to_string()])),
               "SubscriberList([\"t1\"])");
    assert_eq!(format!("{:?}", ControlResult::TopicList(HashSet::from(["t1".to_string()]))),
               "TopicList({\"t1\"})");
    assert_eq!(format!("{:?}", ControlResult::Pong), "Pong");
    assert_eq!(format!("{:?}", ControlResult::Disconnected), "Disconnected");
    assert_eq!(format!("{:?}", ControlResult::ShutdownInitiated), "ShutdownInitiated");
}

#[test]
fn test_control_error_variants() {
    assert_eq!(format!("{:?}", ControlError::NotFound("not found".to_string())),
               "NotFound(\"not found\")");
    assert_eq!(format!("{:?}", ControlError::PermissionDenied("denied".to_string())),
               "PermissionDenied(\"denied\")");
    assert_eq!(format!("{:?}", ControlError::InternalError("internal".to_string())),
               "InternalError(\"internal\")");
}

#[test]
fn test_session_summary_constructor() {
    let summary = SessionSummary {
        registration_id: "test-id".to_string(),
    };

    assert_eq!(summary.registration_id, "test-id");
}

#[test]
fn test_session_map() {
    let mut map: SessionMap = HashMap::new();
    map.insert("id1".to_string(), SessionDetails {
        registration_id: "id1".to_string(),
        subscriptions: HashSet::new(),
    });

    assert_eq!(map.len(), 1);
    assert_eq!(map.get("id1").unwrap().registration_id, "id1");
}

#[test]
fn test_client_event_variants() {
    let event = ClientEvent::Registered {
        registration_id: "id".to_string(),
    };

    assert_eq!(format!("{:?}", event), "Registered { registration_id: \"id\" }");

    let event = ClientEvent::MessagePublished {
        topic: "topic".to_string(),
        payload: vec![1, 2, 3],
        trace_ctx: None,
    };

    assert_eq!(format!("{:?}", event), "MessagePublished { topic: \"topic\", payload: [1, 2, 3], trace_ctx: None }");

    let event = ClientEvent::TransportError {
        reason: "test".to_string(),
    };

    assert_eq!(format!("{:?}", event), "TransportError { reason: \"test\" }");

    let event = ClientEvent::ControlResponse {
        registration_id: "id".to_string(),
        result: Ok(ControlResult::Pong),
        trace_ctx: None,
    };

    assert_eq!(format!("{:?}", event), "ControlResponse { registration_id: \"id\", result: Ok(Pong), trace_ctx: None }");

    let event = ClientEvent::PublishAcknowledged {
        topic: "topic".to_string(),
    };

    assert_eq!(format!("{:?}", event), "PublishAcknowledged { topic: \"topic\" }");

    let event = ClientEvent::ClientSubscribed {
        topic: "topic".to_string(),
    };

    assert_eq!(format!("{:?}", event), "ClientSubscribed { topic: \"topic\" }");
}

#[test]
fn test_client_message_registration_request() {
    let msg = ClientMessage::RegistrationRequest {
        registration_id: None,
        trace_ctx: None,
    };

    assert_eq!(format!("{:?}", msg), "RegistrationRequest { registration_id: None, trace_ctx: None }");
}

#[test]
fn test_client_message_registration_response() {
    let msg = ClientMessage::RegistrationResponse {
        result: Ok("new-id".to_string()),
        trace_ctx: None,
    };

    assert_eq!(format!("{:?}", msg), "RegistrationResponse { result: Ok(\"new-id\"), trace_ctx: None }");

    let msg = ClientMessage::RegistrationResponse {
        result: Err("failed".to_string()),
        trace_ctx: None,
    };

    assert_eq!(format!("{:?}", msg), "RegistrationResponse { result: Err(\"failed\"), trace_ctx: None }");
}

#[test]
fn test_client_message_publish_request() {
    let payload = Arc::new(vec![1, 2, 3]);
    let msg = ClientMessage::PublishRequest {
        topic: "topic".to_string(),
        payload: payload.clone(),
        registration_id: "id".to_string(),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::PublishRequest { topic, payload, registration_id, .. } => {
            assert_eq!(topic, "topic");
            assert_eq!(registration_id, "id");
            assert_eq!(payload, payload);
        }
        _ => panic!("Expected PublishRequest"),
    }
}

#[test]
fn test_client_message_subscribe_request() {
    let msg = ClientMessage::SubscribeRequest {
        registration_id: "id".to_string(),
        topic: "topic".to_string(),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::SubscribeRequest { registration_id, topic, .. } => {
            assert_eq!(registration_id, "id");
            assert_eq!(topic, "topic");
        }
        _ => panic!("Expected SubscribeRequest"),
    }
}

#[test]
fn test_client_message_unsubscribe_request() {
    let msg = ClientMessage::UnsubscribeRequest {
        registration_id: "id".to_string(),
        topic: "topic".to_string(),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::UnsubscribeRequest { registration_id, topic, .. } => {
            assert_eq!(registration_id, "id");
            assert_eq!(topic, "topic");
        }
        _ => panic!("Expected UnsubscribeRequest"),
    }
}

#[test]
fn test_client_message_disconnect_request() {
    let msg = ClientMessage::DisconnectRequest {
        registration_id: Some("id".to_string()),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::DisconnectRequest { registration_id, .. } => {
            assert_eq!(*registration_id, Some("id".to_string()));
        }
        _ => panic!("Expected DisconnectRequest"),
    }
}

#[test]
fn test_client_message_control_request() {
    let msg = ClientMessage::ControlRequest {
        registration_id: "id".to_string(),
        op: ControlOp::Ping,
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::ControlRequest { registration_id, op, .. } => {
            assert_eq!(registration_id, "id");
            assert_eq!(format!("{:?}", op), "Ping");
        }
        _ => panic!("Expected ControlRequest"),
    }
}

#[test]
fn test_broker_message_from_client_message() {
    let client_msg = ClientMessage::RegistrationRequest {
        registration_id: Some("client-id".to_string()),
        trace_ctx: None,
    };

    let broker_msg = BrokerMessage::from_client_message(client_msg, "client1".to_string());

    match broker_msg {
        BrokerMessage::RegistrationRequest {
            registration_id,
            client_id,
            ..
        } => {
            assert_eq!(registration_id, Some("client-id".to_string()));
            assert_eq!(client_id, "client1".to_string());
        }
        _ => panic!("Expected RegistrationRequest"),
    }
}

#[test]
fn test_broker_message_from_client_message_unsupported() {
    // Test that PublishRequest is converted to PublishRequest (not ErrorMessage)
    let client_msg = ClientMessage::PublishRequest {
        topic: "topic".to_string(),
        payload: Arc::new(vec![1, 2, 3]),
        registration_id: "id".to_string(),
        trace_ctx: None,
    };

    let broker_msg = BrokerMessage::from_client_message(client_msg, "client1".to_string());

    match &broker_msg {
        BrokerMessage::PublishRequest { topic, payload, registration_id, .. } => {
            assert_eq!(topic, "topic");
            assert_eq!(registration_id, "id");
            assert_eq!(payload, &Arc::new(vec![1, 2, 3]));
        }
        _ => panic!("Expected PublishRequest"),
    }
}

#[test]
fn test_client_message_publish_response() {
    let payload = Arc::new(vec![1, 2, 3]);
    let msg = ClientMessage::PublishResponse {
        topic: "topic".to_string(),
        payload: payload.clone(),
        result: Ok(()),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::PublishResponse { topic, result, .. } => {
            assert_eq!(topic, "topic");
            assert_eq!(result, &Ok(()));
        }
        _ => panic!("Expected PublishResponse"),
    }
}

#[test]
fn test_client_message_subscribe_acknowledgment() {
    let msg = ClientMessage::SubscribeAcknowledgment {
        topic: "topic".to_string(),
        result: Ok(()),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::SubscribeAcknowledgment { topic, result, .. } => {
            assert_eq!(topic, "topic");
            assert_eq!(result, &Ok(()));
        }
        _ => panic!("Expected SubscribeAcknowledgment"),
    }
}

#[test]
fn test_client_message_unsubscribe_acknowledgment() {
    let msg = ClientMessage::UnsubscribeAcknowledgment {
        topic: "topic".to_string(),
        result: Ok(()),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::UnsubscribeAcknowledgment { topic, result, .. } => {
            assert_eq!(topic, "topic");
            assert_eq!(result, &Ok(()));
        }
        _ => panic!("Expected UnsubscribeAcknowledgment"),
    }
}

#[test]
fn test_client_message_error_message() {
    let msg = ClientMessage::ErrorMessage {
        error: "test error".to_string(),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::ErrorMessage { error, .. } => {
            assert_eq!(error, "test error");
        }
        _ => panic!("Expected ErrorMessage"),
    }
}

#[test]
fn test_client_message_control_response() {
    let msg = ClientMessage::ControlResponse {
        registration_id: "id".to_string(),
        result: Ok(ControlResult::Pong),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::ControlResponse { registration_id, result, .. } => {
            assert_eq!(*registration_id, "id");
            // ControlResult doesn't implement PartialEq, so we just check the variant exists
            assert!(matches!(result, Ok(ControlResult::Pong)));
        }
        _ => panic!("Expected ControlResponse"),
    }
}

#[test]
fn test_client_message_publish_request_ack() {
    let msg = ClientMessage::PublishRequestAck {
        topic: "topic".to_string(),
        trace_ctx: None,
    };

    match &msg {
        ClientMessage::PublishRequestAck { topic, .. } => {
            assert_eq!(topic, "topic");
        }
        _ => panic!("Expected PublishRequestAck"),
    }
}
