//! The processor and signed configuration documents: what it writes to the graph and what it forwards.

use cassini_client::TcpClientMessage;
use polar::graph::controller::{GraphControllerMsg, GraphOp, GraphValue};
use polar_scheduler::processor::ScheduleInfoProcessor;
use polar_scheduler::types::ProcessorMsg;
use polar_scheduler_common::{ConfigurationNotification, GitScheduleChange, configuration_topic};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a graph write was: the node it is for, and its properties by name.
#[derive(Debug, Clone)]
struct Written {
    node: String,
    props: Vec<(String, GraphValue)>,
}

impl Written {
    fn text(&self, name: &str) -> Option<String> {
        self.props.iter().find_map(|(n, v)| match v {
            GraphValue::String(s) if n == name => Some(s.clone()),
            _ => None,
        })
    }

    fn number(&self, name: &str) -> Option<i64> {
        self.props.iter().find_map(|(n, v)| match v {
            GraphValue::I64(i) if n == name => Some(*i),
            _ => None,
        })
    }
}

struct Graph(Arc<Mutex<Vec<Written>>>);

#[ractor::async_trait]
impl Actor for Graph {
    type Msg = GraphControllerMsg;
    type State = Arc<Mutex<Vec<Written>>>;
    type Arguments = ();
    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(self.0.clone())
    }
    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let GraphControllerMsg::Op(GraphOp::UpsertNode { key, props }) = message {
            let (node, params) = key.cypher_match("n");
            let node = format!("{node} {params:?}");
            state.lock().unwrap().push(Written {
                node,
                props: props.into_iter().map(|p| (p.0, p.1)).collect(),
            });
        }
        Ok(())
    }
}

/// What was published: the topic and the payload.
type Published = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

struct Cassini(Published);

#[ractor::async_trait]
impl Actor for Cassini {
    type Msg = TcpClientMessage;
    type State = Published;
    type Arguments = ();
    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(self.0.clone())
    }
    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let TcpClientMessage::Publish { topic, payload, .. } = message {
            state.lock().unwrap().push((topic, payload));
        }
        Ok(())
    }
}

struct Rig {
    processor: ActorRef<ProcessorMsg>,
    graph: Arc<Mutex<Vec<Written>>>,
    published: Published,
}

impl Rig {
    async fn new() -> Self {
        let graph = Arc::new(Mutex::new(Vec::new()));
        let published = Arc::new(Mutex::new(Vec::new()));
        let (cassini, _) = Actor::spawn(None, Cassini(published.clone()), ())
            .await
            .unwrap();
        let (graph_actor, _) = Actor::spawn(None, Graph(graph.clone()), ()).await.unwrap();
        let (processor, _) = Actor::spawn(None, ScheduleInfoProcessor, (cassini, graph_actor))
            .await
            .unwrap();
        Self {
            processor,
            graph,
            published,
        }
    }

