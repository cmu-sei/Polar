use ractor::{concurrency::JoinHandle, Actor, ActorCell, ActorProcessingErr, ActorRef, SpawnErr};
use std::fmt::Debug;

use neo4rs::{BoltNull, BoltType, Config, Graph, Query};
use serde::{Deserialize, Serialize};

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

/// A Controller actor intended to centralize interactions with the graph database.
pub trait GraphController {
    /// A function intended to compile graph operations into Cypher queries and execute them against a Neo4j database.
    /// Implementing it for a given nodekey assures that the graph controller can handle the specific node identities.
    fn compile_graph_op<K>(op: &GraphOp<K>) -> Query
    where
        K: GraphNodeKey + Debug;
}
