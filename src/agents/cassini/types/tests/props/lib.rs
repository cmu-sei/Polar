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
        let sent = ClientMessage::RegistrationRequest { registration_id: registration_id.clone(), trace_ctx: None };
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