    async fn change(&self, change: GitScheduleChange) {
        self.processor
            .cast(ProcessorMsg::GitChange(change))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    async fn create(&self, path: &str, document: &Value) {
        self.change(GitScheduleChange::Create {
            path: path.to_string(),
            json: document.to_string(),
        })
        .await
    }

    fn graph(&self) -> Vec<Written> {
        self.graph.lock().unwrap().clone()
    }

    fn published(&self) -> Vec<(String, Vec<u8>)> {
        self.published.lock().unwrap().clone()
    }
}

fn document(id: &str, agent: &str, version: u64) -> Value {
    json!({
        "payload": {
            "id": id,
            "target": {"agent": agent},
            "version": version,
            "changes": [{"name": "HIGH", "value": "20"}]
        },
        "signature": {"algorithm": "ed25519", "value": "AAAA", "certificate_chain": ["x"]}
    })
}

fn decode(payload: &[u8]) -> ConfigurationNotification {
    rkyv::from_bytes::<ConfigurationNotification, rkyv::rancor::Error>(payload).unwrap()
}

#[tokio::test]
async fn a_signed_document_is_written_to_the_graph_and_forwarded_untouched_to_its_agent() {
    let rig = Rig::new().await;
    let doc = document("config/git-agent", "git-agent", 3);
    rig.create("permanent/git-agent.dhall", &doc).await;

    let written = rig.graph();
    assert_eq!(written.len(), 1);
    assert!(
        written[0].node.contains("Schedule:Configuration"),
        "{}",
        written[0].node
    );
    assert!(
        written[0].node.contains("config/git-agent"),
        "{}",
        written[0].node
    );
    assert_eq!(written[0].text("status").as_deref(), Some("received"));
    assert_eq!(written[0].text("target").as_deref(), Some("git-agent"));
    assert_eq!(written[0].number("version"), Some(3));
    assert_eq!(
        written[0].text("source_path").as_deref(),
        Some("permanent/git-agent.dhall")
    );
    assert!(written[0].text("received_at").is_some());
    // The document is stored as it was received.
    let stored: Value = serde_json::from_str(&written[0].text("document").unwrap()).unwrap();
    assert_eq!(stored, doc);

    let published = rig.published();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].0, configuration_topic("git-agent"));
    assert_eq!(published[0].0, "agent.git-agent.configuration");
    let notification = decode(&published[0].1);
    assert_eq!(notification.agent, "git-agent");
    let forwarded: Value = serde_json::from_str(&notification.document_json).unwrap();
    assert_eq!(
        forwarded, doc,
        "the signature and everything else, untouched"
    );
}

#[tokio::test]
async fn a_document_that_is_not_newer_is_recorded_but_not_forwarded_again() {
    let rig = Rig::new().await;
    rig.create("a.dhall", &document("c", "agent-a", 2)).await;
    rig.create("a.dhall", &document("c", "agent-a", 2)).await; // the same again
    rig.create("a.dhall", &document("c", "agent-a", 1)).await; // an older one
    assert_eq!(
        rig.graph().len(),
        3,
        "every event is observable in the graph"
    );
    assert_eq!(rig.published().len(), 1, "only the first is forwarded");

    rig.create("a.dhall", &document("c", "agent-a", 3)).await;
    assert_eq!(rig.published().len(), 2);
    assert_eq!(rig.graph().len(), 4);
}

#[tokio::test]
async fn items_are_followed_separately_and_each_goes_to_its_own_agent() {
    let rig = Rig::new().await;
    rig.create("a.dhall", &document("one", "agent-a", 1)).await;
    rig.create("b.dhall", &document("two", "agent-b", 1)).await;
    let topics: Vec<String> = rig.published().into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        topics,
        ["agent.agent-a.configuration", "agent.agent-b.configuration"]
    );
}

#[tokio::test]
async fn a_document_that_cannot_be_routed_is_written_to_the_graph_and_goes_nowhere() {
    let rig = Rig::new().await;
    let doc = json!({"payload": {"id": "c", "version": 1}, "signature": {}});
    rig.create("permanent/broken.dhall", &doc).await;

    let written = rig.graph();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].text("status").as_deref(), Some("unroutable"));
    assert!(written[0].text("reason").unwrap().contains("target.agent"));
    assert!(
        written[0]
            .node
            .contains("unroutable:permanent/broken.dhall")
    );
    assert!(rig.published().is_empty());
}

#[tokio::test]
async fn an_unsigned_schedule_still_takes_the_old_path() {
    let rig = Rig::new().await;
    let node = json!({
        "id": "permanent/x", "kind": "Permanent", "agent_id": "x", "agent_type": null,
        "schedule": {"Periodic": {"interval": 5, "unit": "Minutes"}},
        "config": {}, "metadata": {"description": null, "owner": null}, "version": 1
    });
    rig.create("permanent/x.dhall", &node).await;

    let written = rig.graph();
    assert_eq!(written.len(), 1);
    assert!(
        written[0].node.contains("Schedule:Permanent"),
        "{}",
        written[0].node
    );
    let published = rig.published();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].0, "agent.x.schedule");
}
