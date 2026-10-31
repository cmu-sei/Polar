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

//! Property tests for cassini-types lib.rs types
//! Tests rkyv serialize→deserialize roundtrips, Arc<Vec<u8>> payloads, and boundary conditions

use cassini_types::SessionDetails;
use proptest::prelude::*;

// ==================== SessionDetails Property Tests ====================

proptest! {
    #[test]
    fn test_session_details_rkyv_roundtrip(_payload in any::<Vec<u8>>()) {
        let details = SessionDetails {
            registration_id: "test-id".to_string(),
            subscriptions: std::collections::HashSet::new(),
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&details).unwrap();
        let deserialized: SessionDetails = rkyv::from_bytes::<SessionDetails, rkyv::rancor::Error>(&bytes).unwrap();

        // Compare fields individually since PartialEq is not implemented
        prop_assert_eq!(details.registration_id, deserialized.registration_id);
        prop_assert_eq!(details.subscriptions, deserialized.subscriptions);
    }

    #[test]
    fn test_session_details_serde_roundtrip(_payload in any::<Vec<u8>>()) {
        let details = SessionDetails {
            registration_id: "test-id".to_string(),
            subscriptions: std::collections::HashSet::new(),
        };

        let json = serde_json::to_string(&details).unwrap();
        let deserialized: SessionDetails = serde_json::from_str(&json).unwrap();

        prop_assert_eq!(details.registration_id, deserialized.registration_id);
        prop_assert_eq!(details.subscriptions, deserialized.subscriptions);
    }

    #[test]
    fn test_session_details_zero_length_payload(_payload in any::<Vec<u8>>()) {
        let details = SessionDetails {
            registration_id: "".to_string(),
            subscriptions: std::collections::HashSet::new(),
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&details).unwrap();
        let deserialized: SessionDetails = rkyv::from_bytes::<SessionDetails, rkyv::rancor::Error>(&bytes).unwrap();

        prop_assert_eq!(details.registration_id, deserialized.registration_id);
    }

    #[test]
    fn test_session_details_max_size_payload(_payload in any::<Vec<u8>>()) {
        let details = SessionDetails {
            registration_id: "a".repeat(1000).to_string(),
            subscriptions: std::collections::HashSet::new(),
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&details).unwrap();
        let deserialized: SessionDetails = rkyv::from_bytes::<SessionDetails, rkyv::rancor::Error>(&bytes).unwrap();

        prop_assert_eq!(details.registration_id, deserialized.registration_id);
    }

    #[test]
    fn test_session_details_non_utf8_payload(payload in any::<Vec<u8>>()) {
        let details = SessionDetails {
            registration_id: String::from_utf8(payload.clone()).unwrap_or_else(|_| "fallback".to_string()),
            subscriptions: std::collections::HashSet::new(),
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&details).unwrap();
        let deserialized: SessionDetails = rkyv::from_bytes::<SessionDetails, rkyv::rancor::Error>(&bytes).unwrap();

        prop_assert_eq!(details.registration_id, deserialized.registration_id);
    }

    #[test]
    fn test_session_details_null_bytes_payload(payload in any::<Vec<u8>>()) {
        let details = SessionDetails {
            registration_id: String::from_utf8(payload.clone()).unwrap_or_else(|_| "fallback".to_string()),
            subscriptions: std::collections::HashSet::new(),
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&details).unwrap();
        let deserialized: SessionDetails = rkyv::from_bytes::<SessionDetails, rkyv::rancor::Error>(&bytes).unwrap();

        prop_assert_eq!(details.registration_id, deserialized.registration_id);
    }
}

// ==================== Registration Property Tests ====================

proptest! {
    /// A registration request comes off the wire as it was sent, and the broker's message made of it asks to resume the
    /// same session, if any.
    #[test]
    fn a_registration_request_keeps_its_session_through_the_wire(
        registration_id in proptest::option::of("[a-z0-9-]{1,36}"),
    ) {
        use cassini_types::{BrokerMessage, ClientMessage};
        let sent = ClientMessage::RegistrationRequest { registration_id: registration_id.clone(), trace_ctx: None, protocol: cassini_types::PROTOCOL_VERSION };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&sent).unwrap();
        let received = rkyv::from_bytes::<ClientMessage, rkyv::rancor::Error>(&bytes).unwrap();
        match BrokerMessage::from_client_message(received, "c".to_string(), None) {
            BrokerMessage::RegistrationRequest { registration_id: got, .. } => {
                prop_assert_eq!(got, registration_id);
            }
            _ => prop_assert!(false, "not a registration request"),
        }
    }
}

// ==================== Numbered publishes ====================

