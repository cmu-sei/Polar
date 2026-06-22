use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    #[test]
    fn payload_survives_arc_wrap(bytes in any::<Vec<u8>>()) {
        let payload = Arc::new(bytes.clone());
        prop_assert_eq!(payload.as_ref(), &bytes);
        prop_assert_eq!(payload.len(), bytes.len());
    }

    #[test]
    fn empty_payload_arc(topic in "[a-z]{1,20}") {
        let payload: Arc<Vec<u8>> = Arc::new(vec![]);
        prop_assert_eq!(payload.len(), 0);
        prop_assert!(topic.len() >= 1 && topic.len() <= 20);
    }
}
