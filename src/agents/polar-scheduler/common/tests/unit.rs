use polar_scheduler_common::{
    ScheduleNode, ScheduleKind, ScheduleMetadata, TimeSpec, TimeUnit, Weekday,
    GitScheduleChange, AdhocAgentAnnouncement, ScheduleNotification,
};
use serde_json::json;
use rkyv::{to_bytes, from_bytes};

#[test]
fn test_serde_time_spec_periodic() {
    let spec = TimeSpec::Periodic {
        interval: 300,
        unit: TimeUnit::Minutes,
    };
    let json = serde_json::to_string(&spec).unwrap();
    let roundtrip: TimeSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec, roundtrip);
}

#[test]
fn test_serde_time_spec_exact() {
    let spec = TimeSpec::Exact {
        timestamp: "2026-03-25T10:00:00Z".to_string(),
    };
    let json = serde_json::to_string(&spec).unwrap();
    let roundtrip: TimeSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec, roundtrip);
}

#[test]
fn test_serde_time_spec_daily() {
    let spec = TimeSpec::Daily {
        hour: 9,
        minute: 30,
        timezone: Some("UTC".to_string()),
    };
    let json = serde_json::to_string(&spec).unwrap();
    let roundtrip: TimeSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec, roundtrip);
}

#[test]
fn test_serde_time_spec_daily_no_timezone() {
    let spec = TimeSpec::Daily {
        hour: 9,
        minute: 30,
        timezone: None,
    };
    let json = serde_json::to_string(&spec).unwrap();
    let roundtrip: TimeSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec, roundtrip);
}

#[test]
fn test_serde_time_spec_weekly() {
    let spec = TimeSpec::Weekly {
        day: Weekday::Mon,
        hour: 8,
        minute: 0,
        timezone: Some("America/New_York".to_string()),
    };
    let json = serde_json::to_string(&spec).unwrap();
    let roundtrip: TimeSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec, roundtrip);
}

#[test]
fn test_serde_time_spec_monthly() {
    let spec = TimeSpec::Monthly {
        day: 15,
        hour: 12,
        minute: 0,
        timezone: Some("UTC".to_string()),
    };
    let json = serde_json::to_string(&spec).unwrap();
    let roundtrip: TimeSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec, roundtrip);
}

#[test]
fn test_serde_time_unit() {
    let unit = TimeUnit::Minutes;
    let json = serde_json::to_string(&unit).unwrap();
    let roundtrip: TimeUnit = serde_json::from_str(&json).unwrap();
    assert_eq!(unit, roundtrip);
}

#[test]
fn test_serde_time_unit_hours() {
    let unit = TimeUnit::Hours;
    let json = serde_json::to_string(&unit).unwrap();
    let roundtrip: TimeUnit = serde_json::from_str(&json).unwrap();
    assert_eq!(unit, roundtrip);
}

#[test]
fn test_serde_time_unit_days() {
    let unit = TimeUnit::Days;
    let json = serde_json::to_string(&unit).unwrap();
    let roundtrip: TimeUnit = serde_json::from_str(&json).unwrap();
    assert_eq!(unit, roundtrip);
}

#[test]
fn test_serde_weekday_mon() {
    let day = Weekday::Mon;
    let json = serde_json::to_string(&day).unwrap();
    let roundtrip: Weekday = serde_json::from_str(&json).unwrap();
    assert_eq!(day, roundtrip);
}

#[test]
fn test_serde_weekday_all() {
    let days = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    for day in days {
        let json = serde_json::to_string(&day).unwrap();
        let roundtrip: Weekday = serde_json::from_str(&json).unwrap();
        assert_eq!(day, roundtrip);
    }
}

#[test]
fn test_serde_schedule_kind_permanent() {
    let kind = ScheduleKind::Permanent;
    let json = serde_json::to_string(&kind).unwrap();
    let roundtrip: ScheduleKind = serde_json::from_str(&json).unwrap();
    assert_eq!(kind, roundtrip);
}

#[test]
fn test_serde_schedule_kind_adhoc() {
    let kind = ScheduleKind::Adhoc;
    let json = serde_json::to_string(&kind).unwrap();
    let roundtrip: ScheduleKind = serde_json::from_str(&json).unwrap();
    assert_eq!(kind, roundtrip);
}

#[test]
fn test_serde_schedule_kind_ephemeral() {
    let kind = ScheduleKind::Ephemeral;
    let json = serde_json::to_string(&kind).unwrap();
    let roundtrip: ScheduleKind = serde_json::from_str(&json).unwrap();
    assert_eq!(kind, roundtrip);
}

#[test]
fn test_serde_schedule_metadata() {
    let metadata = ScheduleMetadata {
        description: Some("Test schedule".to_string()),
        owner: Some("owner@example.com".to_string()),
    };
    let json = serde_json::to_string(&metadata).unwrap();
    let roundtrip: ScheduleMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(metadata, roundtrip);
}

#[test]
fn test_serde_schedule_metadata_none() {
    let metadata = ScheduleMetadata {
        description: None,
        owner: None,
    };
    let json = serde_json::to_string(&metadata).unwrap();
    let roundtrip: ScheduleMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(metadata, roundtrip);
}

#[test]
fn test_serde_schedule_node_full() {
    let node = ScheduleNode {
        id: "node-1".to_string(),
        kind: ScheduleKind::Permanent,
        agent_id: Some("agent-1".to_string()),
        agent_type: Some("worker".to_string()),
        schedule: TimeSpec::Periodic {
            interval: 60,
            unit: TimeUnit::Minutes,
        },
        config: json!({"key": "value"}),
        metadata: ScheduleMetadata {
            description: Some("A permanent schedule".to_string()),
            owner: Some("admin".to_string()),
        },
        version: 1,
    };
    let json = serde_json::to_string(&node).unwrap();
    let roundtrip: ScheduleNode = serde_json::from_str(&json).unwrap();
    assert_eq!(node, roundtrip);
}

