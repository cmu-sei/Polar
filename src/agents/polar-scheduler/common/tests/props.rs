use proptest::prelude::*;
use polar_scheduler_common::{
    ScheduleNode, ScheduleKind, ScheduleMetadata, TimeSpec, TimeUnit,
    GitScheduleChange, AdhocAgentAnnouncement, ScheduleNotification,
};
use serde_json::json;

proptest! {
    // Test that agent_id survives serialization roundtrip for any string
    #[test]
    fn agent_id_survives_arc_wrap(agent_id in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let node = ScheduleNode {
            id: "test-node".to_string(),
            kind: ScheduleKind::Permanent,
            agent_id: Some(agent_id.clone()),
            agent_type: None,
            schedule: TimeSpec::Periodic {
                interval: 60,
                unit: TimeUnit::Minutes,
            },
            config: serde_json::Value::Null,
            metadata: ScheduleMetadata {
                description: None,
                owner: None,
            },
            version: 1,
        };
        let json = serde_json::to_string(&node).unwrap();
        let roundtrip: ScheduleNode = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.agent_id, Some(agent_id));
    }

    // Test that agent_type survives serialization roundtrip for any string
    #[test]
    fn agent_type_survives_arc_wrap(agent_type in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let node = ScheduleNode {
            id: "test-node".to_string(),
            kind: ScheduleKind::Permanent,
            agent_id: None,
            agent_type: Some(agent_type.clone()),
            schedule: TimeSpec::Periodic {
                interval: 60,
                unit: TimeUnit::Minutes,
            },
            config: serde_json::Value::Null,
            metadata: ScheduleMetadata {
                description: None,
                owner: None,
            },
            version: 1,
        };
        let json = serde_json::to_string(&node).unwrap();
        let roundtrip: ScheduleNode = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.agent_type, Some(agent_type));
    }

    // Test that topic strings survive serialization roundtrip
    #[test]
    fn topic_survives_arc_wrap(topic in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let node = ScheduleNode {
            id: "test-node".to_string(),
            kind: ScheduleKind::Permanent,
            agent_id: None,
            agent_type: Some("worker".to_string()),
            schedule: TimeSpec::Periodic {
                interval: 60,
                unit: TimeUnit::Minutes,
            },
            config: json!({"topic": topic}),
            metadata: ScheduleMetadata {
                description: None,
                owner: None,
            },
            version: 1,
        };
        let json = serde_json::to_string(&node).unwrap();
        let roundtrip: ScheduleNode = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.config, json!({"topic": topic}));
    }

    // Test that timezone strings survive serialization roundtrip
    #[test]
    fn timezone_survives_arc_wrap(timezone in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let spec = TimeSpec::Daily {
            hour: 9,
            minute: 30,
            timezone: Some(timezone.clone()),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let roundtrip: TimeSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, TimeSpec::Daily {
            hour: 9,
            minute: 30,
            timezone: Some(timezone),
        });
    }

    // Test that schedule_json strings survive serialization roundtrip
    #[test]
    fn schedule_json_survives_arc_wrap(schedule_json in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let notification = ScheduleNotification::PermanentUpdate {
            agent_id: "agent-1".to_string(),
            schedule_json: schedule_json.clone(),
        };
        let json = serde_json::to_string(&notification).unwrap();
        let roundtrip: ScheduleNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, ScheduleNotification::PermanentUpdate {
            agent_id: "agent-1".to_string(),
            schedule_json,
        });
    }

    // Test that path strings survive serialization roundtrip
    #[test]
    fn path_survives_arc_wrap(path in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let change = GitScheduleChange::Create {
            path: path.clone(),
            json: r#"{}"#.to_string(),
        };
        let json = serde_json::to_string(&change).unwrap();
        let roundtrip: GitScheduleChange = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, GitScheduleChange::Create {
            path,
            json: r#"{}"#.to_string(),
        });
    }

    // Test that default_schedule_json strings survive serialization roundtrip
    #[test]
    fn default_schedule_json_survives_arc_wrap(default_schedule_json in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let announcement = AdhocAgentAnnouncement {
            agent_type: "worker".to_string(),
            version: 1,
            default_schedule_json: default_schedule_json.clone(),
        };
        let json = serde_json::to_string(&announcement).unwrap();
        let roundtrip: AdhocAgentAnnouncement = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, AdhocAgentAnnouncement {
            agent_type: "worker".to_string(),
            version: 1,
            default_schedule_json,
        });
    }

    // Test that description strings survive serialization roundtrip
    #[test]
    fn description_survives_arc_wrap(description in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let metadata = ScheduleMetadata {
            description: Some(description.clone()),
            owner: None,
        };
        let json = serde_json::to_string(&metadata).unwrap();
        let roundtrip: ScheduleMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, ScheduleMetadata {
            description: Some(description),
            owner: None,
        });
    }

    // Test that owner strings survive serialization roundtrip
    #[test]
    fn owner_survives_arc_wrap(owner in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let metadata = ScheduleMetadata {
            description: None,
            owner: Some(owner.clone()),
        };
        let json = serde_json::to_string(&metadata).unwrap();
        let roundtrip: ScheduleMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, ScheduleMetadata {
            description: None,
            owner: Some(owner),
        });
    }

    // Test that node id strings survive serialization roundtrip
    #[test]
    fn node_id_survives_arc_wrap(node_id in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let node = ScheduleNode {
            id: node_id.clone(),
            kind: ScheduleKind::Permanent,
            agent_id: None,
            agent_type: None,
            schedule: TimeSpec::Periodic {
                interval: 60,
                unit: TimeUnit::Minutes,
            },
            config: serde_json::Value::Null,
            metadata: ScheduleMetadata {
                description: None,
                owner: None,
            },
            version: 1,
        };
        let json = serde_json::to_string(&node).unwrap();
        let roundtrip: ScheduleNode = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.id, node_id);
    }

    // Test that agent_type in notifications survives serialization roundtrip
    #[test]
    fn notification_agent_type_survives_arc_wrap(agent_type in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let notification = ScheduleNotification::AdhocUpdate {
            agent_type: agent_type.clone(),
            schedule_json: r#"{}"#.to_string(),
        };
        let json = serde_json::to_string(&notification).unwrap();
        let roundtrip: ScheduleNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, ScheduleNotification::AdhocUpdate {
            agent_type,
            schedule_json: r#"{}"#.to_string(),
        });
    }

    // Test that agent_type in ephemeral notifications survives serialization roundtrip
    #[test]
    fn ephemeral_notification_agent_type_survives_arc_wrap(agent_type in "[^any::<String>().filter(|s| !s.is_empty())]+") {
        let notification = ScheduleNotification::EphemeralUpdate {
            agent_type: agent_type.clone(),
            schedule_json: r#"{}"#.to_string(),
        };
        let json = serde_json::to_string(&notification).unwrap();
        let roundtrip: ScheduleNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, ScheduleNotification::EphemeralUpdate {
            agent_type,
            schedule_json: r#"{}"#.to_string(),
        });
    }
}
