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

use cassini_client::{
    ClientKey, RegistrationState, TCPClientConfig,
    cli::{IpcRequest, IpcResponse, OutputFormat, print_control_result},
};
use cassini_types::{ControlResult, SessionDetails, SessionMap};
use proptest::prelude::*;
use std::collections::HashSet;

// ===== Proptests for TCPClientConfig::from_values =====

proptest! {
    #[test]
    fn prop_tcp_client_config_stores_arbitrary_strings(
        endpoint in any::<String>(),
        server_name in any::<String>(),
        ca_cert in any::<String>(),
        client_cert in any::<String>(),
        client_key in any::<String>(),
    ) {
        let config = TCPClientConfig::from_values(
            endpoint.clone(),
            server_name.clone(),
            ca_cert.clone(),
            client_cert.clone(),
            client_key.clone(),
        );
        prop_assert_eq!(&config.broker_endpoint, &endpoint);
        prop_assert_eq!(&config.server_name, &server_name);
        prop_assert_eq!(&config.ca_certificate_path, &ca_cert);
        prop_assert_eq!(&config.client_certificate_path, &client_cert);
        prop_assert_eq!(&config.client_key, &ClientKey::File(client_key));
    }

    #[test]
    fn prop_tcp_client_config_handles_unicode(
        endpoint in "\\PC*",
        server_name in "\\PC*",
    ) {
        let config = TCPClientConfig::from_values(
            endpoint.clone(),
            server_name.clone(),
            "/ca.crt".to_string(),
            "/client.crt".to_string(),
            "/client.key".to_string(),
        );
        prop_assert_eq!(&config.broker_endpoint, &endpoint);
        prop_assert_eq!(&config.server_name, &server_name);
    }
}

// ===== Proptests for RegistrationState =====

proptest! {
    #[test]
    fn prop_registration_state_registered_stores_id(
        registration_id in any::<String>(),
    ) {
        let state = RegistrationState::Registered {
            registration_id: registration_id.clone(),
        };
        match state {
            RegistrationState::Registered { registration_id: id } => {
                prop_assert_eq!(id, registration_id);
            }
            RegistrationState::Unregistered => prop_assert!(false, "Expected Registered"),
        }
    }
}

// ===== Proptests for IpcRequest serde roundtrips =====

proptest! {
    #[test]
    fn prop_ipc_request_publish_roundtrip(
        topic in "\\PC{0,128}",
        payload_b64 in "\\PC{0,256}",
        timeout_secs in 1u64..300,
    ) {
        let original = IpcRequest::Publish {
            topic: topic.clone(),
            payload_b64: payload_b64.clone(),
            timeout_secs,
        };
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: IpcRequest = serde_json::from_str(&serialized).unwrap();
        match deserialized {
            IpcRequest::Publish { topic: t, payload_b64: pb, timeout_secs: ts } => {
                prop_assert_eq!(t, topic);
                prop_assert_eq!(pb, payload_b64);
                prop_assert_eq!(ts, timeout_secs);
            }
            _ => prop_assert!(false, "Expected Publish variant after roundtrip"),
        }
    }

    #[test]
    fn prop_ipc_request_get_session_roundtrip(
        registration_id in "\\PC{0,128}",
        timeout_secs in 1u64..300,
    ) {
        let original = IpcRequest::GetSession {
            registration_id: registration_id.clone(),
            timeout_secs,
        };
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: IpcRequest = serde_json::from_str(&serialized).unwrap();
        match deserialized {
            IpcRequest::GetSession { registration_id: rid, timeout_secs: ts } => {
                prop_assert_eq!(rid, registration_id);
                prop_assert_eq!(ts, timeout_secs);
            }
            _ => prop_assert!(false, "Expected GetSession variant after roundtrip"),
        }
    }

    #[test]
    fn prop_ipc_request_topic_arbitrary_chars(
        topic in "\\PC{0,256}",
    ) {
        // Topics with arbitrary printable characters — client must not panic
        let original = IpcRequest::Publish {
            topic: topic.clone(),
            payload_b64: "SGVsbG8=".to_string(),
            timeout_secs: 5,
        };
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: IpcRequest = serde_json::from_str(&serialized).unwrap();
        match deserialized {
            IpcRequest::Publish { topic: t, .. } => prop_assert_eq!(t, topic),
            _ => prop_assert!(false, "Expected Publish"),
        }
    }
}

// ===== Proptests for IpcResponse serde roundtrips =====

proptest! {
    #[test]
    fn prop_ipc_response_error_roundtrip(
        reason in any::<String>(),
    ) {
        let original = IpcResponse::Error { reason: reason.clone() };
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: IpcResponse = serde_json::from_str(&serialized).unwrap();
        match deserialized {
            IpcResponse::Error { reason: r } => prop_assert_eq!(r, reason),
            _ => prop_assert!(false, "Expected Error variant after roundtrip"),
        }
    }

    #[test]
    fn prop_ipc_response_ok_roundtrip(
        key in "[a-z]{1,16}",
        value in "[a-z]{1,32}",
    ) {
        let original = IpcResponse::Ok {
            result: Some(serde_json::json!({ key.clone(): value.clone() })),
        };
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: IpcResponse = serde_json::from_str(&serialized).unwrap();
        match deserialized {
            IpcResponse::Ok { result: Some(v) } => {
                prop_assert_eq!(v[&key].as_str().unwrap(), &value);
            }
            _ => prop_assert!(false, "Expected Ok variant after roundtrip"),
        }
    }
}