#[test]
fn test_serde_schedule_node_minimal() {
    let node = ScheduleNode {
        id: "node-2".to_string(),
        kind: ScheduleKind::Adhoc,
        agent_id: None,
        agent_type: None,
        schedule: TimeSpec::Exact {
            timestamp: "2026-03-25T12:00:00Z".to_string(),
        },
        config: json!({}),
        metadata: ScheduleMetadata {
            description: None,
            owner: None,
        },
        version: 0,
    };
    let json = serde_json::to_string(&node).unwrap();
    let roundtrip: ScheduleNode = serde_json::from_str(&json).unwrap();
    assert_eq!(node, roundtrip);
}

#[test]
fn test_serde_git_schedule_change_create() {
    let change = GitScheduleChange::Create {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "1"}"#.to_string(),
    };
    let json = serde_json::to_string(&change).unwrap();
    let roundtrip: GitScheduleChange = serde_json::from_str(&json).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_serde_git_schedule_change_update() {
    let change = GitScheduleChange::Update {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "2"}"#.to_string(),
    };
    let json = serde_json::to_string(&change).unwrap();
    let roundtrip: GitScheduleChange = serde_json::from_str(&json).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_serde_git_schedule_change_delete() {
    let change = GitScheduleChange::Delete {
        path: "schedules/worker.yaml".to_string(),
    };
    let json = serde_json::to_string(&change).unwrap();
    let roundtrip: GitScheduleChange = serde_json::from_str(&json).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_serde_adhoc_agent_announcement() {
    let announcement = AdhocAgentAnnouncement {
        agent_type: "worker".to_string(),
        version: 1,
        default_schedule_json: r#"{"id": "1"}"#.to_string(),
    };
    let json = serde_json::to_string(&announcement).unwrap();
    let roundtrip: AdhocAgentAnnouncement = serde_json::from_str(&json).unwrap();
    assert_eq!(announcement, roundtrip);
}

#[test]
fn test_serde_schedule_notification_permanent() {
    let notification = ScheduleNotification::PermanentUpdate {
        agent_id: "agent-1".to_string(),
        schedule_json: r#"{"id": "1"}"#.to_string(),
    };
    let json = serde_json::to_string(&notification).unwrap();
    let roundtrip: ScheduleNotification = serde_json::from_str(&json).unwrap();
    assert_eq!(notification, roundtrip);
}

#[test]
fn test_serde_schedule_notification_adhoc() {
    let notification = ScheduleNotification::AdhocUpdate {
        agent_type: "worker".to_string(),
        schedule_json: r#"{"id": "2"}"#.to_string(),
    };
    let json = serde_json::to_string(&notification).unwrap();
    let roundtrip: ScheduleNotification = serde_json::from_str(&json).unwrap();
    assert_eq!(notification, roundtrip);
}

#[test]
fn test_serde_schedule_notification_ephemeral() {
    let notification = ScheduleNotification::EphemeralUpdate {
        agent_type: "worker".to_string(),
        schedule_json: r#"{"id": "3"}"#.to_string(),
    };
    let json = serde_json::to_string(&notification).unwrap();
    let roundtrip: ScheduleNotification = serde_json::from_str(&json).unwrap();
    assert_eq!(notification, roundtrip);
}

#[test]
fn test_rkyv_git_schedule_change_create() {
    let change = GitScheduleChange::Create {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "1"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&change).unwrap();
    let roundtrip: GitScheduleChange = from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_rkyv_git_schedule_change_update() {
    let change = GitScheduleChange::Update {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "2"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&change).unwrap();
    let roundtrip: GitScheduleChange = from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_rkyv_git_schedule_change_delete() {
    let change = GitScheduleChange::Delete {
        path: "schedules/worker.yaml".to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&change).unwrap();
    let roundtrip: GitScheduleChange = from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_rkyv_adhoc_agent_announcement() {
    let announcement = AdhocAgentAnnouncement {
        agent_type: "worker".to_string(),
        version: 1,
        default_schedule_json: r#"{"id": "1"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&announcement).unwrap();
    let roundtrip: AdhocAgentAnnouncement = from_bytes::<AdhocAgentAnnouncement, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(announcement, roundtrip);
}

#[test]
fn test_rkyv_schedule_notification_permanent() {
    let notification = ScheduleNotification::PermanentUpdate {
        agent_id: "agent-1".to_string(),
        schedule_json: r#"{"id": "1"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&notification).unwrap();
    let roundtrip: ScheduleNotification = from_bytes::<ScheduleNotification, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(notification, roundtrip);
}

#[test]
fn test_rkyv_schedule_notification_adhoc() {
    let notification = ScheduleNotification::AdhocUpdate {
        agent_type: "worker".to_string(),
        schedule_json: r#"{"id": "2"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&notification).unwrap();
    let roundtrip: ScheduleNotification = from_bytes::<ScheduleNotification, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(notification, roundtrip);
}

#[test]
fn test_rkyv_schedule_notification_ephemeral() {
    let notification = ScheduleNotification::EphemeralUpdate {
        agent_type: "worker".to_string(),
        schedule_json: r#"{"id": "3"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&notification).unwrap();
    let roundtrip: ScheduleNotification = from_bytes::<ScheduleNotification, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(notification, roundtrip);
}