proptest! {
    /// A publish keeps its `(epoch, seq)` through the wire and into the broker's message made of it, and its
    /// acknowledgement and a refusal of it keep the same pair: what the client's outbox settles by.
    #[test]
    fn a_publish_s_number_survives_the_wire_its_acknowledgement_and_its_refusal(
        epoch in any::<u64>(),
        seq in any::<u64>(),
        topic in "[a-z][a-z0-9.:-]{0,40}",
        payload in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        use cassini_types::{BrokerMessage, ClientMessage, MessageId};
        use std::sync::Arc;
        fn wire(message: &ClientMessage) -> ClientMessage {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(message).unwrap();
            rkyv::from_bytes::<ClientMessage, rkyv::rancor::Error>(&bytes).unwrap()
        }
        let sent = ClientMessage::PublishRequest {
            topic: topic.clone(),
            payload: Arc::new(payload.clone()),
            registration_id: "r".to_string(),
            trace_ctx: None,
            share_identity: true,
            epoch,
            seq,
        };
        match BrokerMessage::from_client_message(wire(&sent), "c".to_string(), Some("me".into())) {
            BrokerMessage::PublishRequest { id, topic: t, payload: p, published_by, .. } => {
                prop_assert_eq!(id, MessageId { epoch, seq });
                prop_assert_eq!(t, topic.clone());
                prop_assert_eq!(p.as_ref(), &payload);
                prop_assert_eq!(published_by.as_deref(), Some("me"));
            }
            _ => prop_assert!(false, "not a publish"),
        }
        let ack = ClientMessage::PublishRequestAck { topic: topic.clone(), epoch, seq, trace_ctx: None };
        match wire(&ack) {
            ClientMessage::PublishRequestAck { epoch: e, seq: s, topic: t, .. } => {
                prop_assert_eq!((e, s, t), (epoch, seq, topic.clone()));
            }
            _ => prop_assert!(false, "not an acknowledgement"),
        }
        let refusal = ClientMessage::PublishResponse {
            topic,
            payload: Arc::new(Vec::new()),
            result: Err("no".into()),
            trace_ctx: None,
            published_by: None,
            delivery_id: None,
            refusal: None,
            origin: Some(MessageId { epoch, seq }),
        };
        match wire(&refusal) {
            ClientMessage::PublishResponse { origin, result: Err(_), .. } => {
                prop_assert_eq!(origin, Some(MessageId { epoch, seq }));
            }
            _ => prop_assert!(false, "not a refusal"),
        }
    }
}

proptest! {
    /// A delivery keeps its origin (who published it and their number for it) and its delivery number through the
    /// wire, and the client's acknowledgement of it becomes the broker's message naming the same number.
    #[test]
    fn a_delivery_s_origin_and_number_survive_the_wire_and_its_acknowledgement(
        epoch in any::<u64>(),
        seq in any::<u64>(),
        delivery_id in any::<u64>(),
        published_by in proptest::option::of("[a-z][a-z0-9.-]{0,30}"),
    ) {
        use cassini_types::{BrokerMessage, ClientMessage, MessageId};
        use std::sync::Arc;
        fn wire(message: &ClientMessage) -> ClientMessage {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(message).unwrap();
            rkyv::from_bytes::<ClientMessage, rkyv::rancor::Error>(&bytes).unwrap()
        }
        let delivery = ClientMessage::PublishResponse {
            topic: "t".to_string(),
            payload: Arc::new(vec![1, 2]),
            result: Ok(()),
            trace_ctx: None,
            published_by: published_by.clone(),
            origin: Some(MessageId { epoch, seq }),
            delivery_id: Some(delivery_id),
            refusal: None,
        };
        match wire(&delivery) {
            ClientMessage::PublishResponse { origin, delivery_id: d, published_by: p, .. } => {
                prop_assert_eq!(origin, Some(MessageId { epoch, seq }));
                prop_assert_eq!(d, Some(delivery_id));
                prop_assert_eq!(p, published_by);
            }
            _ => prop_assert!(false, "not a delivery"),
        }
        let ack = wire(&ClientMessage::PublishResponseAck { delivery_id });
        match BrokerMessage::from_client_message(ack, "c".to_string(), None) {
            BrokerMessage::PublishResponseAck { delivery_id: d } => prop_assert_eq!(d, delivery_id),
            _ => prop_assert!(false, "not an acknowledgement of a delivery"),
        }
    }
}

// ==================== Registration refusal Property Tests ====================

fn refusal_code() -> impl Strategy<Value = cassini_types::RefusalCode> {
    prop_oneof![
        Just(cassini_types::RefusalCode::Revoked),
        Just(cassini_types::RefusalCode::Other),
    ]
}

proptest! {
    /// A refused registration comes off the wire with the code it was sent with, whatever its text: a reader acts on
    /// the code, and the text, for people, cannot change what it means.
    #[test]
    fn a_registration_refusal_keeps_its_code_and_its_text_through_the_wire(
        code in refusal_code(),
        reason in any::<String>(),
    ) {
        use cassini_types::{ClientMessage, Refusal};
        let sent = ClientMessage::RegistrationResponse {
            result: Err(Refusal { code, reason: reason.clone() }),
            trace_ctx: None,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&sent).unwrap();
        match rkyv::from_bytes::<ClientMessage, rkyv::rancor::Error>(&bytes).unwrap() {
            ClientMessage::RegistrationResponse { result: Err(got), .. } => {
                prop_assert_eq!(got, Refusal { code, reason });
            }
            other => prop_assert!(false, "not a refused registration: {other:?}"),
        }
    }
}
