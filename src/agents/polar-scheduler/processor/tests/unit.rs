use cassini_client::TcpClientMessage;
use polar::graph::{
    controller::{compile_graph_op, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property},
    nodes::schedule::ScheduleKey,
};
use polar_scheduler::processor::ScheduleInfoProcessor;
use polar_scheduler::types::ProcessorMsg;
use polar_scheduler_common::{AdhocAgentAnnouncement, GitScheduleChange};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::time::Duration;

// ── Mock actors ───────────────────────────────────────────────────────────────

struct MockTcpClient;

#[ractor::async_trait]
impl Actor for MockTcpClient {
    type Msg = TcpClientMessage;
    type State = ();
    type Arguments = ();
    async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

struct MockGraphController;

#[ractor::async_trait]
impl Actor for MockGraphController {
    type Msg = GraphControllerMsg;
    type State = ();
    type Arguments = ();
    async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

// ── compile_graph_op tests ────────────────────────────────────────────────────

#[test]
fn test_compile_upsert_node() {
    let op = GraphOp::UpsertNode {
        key: ScheduleKey::Permanent { agent_id: "agent-1".to_string() }.into_key(),
        props: vec![Property("schedule".to_string(), GraphValue::String("{}".to_string()))],
    };
    let q = compile_graph_op(&op);
    assert!(!q.query().is_empty());
    assert!(q.query().contains("MERGE"));
}

#[test]
fn test_compile_upsert_node_adhoc() {
    let op = GraphOp::UpsertNode {
        key: ScheduleKey::Adhoc { agent_type: "worker".to_string() }.into_key(),
        props: vec![],
    };
    let q = compile_graph_op(&op);
    assert!(q.query().contains("MERGE"));
    assert!(q.query().contains("Adhoc"));
}

#[test]
fn test_compile_upsert_node_ephemeral() {
    let op = GraphOp::UpsertNode {
        key: ScheduleKey::Ephemeral { agent_type: "handler".to_string() }.into_key(),
        props: vec![],
    };
    let q = compile_graph_op(&op);
    assert!(q.query().contains("MERGE"));
    assert!(q.query().contains("Ephemeral"));
}

#[test]
fn test_compile_ensure_edge() {
    let op = GraphOp::EnsureEdge {
        from: ScheduleKey::Permanent { agent_id: "a".to_string() }.into_key(),
        to: ScheduleKey::Adhoc { agent_type: "b".to_string() }.into_key(),
        rel_type: "DESCRIBES".to_string(),
        props: vec![],
    };
    let q = compile_graph_op(&op);
    assert!(q.query().contains("MERGE"));
    assert!(q.query().contains("DESCRIBES"));
}

#[test]
fn test_compile_replace_edge() {
    let op = GraphOp::ReplaceEdge {
        from: ScheduleKey::Permanent { agent_id: "a".to_string() }.into_key(),
        rel_type: "HAS_STATE".to_string(),
        to: ScheduleKey::Adhoc { agent_type: "b".to_string() }.into_key(),
    };
    let q = compile_graph_op(&op);
    assert!(q.query().contains("HAS_STATE"));
}

#[test]
fn test_compile_remove_edges() {
    let op = GraphOp::RemoveEdges {
        from: ScheduleKey::Permanent { agent_id: "a".to_string() }.into_key(),
        rel_type: "HAS_STATE".to_string(),
    };
    let q = compile_graph_op(&op);
    assert!(q.query().contains("MATCH"));
    assert!(q.query().contains("DELETE"));
}

#[test]
fn test_compile_update_state() {
    let op = GraphOp::UpdateState {
        resource_key: ScheduleKey::Permanent { agent_id: "a".to_string() }.into_key(),
        state_type_key: ScheduleKey::Adhoc { agent_type: "state-type".to_string() }.into_key(),
        state_instance_key: ScheduleKey::Ephemeral { agent_type: "instance".to_string() }.into_key(),
        state_instance_props: vec![Property("status".to_string(), GraphValue::String("active".to_string()))],
    };
    let q = compile_graph_op(&op);
    assert!(q.query().contains("MERGE"));
    assert!(q.query().contains("TRANSITIONED_TO"));
}

// ── ScheduleInfoProcessor actor tests ────────────────────────────────────────

async fn spawn_processor() -> (
    ActorRef<ProcessorMsg>,
    tokio::task::JoinHandle<()>,
) {
    let (tcp, _) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (graph, _) = Actor::spawn(None, MockGraphController, ()).await.unwrap();
    let (processor, handle) = Actor::spawn(None, ScheduleInfoProcessor, (tcp, graph))
        .await
        .unwrap();
    (processor, handle)
}

#[tokio::test]
async fn test_processor_spawns() {
    let (processor, handle) = spawn_processor().await;
    processor.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_processor_git_change_create() {
    let (processor, handle) = spawn_processor().await;

    let schedule_json = serde_json::json!({
        "id": "test-schedule",
        "kind": "Permanent",
        "agent_id": "test-agent",
        "agent_type": null,
        "schedule": {"Periodic": {"interval": 10, "unit": "Minutes"}},
        "config": {},
        "metadata": {"description": null, "owner": null},
        "version": 1
    }).to_string();

    let change = GitScheduleChange::Create {
        path: "permanent/test.dhall".to_string(),
        json: schedule_json,
    };
    processor.send_message(ProcessorMsg::GitChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    processor.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_processor_git_change_update() {
    let (processor, handle) = spawn_processor().await;

    let schedule_json = serde_json::json!({
        "id": "test-schedule",
        "kind": "Adhoc",
        "agent_id": null,
        "agent_type": "worker",
        "schedule": {"Periodic": {"interval": 5, "unit": "Minutes"}},
        "config": {},
        "metadata": {"description": null, "owner": null},
        "version": 2
    }).to_string();

    let change = GitScheduleChange::Update {
        path: "adhoc/worker.dhall".to_string(),
        json: schedule_json,
    };
    processor.send_message(ProcessorMsg::GitChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    processor.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_processor_git_change_delete() {
    let (processor, handle) = spawn_processor().await;
    let change = GitScheduleChange::Delete {
        path: "permanent/test.dhall".to_string(),
    };
    processor.send_message(ProcessorMsg::GitChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    processor.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_processor_announcement() {
    let (processor, handle) = spawn_processor().await;

    let default_schedule_json = serde_json::json!({
        "id": "adhoc/worker",
        "kind": "Adhoc",
        "agent_id": null,
        "agent_type": "worker",
        "schedule": {"Periodic": {"interval": 60, "unit": "Minutes"}},
        "config": {},
        "metadata": {"description": null, "owner": null},
        "version": 1
    }).to_string();

    let ann = AdhocAgentAnnouncement {
        agent_type: "worker".to_string(),
        version: 1,
        default_schedule_json,
    };
    processor.send_message(ProcessorMsg::Announcement(ann)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    processor.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_processor_event() {
    let (processor, handle) = spawn_processor().await;
    processor.send_message(ProcessorMsg::Event {
        topic: "events.user.updated".to_string(),
        payload: vec![],
    }).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    processor.stop(None);
    handle.await.unwrap();
}

// ── rkyv roundtrip tests ──────────────────────────────────────────────────────

#[test]
fn test_git_schedule_change_create_rkyv() {
    let original = GitScheduleChange::Create {
        path: "permanent/test.dhall".to_string(),
        json: "{}".to_string(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
    let decoded = rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn test_git_schedule_change_update_rkyv() {
    let original = GitScheduleChange::Update {
        path: "adhoc/worker.dhall".to_string(),
        json: r#"{"id":"worker"}"#.to_string(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
    let decoded = rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn test_git_schedule_change_delete_rkyv() {
    let original = GitScheduleChange::Delete {
        path: "permanent/gone.dhall".to_string(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
    let decoded = rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn test_adhoc_announcement_rkyv() {
    let original = AdhocAgentAnnouncement {
        agent_type: "worker".to_string(),
        version: 1,
        default_schedule_json: "{}".to_string(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
    let decoded = rkyv::from_bytes::<AdhocAgentAnnouncement, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(original, decoded);
}
