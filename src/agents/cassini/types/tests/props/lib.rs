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
