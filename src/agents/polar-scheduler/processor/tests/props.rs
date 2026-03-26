use polar_scheduler_common::{AdhocAgentAnnouncement, GitScheduleChange};
use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_git_change_create_rkyv_roundtrip(
        path in ".+",
        json in ".*"
    ) {
        let original = GitScheduleChange::Create {
            path: path.clone(),
            json: json.clone(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let decoded = rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
        prop_assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&decoded).unwrap()
        );
    }

    #[test]
    fn prop_git_change_delete_rkyv_roundtrip(path in ".+") {
        let original = GitScheduleChange::Delete { path };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let decoded = rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
        prop_assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&decoded).unwrap()
        );
    }

    #[test]
    fn prop_adhoc_announcement_rkyv_roundtrip(
        agent_type in ".+",
        json in ".*"
    ) {
        let original = AdhocAgentAnnouncement {
            agent_type,
            version: 1,
            default_schedule_json: json,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let decoded = rkyv::from_bytes::<AdhocAgentAnnouncement, rkyv::rancor::Error>(&bytes).unwrap();
        prop_assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&decoded).unwrap()
        );
    }
}
