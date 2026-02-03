use neo4rs::{BoltNull, BoltType, Graph, Query};
use ractor::ActorProcessingErr;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tracing::{debug, instrument, trace};

/// Graph relationship type constants.
/// ------ IMPORTANT!!!!! ------
/// These must stay in sync with the Neo4j schema.
/// Treat changes here as breaking schema changes.
/// ------ IMPORTANT!!!!! ------
pub mod rel {
    pub const IS: &str = "IS";
    pub const INSTANCE_OF: &str = "INSTANCE_OF";
    pub const CONTAINS: &str = "CONTAINS";
    /// Pod -> PodContainer
    pub const HAS_CONTAINER: &str = "HAS_CONTAINER";

    /// Pod -> Volume
    pub const USES_VOLUME: &str = "USES_VOLUME";

    /// PodContainer -> ContainerImageReference
    pub const USES_IMAGE: &str = "USES_IMAGE";

    /// ContainerImageReference -> OCIArtifact
    pub const POINTS_TO: &str = "POINTS_TO";

    /// OCIArtifact -> OCIRegistry
    pub const HOSTED_BY: &str = "HOSTED_BY";

    /// Volume -> ConfigMap
    pub const BACKED_BY_CONFIG_MAP: &str = "BACKED_BY";

    /// Volume -> Secret
    pub const BACKED_BY_SECRET: &str = "BACKED_BY";

    pub const DESCRIBES: &str = "DESCRIBES";
}

pub trait GraphNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GraphValue {
    String(String),
    Bool(bool),
    I64(i64),
    F64(f64),

    Bytes(Vec<u8>),

    List(Vec<GraphValue>),
    Map(Vec<(String, GraphValue)>),

    Null,
}

