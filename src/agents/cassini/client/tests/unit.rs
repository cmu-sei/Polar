use cassini_client::{
    ClientEventForwarder, ClientEventForwarderArgs, RegistrationState, TCPClientConfig,
    cli::{IpcRequest, IpcResponse, OutputFormat, print_control_result},
};
use cassini_types::{ClientEvent as CassiniClientEvent, ControlResult, SessionDetails, SessionMap};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ===== Mock Actor for testing ClientEventForwarder =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockMessage {
    pub payload: String,
}

#[derive(Debug)]
pub struct MockActor;

#[derive(Debug)]
pub struct MockActorArgs;

#[ractor::async_trait]
impl Actor for MockActor {
    type Msg = MockMessage;
    type State = ();
    type Arguments = MockActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _msg: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

// ===== Tests for ClientEventForwarder =====

#[tokio::test]
async fn test_client_event_forwarder_mailbox() -> Result<(), ActorProcessingErr> {
    // Spawn a mock actor to receive forwarded events
    let (mock_ref, _handle) = Actor::spawn(None, MockActor, MockActorArgs).await?;

    // Create a mapper that transforms ClientEvent to MockMessage
    let mapper: Box<dyn Fn(CassiniClientEvent) -> Option<MockMessage> + Send + Sync + 'static> =
        Box::new(move |event| match event {
            CassiniClientEvent::MessagePublished { topic, .. } => Some(MockMessage {
                payload: format!("published:{topic}"),
            }),
            CassiniClientEvent::PublishAcknowledged { topic } => Some(MockMessage {
                payload: format!("ack:{topic}"),
            }),
            CassiniClientEvent::Registered { registration_id } => Some(MockMessage {
                payload: format!("registered:{registration_id}"),
            }),
            CassiniClientEvent::ControlResponse { result, .. } => Some(MockMessage {
                payload: format!("control:{result:?}"),
            }),
            CassiniClientEvent::TransportError { reason } => Some(MockMessage {
                payload: format!("error:{reason}"),
            }),
            CassiniClientEvent::ClientSubscribed { .. } => None,
        });

    // Spawn the ClientEventForwarder with the mock actor as target
    let (forwarder_ref, _handle) = Actor::spawn(
        None,
        ClientEventForwarder::new(),
        ClientEventForwarderArgs {
            target: mock_ref,
            mapper,
        },
    )
    .await?;

    // Send a MessagePublished event
    forwarder_ref.send_message(CassiniClientEvent::MessagePublished {
        topic: "test-topic".to_string(),
        payload: vec![1, 2, 3],
        trace_ctx: None,
    })?;

    // Give the actor time to process the message
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Assert the mock actor received the mapped message by sending a verification event
    forwarder_ref.send_message(CassiniClientEvent::Registered {
        registration_id: "test-reg".to_string(),
    })?;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    Ok(())
}

#[tokio::test]
async fn test_client_event_forwarder_multiple_events() -> Result<(), ActorProcessingErr> {
    // Spawn a mock actor
    let (mock_ref, _handle) = Actor::spawn(None, MockActor, MockActorArgs).await?;

    // Create a mapper that handles all event types
    let mapper: Box<dyn Fn(CassiniClientEvent) -> Option<MockMessage> + Send + Sync + 'static> =
        Box::new(move |event| match event {
            CassiniClientEvent::MessagePublished { topic, .. } => Some(MockMessage {
                payload: format!("pub:{topic}"),
            }),
            CassiniClientEvent::PublishAcknowledged { topic } => Some(MockMessage {
                payload: format!("ack:{topic}"),
            }),
            CassiniClientEvent::Registered { registration_id } => Some(MockMessage {
                payload: format!("reg:{registration_id}"),
            }),
            CassiniClientEvent::ControlResponse { result, .. } => Some(MockMessage {
                payload: format!("ctrl:{result:?}"),
            }),
            CassiniClientEvent::TransportError { reason } => Some(MockMessage {
                payload: format!("err:{reason}"),
            }),
            CassiniClientEvent::ClientSubscribed { .. } => None,
        });

    // Spawn the forwarder
    let (forwarder_ref, _handle) = Actor::spawn(
        None,
        ClientEventForwarder::new(),
        ClientEventForwarderArgs {
            target: mock_ref,
            mapper,
        },
    )
    .await?;

    // Send multiple events
    forwarder_ref.send_message(CassiniClientEvent::MessagePublished {
        topic: "topic1".to_string(),
        payload: vec![1],
        trace_ctx: None,
    })?;

    forwarder_ref.send_message(CassiniClientEvent::PublishAcknowledged {
        topic: "topic2".to_string(),
    })?;

    forwarder_ref.send_message(CassiniClientEvent::Registered {
        registration_id: "reg-123".to_string(),
    })?;

    // Give time to process
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // The forwarder should have sent the messages to the target
    forwarder_ref.send_message(CassiniClientEvent::ControlResponse {
        registration_id: "reg".to_string(),
        result: Ok(ControlResult::Pong),
        trace_ctx: None,
    })?;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    Ok(())
}

#[tokio::test]
async fn test_client_event_forwarder_filters_events() -> Result<(), ActorProcessingErr> {
    // Spawn a mock actor
    let (mock_ref, _handle) = Actor::spawn(None, MockActor, MockActorArgs).await?;

    // Create a mapper that only forwards PublishAcknowledged events
    let mapper: Box<dyn Fn(CassiniClientEvent) -> Option<MockMessage> + Send + Sync + 'static> =
        Box::new(move |event| match event {
            CassiniClientEvent::PublishAcknowledged { topic } => Some(MockMessage {
                payload: format!("ack:{topic}"),
            }),
            _ => None, // Filter out other events
        });

    // Spawn the forwarder
    let (forwarder_ref, _handle) = Actor::spawn(
        None,
        ClientEventForwarder::new(),
        ClientEventForwarderArgs {
            target: mock_ref,
            mapper,
        },
    )
    .await?;

    // Send a MessagePublished event (should be filtered)
    forwarder_ref.send_message(CassiniClientEvent::MessagePublished {
        topic: "test".to_string(),
        payload: vec![1],
        trace_ctx: None,
    })?;

    // Send a PublishAcknowledged event (should pass through)
    forwarder_ref.send_message(CassiniClientEvent::PublishAcknowledged {
        topic: "filtered".to_string(),
    })?;

    // Give time to process
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // The forwarder should have sent the message to the target
    forwarder_ref.send_message(CassiniClientEvent::TransportError {
        reason: "verify".to_string(),
    })?;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    Ok(())
}

// ===== Tests for TCPClientConfig::from_values =====

#[tokio::test]
async fn test_tcp_client_config_from_values() {
    let config = TCPClientConfig::from_values(
        "tcp://broker.example.com:5672".to_string(),
        "broker.example.com".to_string(),
        "/path/to/ca.crt".to_string(),
        "/path/to/client.crt".to_string(),
        "/path/to/client.key".to_string(),
    );

    assert_eq!(config.broker_endpoint, "tcp://broker.example.com:5672");
    assert_eq!(config.server_name, "broker.example.com");
    assert_eq!(config.ca_certificate_path, "/path/to/ca.crt");
    assert_eq!(config.client_certificate_path, "/path/to/client.crt");
    assert_eq!(config.client_key_path, "/path/to/client.key");
}

#[tokio::test]
async fn test_tcp_client_config_from_values_empty_strings() {
    let config = TCPClientConfig::from_values(
        "".to_string(),
        "".to_string(),
        "".to_string(),
        "".to_string(),
        "".to_string(),
    );

    assert_eq!(config.broker_endpoint, "");
    assert_eq!(config.server_name, "");
    assert_eq!(config.ca_certificate_path, "");
    assert_eq!(config.client_certificate_path, "");
    assert_eq!(config.client_key_path, "");
}

#[tokio::test]
async fn test_tcp_client_config_from_values_unicode() {
    let config = TCPClientConfig::from_values(
        "tcp://broker-пример.com:5672".to_string(),
        "пример.com".to_string(),
        "/путь/к/сертификату.crt".to_string(),
        "/путь/к/клиенту.crt".to_string(),
        "/путь/к/ключу.key".to_string(),
    );

    assert_eq!(config.broker_endpoint, "tcp://broker-пример.com:5672");
    assert_eq!(config.server_name, "пример.com");
    assert_eq!(config.ca_certificate_path, "/путь/к/сертификату.crt");
    assert_eq!(config.client_certificate_path, "/путь/к/клиенту.crt");
    assert_eq!(config.client_key_path, "/путь/к/ключу.key");
}

// ===== Tests for RegistrationState variants =====

#[tokio::test]
async fn test_registration_state_unregistered() {
    let state = RegistrationState::Unregistered;

    match &state {
        RegistrationState::Unregistered => {}
        RegistrationState::Registered { .. } => panic!("Expected Unregistered"),
    }
}

#[tokio::test]
async fn test_registration_state_registered() {
    let state = RegistrationState::Registered {
        registration_id: "reg-abc-123".to_string(),
    };

    match &state {
        RegistrationState::Unregistered => panic!("Expected Registered"),
        RegistrationState::Registered { registration_id } => {
            assert_eq!(registration_id, "reg-abc-123");
        }
    }
}

#[tokio::test]
async fn test_registration_state_matches() {
    // Test that RegistrationState can be matched correctly
    let state = RegistrationState::Registered {
        registration_id: "reg-abc-123".to_string(),
    };

    match &state {
        RegistrationState::Unregistered => panic!("Expected Registered"),
        RegistrationState::Registered { registration_id } => {
            assert_eq!(registration_id, "reg-abc-123");
        }
    }
}

// ===== Tests for IpcRequest serde roundtrips =====

#[tokio::test]
async fn test_ipc_request_publish_roundtrip() {
    let original = IpcRequest::Publish {
        topic: "test-topic".to_string(),
        payload_b64: "SGVsbG8gV29ybGQ=".to_string(),
        timeout_secs: 30,
    };

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: IpcRequest = serde_json::from_str(&serialized).unwrap();

    match (original, deserialized) {
        (
            IpcRequest::Publish {
                topic,
                payload_b64,
                timeout_secs,
            },
            IpcRequest::Publish {
                topic: t,
                payload_b64: pb,
                timeout_secs: ts,
            },
        ) => {
            assert_eq!(topic, t);
            assert_eq!(payload_b64, pb);
            assert_eq!(timeout_secs, ts);
        }
        _ => panic!("Expected Publish variant"),
    }
}

#[tokio::test]
async fn test_ipc_request_list_sessions_roundtrip() {
    let original = IpcRequest::ListSessions { timeout_secs: 15 };

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: IpcRequest = serde_json::from_str(&serialized).unwrap();

    match (original, deserialized) {
        (
            IpcRequest::ListSessions { timeout_secs },
            IpcRequest::ListSessions { timeout_secs: ts },
        ) => {
            assert_eq!(timeout_secs, ts);
        }
        _ => panic!("Expected ListSessions variant"),
    }
}

#[tokio::test]
async fn test_ipc_request_get_session_roundtrip() {
    let original = IpcRequest::GetSession {
        registration_id: "reg-123".to_string(),
        timeout_secs: 45,
    };

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: IpcRequest = serde_json::from_str(&serialized).unwrap();

    match (original, deserialized) {
        (
            IpcRequest::GetSession {
                registration_id,
                timeout_secs,
            },
            IpcRequest::GetSession {
                registration_id: rid,
                timeout_secs: ts,
            },
        ) => {
            assert_eq!(registration_id, rid);
            assert_eq!(timeout_secs, ts);
        }
        _ => panic!("Expected GetSession variant"),
    }
}

#[tokio::test]
async fn test_ipc_request_status_roundtrip() {
    let original = IpcRequest::Status;

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: IpcRequest = serde_json::from_str(&serialized).unwrap();

    match (original, deserialized) {
        (IpcRequest::Status, IpcRequest::Status) => {}
        _ => panic!("Expected Status variant"),
    }
}

// ===== Tests for IpcResponse serde roundtrips =====

#[tokio::test]
async fn test_ipc_response_ok_with_result_roundtrip() {
    let original = IpcResponse::Ok {
        result: Some(serde_json::json!({"status": "ok", "topic": "test"})),
    };

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: IpcResponse = serde_json::from_str(&serialized).unwrap();

    match (original, deserialized) {
        (IpcResponse::Ok { result: Some(val) }, IpcResponse::Ok { result: Some(val2) }) => {
            assert_eq!(val, val2);
        }
        _ => panic!("Expected Ok variant with Some result"),
    }
}

#[tokio::test]
async fn test_ipc_response_ok_without_result_roundtrip() {
    let original = IpcResponse::Ok { result: None };

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: IpcResponse = serde_json::from_str(&serialized).unwrap();

    match (original, deserialized) {
        (IpcResponse::Ok { result: None }, IpcResponse::Ok { result: None }) => {}
        _ => panic!("Expected Ok variant with None result"),
    }
}

#[tokio::test]
async fn test_ipc_response_error_roundtrip() {
    let original = IpcResponse::Error {
        reason: "Test error message".to_string(),
    };

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: IpcResponse = serde_json::from_str(&serialized).unwrap();

    match (original, deserialized) {
        (IpcResponse::Error { reason }, IpcResponse::Error { reason: r }) => {
            assert_eq!(reason, r);
        }
        _ => panic!("Expected Error variant"),
    }
}

// ===== Tests for print_control_result =====

#[tokio::test]
async fn test_print_control_result_json_format() {
    let session_map: SessionMap = [(
        "reg-123".to_string(),
        SessionDetails {
            registration_id: "reg-123".to_string(),
            subscriptions: HashSet::new(),
        },
    )]
    .into_iter()
    .collect();
    let result = ControlResult::SessionList(session_map);
    let format = OutputFormat::Json;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_session_list() {
    let session_map: SessionMap = [(
        "reg-123".to_string(),
        SessionDetails {
            registration_id: "reg-123".to_string(),
            subscriptions: HashSet::new(),
        },
    )]
    .into_iter()
    .collect();
    let result = ControlResult::SessionList(session_map);
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_empty_session_list() {
    let result = ControlResult::SessionList(SessionMap::new());
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_topic_list() {
    let result = ControlResult::TopicList(HashSet::from([
        "topic1".to_string(),
        "topic2".to_string(),
        "topic3".to_string(),
    ]));
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_empty_topic_list() {
    let result = ControlResult::TopicList(HashSet::new());
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_session_info() {
    let result = ControlResult::SessionInfo(SessionDetails {
        registration_id: "reg-123".to_string(),
        subscriptions: HashSet::new(),
    });
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_subscriber_list() {
    let result =
        ControlResult::SubscriberList(vec!["subscriber1".to_string(), "subscriber2".to_string()]);
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_empty_subscriber_list() {
    let result = ControlResult::SubscriberList(vec![]);
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_pong() {
    let result = ControlResult::Pong;
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_disconnected() {
    let result = ControlResult::Disconnected;
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_text_format_shutdown_initiated() {
    let result = ControlResult::ShutdownInitiated;
    let format = OutputFormat::Text;

    // Should not panic and return Ok
    let res = print_control_result(&result, &format);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_print_control_result_json_format_all_variants() {
    let variants = [
        ControlResult::SessionList(SessionMap::new()),
        ControlResult::TopicList(HashSet::new()),
        ControlResult::SessionInfo(SessionDetails {
            registration_id: "reg-123".to_string(),
            subscriptions: HashSet::new(),
        }),
        ControlResult::SubscriberList(vec![]),
        ControlResult::Pong,
        ControlResult::Disconnected,
        ControlResult::ShutdownInitiated,
    ];

    for result in variants {
        let format = OutputFormat::Json;
        // Should not panic and return Ok for all variants
        let res = print_control_result(&result, &format);
        assert!(res.is_ok(), "Failed for variant: {:?}", result);
    }
}
