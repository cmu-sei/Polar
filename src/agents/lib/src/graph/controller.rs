use neo4rs::{BoltNull, BoltType, Graph, Query};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tracing::{debug, instrument, trace};

// ── Public type alias ──────────────────────────────────────────────────────────

/// The unified graph controller actor reference.
///
/// Now the controller is monomorphic — all node key types are erased to
/// `Box<dyn GraphNodeKey>` at the message boundary. Any agent, processor, or
/// component that holds a `GraphController` ref can send operations involving
/// any combination of node key types in a single message.
///
/// Concrete key types (KubeNodeKey, CyclopsNodeKey, GitLabNodeKey, etc.) implement
/// `GraphNodeKey` as before. The only change at call sites is wrapping keys with
/// `.into_key()` or `Box::new(key)` when constructing a `GraphOp`. The
/// `IntoGraphKey` convenience trait handles this without boilerplate.
pub type GraphController = ActorRef<GraphControllerMsg>;

pub const NULL_FIELD: &str = "null";

// ── Relationship constants ─────────────────────────────────────────────────────

/// Graph relationship type constants.
///
/// IMPORTANT: these must stay in sync with the Neo4j schema.
/// Treat changes here as breaking schema changes — existing edges in the
/// graph will not be migrated automatically.
pub mod rel {
    pub const IS: &str = "IS";
    pub const INSTANCE_OF: &str = "INSTANCE_OF";
    pub const CONTAINS: &str = "CONTAINS";
    pub const HAS_CONTAINER: &str = "HAS_CONTAINER";
    pub const USES_VOLUME: &str = "USES_VOLUME";
    pub const USES_IMAGE: &str = "USES_IMAGE";
    pub const POINTS_TO: &str = "POINTS_TO";
    pub const HOSTED_BY: &str = "HOSTED_BY";
    pub const BACKED_BY: &str = "BACKED_BY";
    pub const DESCRIBES: &str = "DESCRIBES";
    pub const BUILT_BY: &str = "BUILT_BY";
    pub const PRODUCED: &str = "PRODUCED";
    pub const TRANSITIONED_TO: &str = "TRANSITIONED_TO";
    pub const HAS_STATE: &str = "HAS_STATE";
    pub const OF_TYPE: &str = "OF_TYPE";
    pub const DEPENDS_ON: &str = "DEPENDS_ON";
    pub const ANALYZED_AS: &str = "ANALYZED_AS";
    pub const BUILT_FROM: &str = "BUILT_FROM";
    pub const ATTESTS: &str = "ATTESTS";
}

// ── GraphNodeKey trait ─────────────────────────────────────────────────────────

/// A node identity that can be compiled into a parameterized Cypher MERGE clause.
///
/// Implementors represent a specific node type in the knowledge graph — e.g.
/// `KubeNodeKey::Pod`, `CyclopsNodeKey::BuildJob`, `GitLabNodeKey::MergeRequest`.
///
/// The `prefix` parameter is critical for multi-node queries: when an operation
/// references two nodes (e.g. `EnsureEdge`), each node's parameters must have
/// distinct names to avoid collisions. Using `prefix` as a namespace for all
/// parameter keys (`{prefix}_uid`, `{prefix}_name`, etc.) guarantees this.
///
/// Object-safety requirements met:
/// - No generic methods.
/// - No `Self` in return position.
/// - `&self` receiver only.
/// The `Debug + Send + Sync` bounds are required for use in actor messages.
pub trait GraphNodeKey: Debug + Send + Sync {
    /// Generate a parameterized Cypher node pattern and its parameter bindings.
    ///
    /// Returns:
    /// - A Cypher node pattern string, e.g. `(n:Pod { uid: $n_uid })`
    /// - A list of `(param_name, BoltType)` pairs for binding
    ///
    /// All parameter names must be prefixed with `prefix` to prevent collisions
    /// when this pattern is composed with other nodes in the same query.
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>);
}

/// Convenience trait for converting concrete key types into `Box<dyn GraphNodeKey>`
/// without cluttering call sites with explicit `Box::new(...)` calls.
///
/// All types that implement `GraphNodeKey` get this for free via the blanket impl.
pub trait IntoGraphKey {
    fn into_key(self) -> Box<dyn GraphNodeKey>;
}

impl<T: GraphNodeKey + 'static> IntoGraphKey for T {
    fn into_key(self) -> Box<dyn GraphNodeKey> {
        Box::new(self)
    }
}

// ── Value and property types ───────────────────────────────────────────────────

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

// ── GraphOp ────────────────────────────────────────────────────────────────────