impl From<GraphValue> for BoltType {
    fn from(v: GraphValue) -> Self {
        match v {
            GraphValue::String(s) => s.into(),
            GraphValue::Bool(b) => b.into(),
            GraphValue::I64(i) => i.into(),
            GraphValue::F64(f) => f.into(),
            GraphValue::Bytes(b) => b.into(),
            GraphValue::List(xs) => xs
                .into_iter()
                .map(BoltType::from)
                .collect::<Vec<_>>()
                .into(),
            GraphValue::Map(kvs) => kvs
                .into_iter()
                .map(|(k, v)| (k, BoltType::from(v)))
                .collect::<std::collections::HashMap<_, _>>()
                .into(),
            GraphValue::Null => BoltType::Null(BoltNull),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Property(pub String, pub GraphValue);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GraphOp<K>
where
    K: GraphNodeKey,
{
    /// Upsert a canonical node with properties.
    UpsertNode { key: K, props: Vec<Property> },

    /// Ensure a directed relationship exists between two canonical nodes.
    EnsureEdge {
        from: K,
        to: K,
        rel_type: String,
        props: Vec<Property>,
    },
}

pub struct GraphControllerState {
    pub graph: Graph,
}

/// Message wrapper for GraphController
/// Actors are instantiated once. Their mailboxes are typed once.
/// So every instantiation of an actor cannot claim: “this actor is generic over some future K that messages will decide later”.
/// Each ractor actor must take in a concrete message type. So we must instead instantiate each actor and claim:
/// “The graph controller understands a fixed vocabulary of node identities”
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphControllerMsg<K>
where
    K: GraphNodeKey + Debug,
{
    Op(GraphOp<K>),
}

/// Compile GraphOp to Cypher string and Bolt parameters.
/// Pure and deterministic.
pub fn compile_graph_op<K>(op: &GraphOp<K>) -> Query
where
    K: GraphNodeKey + Debug,
{
    let (cypher, params) = match op {
        GraphOp::UpsertNode { key, props } => {
            trace!("Received UpsertNode directive. {key:?}, {props:?}");
            let (node_pattern, mut params) = key.cypher_match("n");

            let mut cypher = format!(
                "MERGE (n {})",
                node_pattern.trim_start_matches('(').trim_end_matches(')')
            );

            if !props.is_empty() {
                let sets = props
                    .iter()
                    .map(|Property(k, _)| format!("n.{k} = ${k}"))
                    .collect::<Vec<_>>()
                    .join(", ");

                cypher.push_str(&format!("\nSET {sets}"));
            }

            for Property(k, v) in props {
                params.push((k.clone(), v.clone().into()));
            }

            (cypher, params)
        }

        GraphOp::EnsureEdge {
            from,
            to,
            rel_type,
            props,
        } => {
            trace!("Received EnsureEdge directive {to:?} {rel_type} {props:?}");
            let (from_pat, mut params) = from.cypher_match("from");
            let (to_pat, mut to_params) = to.cypher_match("to");
            params.append(&mut to_params);

            let mut cypher = format!(
                "MERGE (a {})\nMERGE (b {})\nMERGE (a)-[r:{}]->(b)",
                from_pat.trim_start_matches('(').trim_end_matches(')'),
                to_pat.trim_start_matches('(').trim_end_matches(')'),
                rel_type
            );

            if !props.is_empty() {
                let sets = props
                    .iter()
                    .map(|Property(k, _)| format!("r.{k} = ${k}"))
                    .collect::<Vec<_>>()
                    .join(", ");

                cypher.push_str(&format!("\nSET {sets}"));
            }

            for Property(k, v) in props {
                params.push((k.clone(), v.clone().into()));
            }

            (cypher, params)
        }
    };

    let mut q = Query::new(cypher);
    for (k, v) in params {
        q = q.param(&k, v);
    }

    q
}

/// helper fn intedned for handling generic graph operations.
/// Should be typed with some sort of Nodekey.
#[instrument(level = "trace", skip(graph))]
pub async fn handle_op<K>(graph: &Graph, op: &GraphOp<K>) -> Result<(), ActorProcessingErr>
where
    K: GraphNodeKey + Debug,
{
    let q = compile_graph_op(&op);

    let mut txn = graph.start_txn().await?;
    debug!("{}", q.query());
    txn.run(q)
        .await
        .map_err(|e| ActorProcessingErr::from(format!("neo4j execution failed: {:?}", e)))?;
    txn.commit().await?;
    trace!("transaction committed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        graph::{handle_op, GraphNodeKey},
        init_logging,
    };

    use super::GraphOp;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::neo4j::Neo4j;

    #[derive(Debug)]
    enum TestNodeKey {
        Node,
    }

    impl GraphNodeKey for TestNodeKey {
        fn cypher_match(&self, _prefix: &str) -> (String, Vec<(String, neo4rs::BoltType)>) {
            (format!("(:TestNode)"), vec![])
        }
    }

    #[tokio::test]
    async fn handle_op_upserts_node() {
        init_logging("polar.lib.testing".to_string());
        let container = Neo4j::default().start().await.unwrap();

        // prepare neo4rs client
        let config = neo4rs::ConfigBuilder::new()
            .uri(format!(
                "bolt://{}:{}",
                container.get_host().await.unwrap(),
                container.image().bolt_port_ipv4().unwrap()
            ))
            .user(container.image().user().expect("default user is set"))
            .password(
                container
                    .image()
                    .password()
                    .expect("default password is set"),
            )
            .build()
            .unwrap();

        // connect ot Neo4j
        let graph = neo4rs::Graph::connect(config).unwrap();

        // run a test query
        let op = GraphOp::UpsertNode {
            key: TestNodeKey::Node,
            props: Vec::default(),
        };

        let result = handle_op(&graph, &op).await;
        assert_eq!(result.is_ok(), true)
    }

    #[tokio::test]
    async fn handle_op_creates_edge() {
        init_logging("polar.handle_op_creates_edge".to_string());
        let container = Neo4j::default().start().await.unwrap();

        // prepare neo4rs client
        let config = neo4rs::ConfigBuilder::new()
            .uri(format!(
                "bolt://{}:{}",
                container.get_host().await.unwrap(),
                container.image().bolt_port_ipv4().unwrap()
            ))
            .user(container.image().user().expect("default user is set"))
            .password(
                container
                    .image()
                    .password()
                    .expect("default password is set"),
            )
            .build()
            .unwrap();

        // connect ot Neo4j
        let graph = neo4rs::Graph::connect(config).unwrap();

        // run a test query
        let op = GraphOp::UpsertNode {
            key: TestNodeKey::Node,
            props: Vec::default(),
        };

        let result = handle_op(&graph, &op).await;
        assert_eq!(result.is_ok(), true);

        let op = GraphOp::EnsureEdge {
            from: TestNodeKey::Node,
            to: TestNodeKey::Node,
            rel_type: "IS".into(),
            props: vec![],
        };

        let result = handle_op(&graph, &op).await;
        assert_eq!(result.is_ok(), true)
    }
}