// ===== Proptests for print_control_result =====

proptest! {
    #[test]
    fn prop_print_control_result_topic_list_no_panic(
        topics in prop::collection::vec("[a-zA-Z0-9._/-]{1,64}", 0..20),
    ) {
        let result = ControlResult::TopicList(topics.into_iter().collect::<HashSet<_>>());
        prop_assert!(print_control_result(&result, &OutputFormat::Text).is_ok());
        let result = ControlResult::TopicList(HashSet::new());
        prop_assert!(print_control_result(&result, &OutputFormat::Json).is_ok());
    }

    #[test]
    fn prop_print_control_result_subscriber_list_no_panic(
        subscribers in prop::collection::vec(any::<String>(), 0..20),
    ) {
        let result = ControlResult::SubscriberList(subscribers);
        prop_assert!(print_control_result(&result, &OutputFormat::Text).is_ok());
    }

    #[test]
    fn prop_print_control_result_session_list_no_panic(
        registration_ids in prop::collection::vec("[a-zA-Z0-9-]{1,32}", 0..10),
    ) {
        let mut session_map = SessionMap::new();
        for id in &registration_ids {
            session_map.insert(id.clone(), SessionDetails {
                registration_id: id.clone(),
                subscriptions: HashSet::new(),
            });
        }
        let result = ControlResult::SessionList(session_map);
        prop_assert!(print_control_result(&result, &OutputFormat::Text).is_ok());
        prop_assert!(print_control_result(&result, &OutputFormat::Json).is_ok());
    }
}

// ===== The outbox: a message leaves only on its own acknowledgement =====

/// What happens to an outbox: a message published (of so many bytes), or an acknowledgement of the message pushed
/// `nth` (counted from the first, so it may name one already acknowledged, one not yet pushed, or another epoch's).
#[derive(Debug, Clone)]
enum OutboxEvent {
    Push(usize),
    Ack { nth: usize, other_epoch: bool },
}

fn outbox_events() -> impl Strategy<Value = Vec<OutboxEvent>> {
    prop::collection::vec(
        prop_oneof![
            (0usize..64).prop_map(OutboxEvent::Push),
            (0usize..40, prop::bool::weighted(0.1))
                .prop_map(|(nth, other_epoch)| OutboxEvent::Ack { nth, other_epoch }),
        ],
        0..80,
    )
}

proptest! {
    /// For any interleaving of publishes and acknowledgements, in any order, repeated or of messages it never held:
    /// the outbox holds exactly the messages pushed whose own acknowledgement has not come, oldest first, numbered
    /// 1, 2, 3 in its epoch, and its bytes are theirs.
    #[test]
    fn no_message_leaves_the_outbox_before_its_own_acknowledgement(events in outbox_events()) {
        use cassini_client::outbox::{Outbox, OutboxLimits};
        use cassini_types::MessageId;
        use std::sync::Arc;
        let epoch = 11;
        let mut outbox = Outbox::new(epoch, OutboxLimits { messages: usize::MAX, bytes: usize::MAX });
        let mut pushed: Vec<(MessageId, usize)> = Vec::new();
        let mut acked: HashSet<MessageId> = HashSet::new();
        for event in events {
            match event {
                OutboxEvent::Push(len) => {
                    let id = outbox.push("t".into(), Arc::new(vec![0; len]), None, true).id;
                    prop_assert_eq!(id, MessageId { epoch, seq: pushed.len() as u64 + 1 });
                    pushed.push((id, len));
                }
                OutboxEvent::Ack { nth, other_epoch } => {
                    let id = MessageId {
                        epoch: if other_epoch { epoch + 1 } else { epoch },
                        seq: nth as u64 + 1,
                    };
                    let held = pushed.iter().any(|(p, _)| *p == id) && !acked.contains(&id);
                    prop_assert_eq!(outbox.settle(id).map(|e| e.id), held.then_some(id));
                    // An acknowledgement of a message not held (not yet numbered, or already let go) changes nothing.
                    if held {
                        acked.insert(id);
                    }
                }
            }
            let expected: Vec<(MessageId, usize)> =
                pushed.iter().filter(|(id, _)| !acked.contains(id)).cloned().collect();
            prop_assert_eq!(
                outbox.entries().map(|e| (e.id, e.payload.len())).collect::<Vec<_>>(),
                expected.clone()
            );
            prop_assert_eq!(outbox.bytes(), expected.iter().map(|(_, len)| len).sum::<usize>());
        }
    }
}