/// A graph mutation operation.
///
/// Previously generic over `K: GraphNodeKey`. Now concrete, using
/// `Box<dyn GraphNodeKey>` for node identity. This eliminates the generic
/// parameter that forced each agent to spawn a separate controller.
///
/// All node keys from any vocabulary — KubeNodeKey, CyclopsNodeKey,
/// GitLabNodeKey — can be mixed freely within a single GraphOp and sent
/// to a single GraphController actor.
///
/// Constructing a GraphOp:
/// ```rust
/// use polar::graph::{GraphOp, IntoGraphKey, Property, GraphValue};
///
/// // Using .into_key() from IntoGraphKey
/// let op = GraphOp::EnsureEdge {
///     from: KubeNodeKey::Pod { uid: "abc".into() }.into_key(),
///     rel_type: "BUILT_BY".into(),
///     to: CyclopsNodeKey::BuildJob { build_id: "xyz".into() }.into_key(),
///     props: vec![],
/// };
/// ```
#[derive(Debug)]
pub enum GraphOp {
    /// Upsert a node and set its properties.
    /// Uses Cypher MERGE semantics — creates if absent, updates if present.
    UpsertNode {
        key: Box<dyn GraphNodeKey>,
        props: Vec<Property>,
    },

    /// Ensure a directed relationship exists between two nodes.
    /// Creates both nodes if absent (MERGE semantics on all three patterns).
    EnsureEdge {
        from: Box<dyn GraphNodeKey>,
        to: Box<dyn GraphNodeKey>,
        rel_type: String,
        props: Vec<Property>,
    },

    /// Replace all outgoing edges of a given type from a node with a single
    /// new edge to a new target. Atomically removes old edges and creates the
    /// new one in the same transaction.
    ReplaceEdge {
        from: Box<dyn GraphNodeKey>,
        rel_type: String,
        to: Box<dyn GraphNodeKey>,
    },

    /// Remove all outgoing edges of a given type from a node.
    RemoveEdges {
        from: Box<dyn GraphNodeKey>,
        rel_type: String,
    },

    /// Transition a resource node to a new state using the append-only
    /// temporal state pattern:
    /// 1. Upsert the abstract state type node (shared taxonomy anchor)
    /// 2. Upsert the immutable state instance node (this specific observation)
    /// 3. Link resource → state instance via TRANSITIONED_TO (history)
    /// 4. Link state instance → state type via OF_TYPE (taxonomy)
    /// 5. Replace the HAS_STATE pointer on the resource (current state)
    UpdateState {
        resource_key: Box<dyn GraphNodeKey>,
        state_type_key: Box<dyn GraphNodeKey>,
        state_instance_key: Box<dyn GraphNodeKey>,
        state_instance_props: Vec<Property>,
    },
}

// ── GraphControllerMsg ─────────────────────────────────────────────────────────

/// Message type for the unified GraphController actor.
///
/// No longer generic — the `K` parameter is gone. Any sender holding a
/// `GraphController` (which is `ActorRef<GraphControllerMsg>`) can send any
/// `GraphOp` regardless of which node key vocabulary it uses.
pub enum GraphControllerMsg {
    Op(GraphOp),
}

// ── Compiler ───────────────────────────────────────────────────────────────────

/// Compile a `GraphOp` into a parameterized Neo4j `Query`.
///
/// Pure and deterministic — same input always produces the same query.
/// The resulting `Query` carries all parameters internally; callers do not
/// need to bind them separately.

