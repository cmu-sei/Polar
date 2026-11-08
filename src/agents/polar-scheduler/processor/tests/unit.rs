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

use cassini_client::TcpClientMessage;
use polar::graph::{
    controller::{
        GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property, compile_graph_op,
    },
    nodes::schedule::ScheduleKey,
};
use polar_scheduler::processor::ScheduleInfoProcessor;
use polar_scheduler::types::ProcessorMsg;
use polar_scheduler_common::GitScheduleChange;
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
        key: ScheduleKey::Configuration {
            id: "agent-1".to_string(),
        }
        .into_key(),
        props: vec![Property(
            "schedule".to_string(),
            GraphValue::String("{}".to_string()),
        )],
    };
    let [q] = &compile_graph_op(&op).unwrap()[..] else {
        panic!("one statement")
    };
    assert!(!q.query().is_empty());
    assert!(q.query().contains("MERGE"));
}

#[test]
fn test_compile_ensure_edge() {
    let op = GraphOp::EnsureEdge {
        from: ScheduleKey::Configuration {
            id: "a".to_string(),
        }
        .into_key(),
        to: ScheduleKey::ConfigurationOutcome {
            key: "b".to_string(),
        }
        .into_key(),
        rel_type: "DESCRIBES".to_string(),
        props: vec![],
    };
    let [q] = &compile_graph_op(&op).unwrap()[..] else {
        panic!("one statement")
    };
    assert!(q.query().contains("MERGE"));
    assert!(q.query().contains("DESCRIBES"));
}

#[test]
fn test_compile_replace_edge() {
    let op = GraphOp::ReplaceEdge {
        from: ScheduleKey::Configuration {
            id: "a".to_string(),
        }
        .into_key(),
        rel_type: "HAS_STATE".to_string(),
        to: ScheduleKey::ConfigurationOutcome {
            key: "b".to_string(),
        }
        .into_key(),
    };
    let [q] = &compile_graph_op(&op).unwrap()[..] else {
        panic!("one statement")
    };
    assert!(q.query().contains("HAS_STATE"));
}

#[test]
fn test_compile_remove_edges() {
    let op = GraphOp::RemoveEdges {
        from: ScheduleKey::Configuration {
            id: "a".to_string(),
        }
        .into_key(),
        rel_type: "HAS_STATE".to_string(),
    };
    let [q] = &compile_graph_op(&op).unwrap()[..] else {
        panic!("one statement")
    };
    assert!(q.query().contains("MATCH"));
    assert!(q.query().contains("DELETE"));
}

#[test]
fn test_compile_update_state() {
    let op = GraphOp::UpdateState {
        resource_key: ScheduleKey::Configuration {
            id: "a".to_string(),
        }
        .into_key(),
        state_type_key: ScheduleKey::ConfigurationOutcome {
            key: "state-type".to_string(),
        }
        .into_key(),
        state_instance_key: ScheduleKey::Configuration {
            id: "instance".to_string(),
        }
        .into_key(),
        state_instance_props: vec![Property(
            "status".to_string(),
            GraphValue::String("active".to_string()),
        )],
    };
    let [q] = &compile_graph_op(&op).unwrap()[..] else {
        panic!("one statement")
    };
    assert!(q.query().contains("MERGE"));
    assert!(q.query().contains("TRANSITIONED_TO"));
}

// ── ScheduleInfoProcessor actor tests ────────────────────────────────────────

async fn spawn_processor() -> (ActorRef<ProcessorMsg>, tokio::task::JoinHandle<()>) {
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
async fn test_processor_git_change_delete() {
    let (processor, handle) = spawn_processor().await;
    let change = GitScheduleChange::Delete {
        path: "permanent/test.dhall".to_string(),
    };
    processor
        .send_message(ProcessorMsg::GitChange(change))
        .unwrap();
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