pub fn compile_graph_op(op: &GraphOp) -> Query {
    let (cypher, params) = match op {
        GraphOp::UpsertNode { key, props } => {
            trace!("Received UpsertNode directive. {key:?}, {props:?}");

            // set a default prefix for the node's variable
            let prefix = "n";

            let (node_pattern, mut params) = key.cypher_match(prefix);
            let mut cypher = format!("MERGE {}", node_pattern);

            if !props.is_empty() {
                let sets = props
                    .iter()
                    .map(|Property(k, _)| format!("{prefix}.{k} = ${k}"))
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

            let from_node = "n";
            let to_node = "m";
            let (from_pat, mut params) = from.cypher_match(from_node);
            let (to_pat, mut to_params) = to.cypher_match(to_node);
            params.append(&mut to_params);

            let mut cypher = format!(
                "MERGE ({})\nMERGE ({})\nMERGE ({from_node})-[r:{}]->({to_node})",
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
        GraphOp::ReplaceEdge { from, rel_type, to } => {
            trace!("Received ReplaceEdge directive {from:?} -[{rel_type}]-> {to:?}");

            let (from_pat, mut params) = from.cypher_match("from");
            let (to_pat, mut to_params) = to.cypher_match("to");
            params.append(&mut to_params);

            let cypher = format!(
                "
                MERGE (a {from})
                WITH a
                OPTIONAL MATCH (a)-[r:{rel_type}]->()
                DELETE r
                WITH a
                MERGE (b {to})
                MERGE (a)-[:{rel_type}]->(b)
                ",
                from = from_pat.trim_start_matches('(').trim_end_matches(')'),
                to = to_pat.trim_start_matches('(').trim_end_matches(')'),
            );

            (cypher, params)
        }

        GraphOp::RemoveEdges { from, rel_type } => {
            trace!("Received RemoveEdges directive {from:?} -[{rel_type}]-> *");

            let (from_pat, params) = from.cypher_match("from");

            let cypher = format!(
                "
                MATCH (a {from})
                OPTIONAL MATCH (a)-[r:{rel_type}]->()
                DELETE r
                ",
                from = from_pat.trim_start_matches('(').trim_end_matches(')'),
            );

            (cypher, params)
        }
        GraphOp::UpdateState {
            resource_key,
            state_type_key,
            state_instance_key,
            state_instance_props,
        } => {
            trace!(
                "Received UpdateState directive {resource_key:?} -> {state_type_key:?} via {state_instance_key:?}"
            );

            // Generate node match patterns
            let (res_pat, mut params) = resource_key.cypher_match("res");
            let (stype_pat, mut stype_params) = state_type_key.cypher_match("stype");
            let (sinst_pat, mut sinst_params) = state_instance_key.cypher_match("sinst");

            params.append(&mut stype_params);
            params.append(&mut sinst_params);

            // Add state instance property SETs if provided
            let mut state_instance_set_clause = String::new();

            if !state_instance_props.is_empty() {
                let sets = state_instance_props
                    .iter()
                    .map(|Property(k, _)| format!("sinst.{k} = ${k}"))
                    .collect::<Vec<_>>()
                    .join(", ");

                state_instance_set_clause = format!("\nSET {sets}");

                for Property(k, v) in state_instance_props {
                    params.push((k.clone(), v.clone().into()));
                }
            }

            let cypher = format!(
                "
                // Upsert resource, state type, and state instance
                MERGE ({res})
                MERGE (stype {stype})
                MERGE ({sinst})
                {state_instance_set}

                // Record transition
                MERGE (res)-[:TRANSITIONED_TO]->(sinst)

                // Bind instance to its type
                MERGE (sinst)-[:OF_TYPE]->(stype)

                // Replace current state pointer
                WITH res, stype
                OPTIONAL MATCH (res)-[r:HAS_STATE]->()
                DELETE r
                MERGE (res)-[:HAS_STATE]->(stype)
                ",
                res = res_pat.trim_start_matches('(').trim_end_matches(')'),
                stype = stype_pat.trim_start_matches('(').trim_end_matches(')'),
                sinst = sinst_pat.trim_start_matches('(').trim_end_matches(')'),
                state_instance_set = state_instance_set_clause,
            );

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
pub async fn handle_op(graph: &Graph, op: &GraphOp) -> Result<(), ActorProcessingErr> {
    let q = compile_graph_op(op);

    let mut txn = graph.start_txn().await?;
    debug!("{}", q.query());
    debug!("{:?}", q.get_params());
    txn.run(q)
        .await
        .map_err(|e| ActorProcessingErr::from(format!("neo4j execution failed: {:?}", e)))?;
    txn.commit().await?;
    trace!("transaction committed");
    Ok(())
}
// ── GraphController actor ──────────────────────────────────────────────────────

/// The unified graph controller actor.
///
/// A single instance of this actor serves the entire Polar framework. Any
/// agent or processor can hold a clone of its `ActorRef<GraphControllerMsg>`
/// and send `GraphOp` messages mixing node keys from any vocabulary.
///
/// Replaces the `impl_graph_controller!` macro pattern. The macro generated
/// one actor type per vocabulary, requiring each agent to spawn its own
/// controller. Now there is one actor, spawned once, shared everywhere.
///
/// The actor serializes all writes to Neo4j — each message is executed in
/// its own transaction. This is intentional: concurrent writes to the same
/// node are safe because MERGE is idempotent, and serialization prevents
/// partial state from two concurrent UpdateState calls interleaving.
pub struct GraphControllerActor;

pub struct GraphControllerState {
    pub graph: Graph,
}

#[async_trait]
impl Actor for GraphControllerActor {
    type Msg = GraphControllerMsg;
    type State = GraphControllerState;
    type Arguments = Graph;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        graph: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("GraphControllerActor starting");
        Ok(GraphControllerState { graph })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GraphControllerMsg::Op(op) => {
                handle_op(&state.graph, &op).await?;
            }
        }
        Ok(())
    }
}
