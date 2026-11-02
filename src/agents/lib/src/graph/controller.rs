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

use super::environment::{GraphSettings, GraphTls};
use cassini_client::ClientKey;
use neo4rs::{BoltNull, BoltType, Graph, Query};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace};

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
    /// An OCIArtifact (registry manifest) refers to a ContainerImage
    /// (content identity). Multiple OCIArtifacts can refer to the same
    /// ContainerImage when pushed to different registries.
    pub const REFERS_TO: &str = "REFERS_TO";

    /// A ContainerImage has layers in a specific order.
    pub const HAS_LAYER: &str = "HAS_LAYER";
}

/// Cypher keywords that would be dangerous to interpolate directly into
/// a query as a label, relationship type, or property name -- not
/// exhaustive against every reserved word, but covers every keyword that
/// could actually mutate or destroy data if smuggled in this way.
const CYPHER_KEYWORD_BLACKLIST: &[&str] = &[
    "MATCH",
    "MERGE",
    "CREATE",
    "DELETE",
    "DETACH",
    "SET",
    "REMOVE",
    "RETURN",
    "WHERE",
    "WITH",
    "UNWIND",
    "CALL",
    "YIELD",
    "FOREACH",
    "LOAD",
    "CSV",
    "UNION",
    "OPTIONAL",
    "DROP",
    "CONSTRAINT",
    "INDEX",
];

#[derive(Debug, thiserror::Error)]
pub enum CypherIdentifierError {
    #[error("identifier cannot be empty")]
    Empty,
    #[error("identifier '{0}' contains characters other than ASCII alphanumerics and underscores")]
    InvalidCharacters(String),
    #[error("identifier '{0}' cannot start with a digit")]
    StartsWithDigit(String),
    #[error("identifier '{0}' is a reserved Cypher keyword")]
    ReservedKeyword(String),
}

/// Validates that `s` is safe to interpolate directly into a Cypher query
/// as a label, relationship type, or property name. Cypher can only
/// parameterize property *values* -- never labels, relationship types, or
/// property names -- so anything derived from external/untrusted input
/// (a Jira custom field name, for example) that ends up in one of those
/// positions must pass this check first.
pub fn validate_cypher_identifier(s: &str) -> Result<(), CypherIdentifierError> {
    if s.is_empty() {
        return Err(CypherIdentifierError::Empty);
    }
    if s.chars().next().unwrap().is_ascii_digit() {
        return Err(CypherIdentifierError::StartsWithDigit(s.to_string()));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == 'Σ')
    {
        return Err(CypherIdentifierError::InvalidCharacters(s.to_string()));
    }
    if CYPHER_KEYWORD_BLACKLIST.contains(&s.to_ascii_uppercase().as_str()) {
        return Err(CypherIdentifierError::ReservedKeyword(s.to_string()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum GraphOpError {
    #[error(transparent)]
    Identifier(#[from] CypherIdentifierError),
    #[error(
        "compiled query uses keyword '{keyword}', not permitted for this \
         GraphOp variant (permitted: {allowed:?})"
    )]
    DisallowedKeyword {
        keyword: String,
        allowed: Vec<String>,
    },
}

/// Crate-private machinery behind every "raw" GraphOp variant -- not
/// pub, so only compile_graph_op's own match arms can call it. Each
/// public "Raw*" variant is a thin wrapper with its own explicit,
/// narrow allowlist; there is no way for code outside this file to
/// bypass a variant's allowlist or invoke this with an arbitrary one
/// of its own choosing.
///
/// An allowlist, not a denylist: scans the compiled query text for
/// every keyword in CYPHER_KEYWORD_BLACKLIST (the same fixed
/// vocabulary validate_cypher_identifier already checks against) and
/// rejects the query unless every one found is explicitly present in
/// `allowed`. A keyword this function doesn't yet know about can't
/// silently slip through the way it could with a hand-picked
/// "dangerous words" denylist -- and, more importantly, a *known*
/// keyword that isn't explicitly permitted for this specific variant
/// is rejected by default, not just a hand-picked dangerous subset of
/// them.
fn validate_raw_op_text(cypher: &str, allowed: &[&str]) -> Result<(), GraphOpError> {
    let upper = cypher.to_ascii_uppercase();
    let words: std::collections::HashSet<&str> = upper
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    for kw in CYPHER_KEYWORD_BLACKLIST {
        if words.contains(kw) && !allowed.contains(kw) {
            return Err(GraphOpError::DisallowedKeyword {
                keyword: (*kw).to_string(),
                allowed: allowed.iter().map(|s| s.to_string()).collect(),
            });
        }
    }
    Ok(())
}

/// A node key whose label and merge-key property name are determined at
/// runtime rather than fixed in an enum variant at compile time -- needed
/// for data sources whose own schema isn't known ahead of time (Jira
/// custom fields, where the field name itself becomes part of the graph
/// schema). label and key_prop_name are validated at construction time,
/// since those two positions can never be safely parameterized; unlike
/// them, key_prop_value is free-form and is parameterized normally.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DynamicNodeKey {
    label: String,
    key_prop_name: String,
    key_prop_value: String,
}

impl DynamicNodeKey {
    pub fn new(
        label: impl Into<String>,
        key_prop_name: impl Into<String>,
        key_prop_value: impl Into<String>,
    ) -> Result<Self, CypherIdentifierError> {
        let label = label.into();
        let key_prop_name = key_prop_name.into();
        validate_cypher_identifier(&label)?;
        validate_cypher_identifier(&key_prop_name)?;
        Ok(Self {
            label,
            key_prop_name,
            key_prop_value: key_prop_value.into(),
        })
    }
}

impl GraphNodeKey for DynamicNodeKey {
    fn identity(&self) -> NodeIdentity {
        // Both names were validated by `new`; owned, since they are known only at run time.
        NodeIdentity::runtime(
            vec![self.label.clone()],
            vec![(
                self.key_prop_name.clone(),
                BoltType::String(self.key_prop_value.clone().into()),
            )],
        )
    }
}

// ── Node identity ──────────────────────────────────────────────────────────────

/// What identifies a node: its labels, and the properties MERGE matches it on with their values, in order. Every node
/// key says only this (`GraphNodeKey::identity`); the Cypher pattern, the batch rows and the constraints are all
/// derived from it (docs/architecture/graph-write-path.md), so no key can write a pattern that ignores the variable
/// it is given.
///
/// A label known when the code is built is `Cow::Borrowed`; one known only at run time (a Jira custom field's, a
/// backend job's) is `Cow::Owned`, validated where it was made, and has no constraint
/// ([`NodeIdentity::shape`] is `None`).
#[derive(Debug, Clone, PartialEq)]
pub struct NodeIdentity {
    pub labels: Vec<std::borrow::Cow<'static, str>>,
    pub keys: Vec<(String, BoltType)>,
}

/// The labels and the key property names of a node, without the values: what a batch is grouped by and what a
/// constraint is made for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeShape {
    pub labels: Vec<String>,
    pub keys: Vec<String>,
}

impl NodeIdentity {
    /// A node with labels fixed in the code.
    pub fn fixed(labels: &[&'static str], keys: Vec<(&str, BoltType)>) -> Self {
        Self {
            labels: labels
                .iter()
                .map(|l| std::borrow::Cow::Borrowed(*l))
                .collect(),
            keys: keys.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }

    /// A node whose labels and key names are known only at run time, already validated.
    pub fn runtime(labels: Vec<String>, keys: Vec<(String, BoltType)>) -> Self {
        Self {
            labels: labels.into_iter().map(std::borrow::Cow::Owned).collect(),
            keys,
        }
    }

    /// The labels joined as Cypher writes them after a variable: `:A:B`.
    fn label_text(&self) -> String {
        self.labels.iter().map(|l| format!(":{l}")).collect()
    }

    /// The node's pattern with the variable `prefix`, and its parameters, each named `{prefix}_{key}`:
    /// `(n:Pod { uid: $n_uid })`, or `(n:State)` for a node with no key.
    pub fn pattern(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        let labels = self.label_text();
        if self.keys.is_empty() {
            return (format!("({prefix}{labels})"), vec![]);
        }
        let props = self
            .keys
            .iter()
            .map(|(k, _)| format!("{k}: ${prefix}_{k}"))
            .collect::<Vec<_>>()
            .join(", ");
        let params = self
            .keys
            .iter()
            .map(|(k, v)| (format!("{prefix}_{k}"), v.clone()))
            .collect();
        (format!("({prefix}{labels} {{ {props} }})"), params)
    }

    /// The labels and key names, for a node whose labels are all fixed in the code; `None` for one with a label
    /// known only at run time, for which nothing can be declared ahead.
    pub fn shape(&self) -> Option<NodeShape> {
        self.labels
            .iter()
            .all(|l| matches!(l, std::borrow::Cow::Borrowed(_)))
            .then(|| self.grouping())
    }

    /// The labels and key names, whatever kind the labels are: what a batch is grouped by.
    fn grouping(&self) -> NodeShape {
        NodeShape {
            labels: self.labels.iter().map(|l| l.to_string()).collect(),
            keys: self.keys.iter().map(|(k, _)| k.clone()).collect(),
        }
    }
}

// ── GraphNodeKey trait ─────────────────────────────────────────────────────────

/// A node identity that can be compiled into a parameterized Cypher MERGE clause.
///
/// Implementors represent a specific node type in the knowledge graph — e.g.
/// `KubeNodeKey::Pod`, `CyclopsNodeKey::BuildJob`, `GitLabNodeKey::MergeRequest`.
///
/// A key says what identifies its node ([`identity`](Self::identity)); the pattern is derived from that, with the
/// variable it is given bound (`(n:State)`, never `(:State)`), so an op may compose any keys in one statement.
///
/// Object-safety requirements met:
/// - No generic methods.
/// - No `Self` in return position.
/// - `&self` receiver only.
///
/// The `Debug + Send + Sync` bounds are required for use in actor messages.
pub trait GraphNodeKey: Debug + Send + Sync {
    /// The node's labels and the properties it is merged on.
    fn identity(&self) -> NodeIdentity;

    /// The parameterized Cypher node pattern and its parameter bindings, derived from
    /// [`identity`](Self::identity): e.g. `(n:Pod { uid: $n_uid })`, every parameter named `{prefix}_…` so patterns
    /// with different prefixes compose in one query.
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        self.identity().pattern(prefix)
    }
}

/// A node key enum whose variants can be listed, one of each with placeholder values: what the shapes of its nodes
/// are read from, for a test that holds every variant to the pattern rules and for the constraints an owner makes at
/// start. `every_variant` matches exhaustively, so a variant added to the enum does not compile until it is listed.
pub trait NodeKeyVocabulary: GraphNodeKey + Sized {
    fn every_variant() -> Vec<Self>;
}

/// What is wrong with `key`'s pattern under the variable `prefix`, by the rules every op relies on to compose
/// patterns in one statement; `None` when it keeps them all:
/// - it binds its variable (`(n:Label ...)`, never `(:Label)`) and names at least one label;
/// - it is one node pattern with at most one property map;
/// - every parameter is named `{prefix}_…` and used in the pattern;
/// - nothing else depends on the prefix: under another variable the pattern differs only in the variable and the
///   parameter names, so the same key matches the same properties in every op.
///
/// For the tests of each vocabulary, in whatever crate it lives.
pub fn pattern_problem(key: &dyn GraphNodeKey, prefix: &str) -> Option<String> {
    let (pattern, params) = key.cypher_match(prefix);
    if !pattern.starts_with(&format!("({prefix}:")) || !pattern.ends_with(')') {
        return Some(format!(
            "{key:?} with `{prefix}` does not bind it: {pattern}"
        ));
    }
    if pattern.matches('(').count() != 1 || pattern.matches(')').count() != 1 {
        return Some(format!("{key:?} is not one node pattern: {pattern}"));
    }
    let maps = pattern.matches('{').count();
    if maps > 1 || maps != pattern.matches('}').count() || maps != usize::from(!params.is_empty()) {
        return Some(format!(
            "{key:?} has a property map that does not match its parameters: {pattern}"
        ));
    }
    for (name, _) in &params {
        if !name.starts_with(&format!("{prefix}_")) {
            return Some(format!(
                "{key:?} with `{prefix}` has a parameter named {name}"
            ));
        }
        if !pattern.contains(&format!("${name}")) {
            return Some(format!("{pattern} does not use ${name}"));
        }
    }
    let other = if prefix == "q" { "r" } else { "q" };
    let (renamed, _) = key.cypher_match(other);
    let expected = pattern
        .replacen(&format!("({prefix}:"), &format!("({other}:"), 1)
        .replace(&format!("${prefix}_"), &format!("${other}_"));
    if renamed != expected {
        return Some(format!(
            "{key:?} depends on its variable beyond its name: `{prefix}` gives {pattern}, `{other}` gives {renamed}"
        ));
    }
    None
}

/// The shapes of a vocabulary's nodes with fixed labels, each once, ordered.
pub fn shapes_of<V: NodeKeyVocabulary>() -> Vec<NodeShape> {
    let mut shapes: Vec<NodeShape> = V::every_variant()
        .iter()
        .filter_map(|k| k.identity().shape())
        .collect();
    shapes.sort();
    shapes.dedup();
    shapes
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
/// ```rust,ignore
/// // Illustrative only -- KubeNodeKey and CyclopsNodeKey are
/// // downstream, agent-specific vocabularies (e.g. kubernetes-common,
/// // cyclops-common), not something this crate itself can import
/// // without a circular dependency. This example can't genuinely be
/// // compiled from within polar/lib alone.
/// use polar::graph::controller::{GraphOp, IntoGraphKey, Property};
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

    /// Upsert many nodes of the same or different kinds in a single
    /// transaction, same MERGE semantics as UpsertNode: one
    /// `UNWIND $rows` statement for each label and key shape among the
    /// items, whatever their number; an empty batch sends nothing
    /// (docs/architecture/graph-write-path.md, "Batches").
    UpsertNodes {
        items: Vec<(Box<dyn GraphNodeKey>, Vec<Property>)>,
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

    /// A parameterized Cypher MERGE-write the typed GraphOp model can't
    /// express (e.g. matching two existing nodes by arbitrary properties
    /// before creating an edge between them, or an UNWIND-batched upsert
    /// with a structure UpsertNodes doesn't cover). Every one of this
    /// codebase's real, current uses is MATCH/MERGE/SET/UNWIND only --
    /// enforced, not just documented: see validate_raw_op_text's call
    /// in this variant's own compile_graph_op arm. Named for that real,
    /// narrow scope rather than "Raw", since the actual permission this
    /// grants is bounded, not arbitrary. A genuine future need for
    /// something broader (a delete, a procedure call) should be its
    /// own new, separately-named, individually-reviewable variant with
    /// its own explicit allowlist -- never a quiet widening of this
    /// one's.
    ///
    /// The caller is responsible for correct Cypher and parameter
    /// binding within that scope. Parameters are passed as (name,
    /// BoltType) pairs matching $name placeholders in the query string.
    RawMergeWrite {
        cypher: String,
        params: Vec<(String, BoltType)>,
    },

    /// Run an arbitrary, heterogeneous sequence of ops in a single
    /// transaction -- one round-trip, one commit -- for callers whose
    /// single logical write spans several different node/edge shapes at
    /// once (e.g. one Jira issue producing an issue node, several related
    /// nodes of different types, and several relationship types, all from
    /// one incoming message). All or nothing: the ops' statements run in
    /// one transaction. Its node upserts and edges between two barriers
    /// (ReplaceEdge, RemoveEdges, UpdateState, RawMergeWrite) are
    /// grouped by shape into UNWIND statements, nodes before edges, and
    /// every barrier keeps its place (compile_graph_op;
    /// docs/architecture/graph-write-path.md, "Round trips").
    Transaction { ops: Vec<GraphOp> },
}

// ── GraphControllerMsg ─────────────────────────────────────────────────────────

/// Message type for the unified GraphController actor.
///
/// No longer generic — the `K` parameter is gone. Any sender holding a
/// `GraphController` (which is `ActorRef<GraphControllerMsg>`) can send any
/// `GraphOp` regardless of which node key vocabulary it uses.
pub enum GraphControllerMsg {
    Op(GraphOp),
    Tick,
    /// A read-only query, returning its collected result rows.
    /// Unlike Op, this bypasses compile_graph_op's own validation
    /// entirely -- the caller is trusted to construct a safe,
    /// parameterized query itself, matching neo4rs::query(...)'s own
    /// established parameterization pattern. No schema-access
    /// enforcement yet (which query labels a given agent should be
    /// allowed to touch at all) -- a real, separate, deliberately
    /// deferred piece of future work, not an oversight.
    Query(
        neo4rs::Query,
        RpcReplyPort<Result<Vec<neo4rs::Row>, String>>,
    ),
}

// ── Compiler ───────────────────────────────────────────────────────────────────

/// What a batch statement's rows are grouped by: a node's shape, or an edge's two shapes and its type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GroupKey {
    Nodes(NodeShape),
    Edges(NodeShape, String, NodeShape),
}

/// The rows of one batch statement, and which of the plan's items they came from.
#[derive(Debug)]
struct Group {
    key: GroupKey,
    rows: Vec<BoltType>,
    items: Vec<usize>,
}

/// An op compiled: its statements in the order they run, one transaction, and for each item it was made of (each node
/// of a batch, each op of a transaction, in order) the statement that writes it.
#[derive(Debug, Default)]
struct Plan {
    statements: Vec<(String, Vec<(String, BoltType)>)>,
    placed: Vec<usize>,
}

/// Builds a [`Plan`]: node upserts and edges are gathered into groups until a barrier (an op whose meaning depends on
/// what was written before it: ReplaceEdge, RemoveEdges, UpdateState, RawMergeWrite), then written, nodes before
/// edges, each group in the order it first appeared and its rows in their order; the barrier follows.
#[derive(Debug, Default)]
struct Planner {
    plan: Plan,
    nodes: Vec<Group>,
    edges: Vec<Group>,
    index: std::collections::HashMap<GroupKey, usize>,
}

/// A map parameter from name-value pairs.
fn bolt_map(pairs: impl IntoIterator<Item = (String, BoltType)>) -> BoltType {
    pairs
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>()
        .into()
}

/// The properties to set, as one map, each name validated first: a name is never a parameter.
fn props_map(props: &[Property]) -> Result<BoltType, GraphOpError> {
    for Property(k, _) in props {
        validate_cypher_identifier(k)?;
    }
    Ok(bolt_map(
        props
            .iter()
            .map(|Property(k, v)| (k.clone(), v.clone().into())),
    ))
}

/// A node's pattern in a batch statement: the variable `var`, its labels, and each key property read from the row's
/// map `field`: `(n:Pod { uid: row.k.uid })`.
fn row_pattern(var: &str, shape: &NodeShape, field: &str) -> String {
    let labels: String = shape.labels.iter().map(|l| format!(":{l}")).collect();
    if shape.keys.is_empty() {
        return format!("({var}{labels})");
    }
    let keys = shape
        .keys
        .iter()
        .map(|k| format!("{k}: row.{field}.{k}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("({var}{labels} {{ {keys} }})")
}

/// A node's key values as the row's map.
fn key_map(identity: NodeIdentity) -> BoltType {
    bolt_map(identity.keys)
}

impl Planner {
    fn item(&mut self) -> usize {
        self.plan.placed.push(usize::MAX);
        self.plan.placed.len() - 1
    }

    fn add(&mut self, key: GroupKey, row: BoltType) {
        let item = self.item();
        let groups = match key {
            GroupKey::Nodes(_) => &mut self.nodes,
            GroupKey::Edges(..) => &mut self.edges,
        };
        let at = *self.index.entry(key.clone()).or_insert_with(|| {
            groups.push(Group {
                key,
                rows: Vec::new(),
                items: Vec::new(),
            });
            groups.len() - 1
        });
        groups[at].rows.push(row);
        groups[at].items.push(item);
    }

    fn node(&mut self, key: &dyn GraphNodeKey, props: &[Property]) -> Result<(), GraphOpError> {
        let identity = key.identity();
        let shape = identity.grouping();
        let row = bolt_map([
            ("k".to_string(), key_map(identity)),
            ("p".to_string(), props_map(props)?),
        ]);
        self.add(GroupKey::Nodes(shape), row);
        Ok(())
    }

    fn edge(
        &mut self,
        from: &dyn GraphNodeKey,
        rel_type: &str,
        to: &dyn GraphNodeKey,
        props: &[Property],
    ) -> Result<(), GraphOpError> {
        validate_cypher_identifier(rel_type)?;
        let (from, to) = (from.identity(), to.identity());
        let key = GroupKey::Edges(from.grouping(), rel_type.to_string(), to.grouping());
        let row = bolt_map([
            ("f".to_string(), key_map(from)),
            ("t".to_string(), key_map(to)),
            ("p".to_string(), props_map(props)?),
        ]);
        self.add(key, row);
        Ok(())
    }

    /// Writes the gathered groups, nodes first.
    fn flush(&mut self) {
        self.index.clear();
        let groups: Vec<Group> = self.nodes.drain(..).chain(self.edges.drain(..)).collect();
        for group in groups {
            let cypher = match &group.key {
                GroupKey::Nodes(shape) => format!(
                    "UNWIND $rows AS row\nMERGE {}\nSET n += row.p",
                    row_pattern("n", shape, "k")
                ),
                GroupKey::Edges(from, rel_type, to) => format!(
                    "UNWIND $rows AS row\nMERGE {}\nMERGE {}\nMERGE (a)-[r:{rel_type}]->(b)\nSET r += row.p",
                    row_pattern("a", from, "f"),
                    row_pattern("b", to, "t"),
                ),
            };
            let at = self.plan.statements.len();
            for item in group.items {
                self.plan.placed[item] = at;
            }
            self.plan
                .statements
                .push((cypher, vec![("rows".to_string(), group.rows.into())]));
        }
    }

    fn barrier(&mut self, statement: (String, Vec<(String, BoltType)>)) {
        self.flush();
        let item = self.item();
        self.plan.placed[item] = self.plan.statements.len();
        self.plan.statements.push(statement);
    }

    fn op(&mut self, op: &GraphOp) -> Result<(), GraphOpError> {
        match op {
            GraphOp::UpsertNode { key, props } => self.node(key.as_ref(), props),
            GraphOp::UpsertNodes { items } => {
                for (key, props) in items {
                    self.node(key.as_ref(), props)?;
                }
                Ok(())
            }
            GraphOp::EnsureEdge {
                from,
                to,
                rel_type,
                props,
            } => self.edge(from.as_ref(), rel_type, to.as_ref(), props),
            GraphOp::Transaction { ops } => {
                for op in ops {
                    self.op(op)?;
                }
                Ok(())
            }
            barrier => {
                let statement = compile_barrier(barrier)?;
                self.barrier(statement);
                Ok(())
            }
        }
    }
}

/// The plan for `op`: its statements and where each of its items is written.
fn plan(op: &GraphOp) -> Result<Plan, GraphOpError> {
    let mut planner = Planner::default();
    planner.op(op)?;
    planner.flush();
    Ok(planner.plan)
}

/// Compile a `GraphOp` into the parameterized Neo4j statements that write it, in the order they run, all in one
/// transaction (docs/architecture/graph-write-path.md).
///
/// - A node upsert, each node of an `UpsertNodes` and an edge become a row of an `UNWIND $rows AS row` statement, one
///   statement for each label and key shape (an edge: its two ends' shapes and its type), whatever the number of
///   rows: one plan, which Neo4j caches. An empty batch compiles to no statement, and nothing is sent.
/// - In a `Transaction`, the nodes and edges between two barriers (ReplaceEdge, RemoveEdges, UpdateState,
///   RawMergeWrite, whose meaning depends on what was written before them) are grouped so: nodes before edges, each
///   group where its first item was, its rows in their order. An edge MERGEs its ends itself and sets nothing on
///   them, so writing a node's properties before an edge that came first changes nothing; a barrier is written after
///   everything before it and before everything after it.
///
/// Pure and deterministic given the same input. Fallible: Cypher can never parameterize labels, relationship types,
/// or property *names*, so every caller-supplied String in one of those positions (a rel_type, every Property's
/// name) is validated here via validate_cypher_identifier before it can reach query text, and the whole op is refused
/// before anything is sent. A key's labels and key names are fixed in the code or validated where the key was made
/// (DynamicNodeKey, BackendJobIdentity).
pub fn compile_graph_op(op: &GraphOp) -> Result<Vec<Query>, GraphOpError> {
    Ok(plan(op)?
        .statements
        .into_iter()
        .map(|(cypher, params)| {
            let mut q = Query::new(cypher);
            for (k, v) in params {
                q = q.param(&k, v);
            }
            q
        })
        .collect())
}

/// The one statement of an op whose meaning depends on what was written before it.
fn compile_barrier(op: &GraphOp) -> Result<(String, Vec<(String, BoltType)>), GraphOpError> {
    let compiled = match op {
        GraphOp::ReplaceEdge { from, rel_type, to } => {
            trace!("Received ReplaceEdge directive {from:?} -[{rel_type}]-> {to:?}");

            validate_cypher_identifier(rel_type)?;

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

            validate_cypher_identifier(rel_type)?;

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

            for Property(k, _) in state_instance_props {
                validate_cypher_identifier(k)?;
            }

            // Generate node match patterns. Each binds the variable it is given (`stype:State`), so the state type
            // is the one node `MERGE ({stype})` names (graph-write-path.md, "Every key binds its variable").
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
                MERGE ({stype})
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
        GraphOp::RawMergeWrite { cypher, params } => {
            validate_raw_op_text(cypher, &["MATCH", "MERGE", "SET", "UNWIND"])?;
            (cypher.clone(), params.clone())
        }
        GraphOp::UpsertNode { .. }
        | GraphOp::UpsertNodes { .. }
        | GraphOp::EnsureEdge { .. }
        | GraphOp::Transaction { .. } => {
            unreachable!("the planner batches nodes, edges and transactions itself")
        }
    };
    Ok(compiled)
}

/// Writes `op`: its statements ([`compile_graph_op`]) in one transaction, all or nothing. Returns how many statements
/// ran; an op with nothing to write (an empty batch) runs none and opens no transaction.
#[instrument(level = "trace", skip(graph))]
pub async fn handle_op(graph: &Graph, op: &GraphOp) -> Result<usize, ActorProcessingErr> {
    let queries = compile_graph_op(op)?;
    if queries.is_empty() {
        trace!("nothing to write");
        return Ok(0);
    }
    let statements = queries.len();
    let mut txn = graph.start_txn().await?;
    for q in queries {
        debug!("{}", q.query());
        trace!("{:?}", q.get_params());
        txn.run(q)
            .await
            .map_err(|e| ActorProcessingErr::from(format!("neo4j execution failed: {:?}", e)))?;
    }
    txn.commit().await?;
    trace!("transaction committed ({statements} statements)");
    Ok(statements)
}

/// Runs a read-only query against `graph` directly, collecting its
/// result rows. Pure with respect to actor state -- takes `&Graph`
/// the same way `handle_op` does, so it's directly testable against a
/// real Neo4j connection without needing to spawn an actor or
/// establish a second, independent connection of its own.
pub async fn handle_query(
    graph: &neo4rs::Graph,
    query: neo4rs::Query,
) -> Result<Vec<neo4rs::Row>, String> {
    match graph.execute_read(query).await {
        Ok(mut stream) => {
            let mut rows = Vec::new();
            loop {
                match stream.next().await {
                    Ok(Some(row)) => rows.push(row),
                    Ok(None) => break,
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(rows)
        }
        Err(e) => Err(e.to_string()),
    }
}

// ── GraphSignal ────────────────────────────────────────────────────────────

/// Signals the GraphController emits about real connectivity — distinct
/// from GraphConnected in HealthCheckMessage (which only means "the actor
/// spawned"). neo4rs::Graph::connect only builds pool config; it does not
/// handshake or authenticate, so actor-spawn success guarantees nothing
/// about actual reachability.
#[derive(Debug, Clone)]
pub enum GraphSignal {
    /// A real operation against Neo4j succeeded — canary or app-level write.
    Available,
    /// A real operation against Neo4j failed. Carries a short reason.
    OpFailed(String),
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
/// TODO: Create an impl for this struct that allows us to spawn a new one based on a given configuration
pub struct GraphControllerActor;

impl GraphControllerActor {
    /// Standard helper fn to get a neo4rs configuration based on environment variables.
    ///
    /// All of the following variables are required unless otherwise specified.
    ///
    ///   GRAPH_DB        — name of the neo4j database
    ///   GRAPH_USER      — username to authenticate with
    ///   GRAPH_PASSWORD  — password credential
    ///   GRAPH_ENDPOINT  — bolt endpoint, e.g. bolt+s://neo4j.polar.svc.cluster.local:7687
    ///
    /// What is read, and the rule between the TLS variables, are declared in
    /// `graph::environment`: GRAPH_CLIENT_CERT, GRAPH_CLIENT_KEY and GRAPH_CA_CERT are all
    /// set (mutual TLS), or only GRAPH_CA_CERT (verify the server, present no client
    /// certificate, as behind a proxy with its own CA), or none. Any other combination
    /// fails fast rather than silently connecting without the full certificate chain.
    pub fn get_neo_config() -> Result<neo4rs::Config, ractor::ActorProcessingErr> {
        Self::neo_config_from(&polar_env::ProcessEnv)
    }

    /// As [`get_neo_config`](Self::get_neo_config), reading from `env`. What is read, and
    /// the rule between the TLS variables, are declared in `graph::environment`.
    pub fn neo_config_from(
        env: &dyn polar_env::Env,
    ) -> Result<neo4rs::Config, ractor::ActorProcessingErr> {
        let settings = GraphSettings::from_env(env).map_err(ractor::ActorProcessingErr::from)?;
        Self::neo_config_for(settings)
    }

    /// The connection to use for `args`: from the settings it was given, or, for an agent
    /// not yet on the builder, from the process.
    pub fn neo_config_for_args(
        args: &GraphControllerArgs,
    ) -> Result<neo4rs::Config, ractor::ActorProcessingErr> {
        match &args.settings {
            Some(settings) => Self::neo_config_for(settings.clone()),
            None => Self::get_neo_config(),
        }
    }

    /// The connection for settings already read and judged.
    pub fn neo_config_for(
        settings: GraphSettings,
    ) -> Result<neo4rs::Config, ractor::ActorProcessingErr> {
        info!("Using Neo4j database at {}", settings.endpoint);

        let builder = neo4rs::ConfigBuilder::default()
            .uri(settings.endpoint)
            .user(settings.user)
            .password(settings.password)
            .db(settings.database)
            .fetch_size(500)
            .max_connections(10);

        let config = match settings.tls {
            GraphTls::Mutual {
                ca,
                cert,
                key: ClientKey::File(key),
            } => {
                info!("Configuring Neo4j mTLS with cert={cert} key={key} ca={ca}");
                builder
                    .with_mutual_tls_validation(Some(ca), cert, key)
                    .build()?
            }
            GraphTls::Mutual {
                ca,
                cert,
                key: custody @ ClientKey::Custody(_),
            } => {
                // The key is the custody daemon's, in no file: rustls is configured here, TLS 1.3 alone, and handed
                // to neo4rs whole (docs/architecture/key-custody.md, "The neo4rs patch").
                info!(
                    "Configuring Neo4j mTLS with cert={cert} ca={ca}, its key the custody daemon's"
                );
                builder
                    .with_rustls_config(Arc::new(
                        custody_tls(&ca, &cert, &custody).map_err(ActorProcessingErr::from)?,
                    ))
                    .build()?
            }
            GraphTls::ServerOnly { ca } => {
                // TLS without client cert: Neo4j verifies the server, we don't present a
                // cert. Covers the old istio proxy CA case.
                info!("Configuring Neo4j TLS (no client cert) with ca={ca}");
                builder.with_client_certificate(ca).build()?
            }
            GraphTls::None => {
                info!("Configuring Neo4j without TLS client configuration");
                builder.build()?
            }
        };

        Ok(config)
    }
}

/// What the graph controller is started with. Built with [`new`](Self::new), so a field added
/// later touches none of its callers.
#[non_exhaustive]
pub struct GraphControllerArgs {
    pub signal_port: std::sync::Arc<ractor::OutputPort<GraphSignal>>,
    /// The connection settings, already read and judged. The agent builder supplies them
    /// from its snapshot. `None` is for agents not yet on the builder: the controller then
    /// reads the process, as it always did, until the last of them moves.
    pub settings: Option<GraphSettings>,
    /// The constraints and indexes for the labels the agent owns, made at the first contact with the graph
    /// (`graph::schema`; docs/architecture/graph-write-path.md, "Constraints and indexes"). The agent builder derives
    /// them from `PolarAgent::GRAPH` and `PolarAgent::GRAPH_KEYS`; empty for a reader, which makes nothing.
    pub key_indexes: Vec<crate::graph::schema::KeyIndex>,
    /// The repairs of what this writer wrote wrong before, run at the first contact with the graph, after the
    /// constraints (`graph::repair`).
    pub repairs: Vec<crate::graph::repair::Repair>,
}

impl GraphControllerArgs {
    pub fn new(signal_port: std::sync::Arc<ractor::OutputPort<GraphSignal>>) -> Self {
        Self {
            signal_port,
            settings: None,
            key_indexes: Vec::new(),
            repairs: Vec::new(),
        }
    }

    /// Run these repairs at the first contact with the graph.
    pub fn with_repairs(mut self, repairs: Vec<crate::graph::repair::Repair>) -> Self {
        self.repairs = repairs;
        self
    }

    /// Make these constraints and indexes at the first contact with the graph.
    pub fn with_key_indexes(mut self, key_indexes: Vec<crate::graph::schema::KeyIndex>) -> Self {
        self.key_indexes = key_indexes;
        self
    }

    /// Connect with these settings instead of reading the process.
    pub fn with_settings(mut self, settings: GraphSettings) -> Self {
        self.settings = Some(settings);
        self
    }
}

pub struct GraphControllerState {
    pub graph: Graph,
    pub signal_port: std::sync::Arc<ractor::OutputPort<GraphSignal>>,
    /// The constraints and indexes not yet made: until the graph first answers.
    pub pending_key_indexes: Vec<crate::graph::schema::KeyIndex>,
    /// The repairs not yet run.
    pub pending_repairs: Vec<crate::graph::repair::Repair>,
}

impl GraphControllerState {
    /// Once the graph answers: makes the pending constraints and indexes, then runs the pending repairs. What the
    /// graph refuses is logged and not asked again; what could not be asked stays pending for the next tick.
    async fn first_contact(&mut self) {
        if !self.pending_key_indexes.is_empty() {
            match crate::graph::schema::ensure_key_indexes(&self.graph, &self.pending_key_indexes)
                .await
            {
                Ok(_) => self.pending_key_indexes.clear(),
                Err(e) => {
                    error!(
                        "GraphControllerActor: could not ask the graph for its constraints, asking again later: {e}"
                    );
                    return;
                }
            }
        }
        while let Some(repair) = self.pending_repairs.first().copied() {
            match repair.run(&self.graph).await {
                Ok(_) => {
                    self.pending_repairs.remove(0);
                }
                Err(e) => {
                    error!("GraphControllerActor: {e}; trying again at the next tick");
                    return;
                }
            }
        }
    }
}

#[async_trait]
impl Actor for GraphControllerActor {
    type Msg = GraphControllerMsg;
    type State = GraphControllerState;
    type Arguments = GraphControllerArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("GraphControllerActor starting");
        match Self::neo_config_for_args(&args) {
            Ok(conf) => match Graph::connect(conf) {
                Ok(graph) => Ok(GraphControllerState {
                    graph,
                    signal_port: args.signal_port,
                    pending_key_indexes: args.key_indexes,
                    pending_repairs: args.repairs,
                }),
                Err(e) => {
                    error!("Failed to set up graph connection. {e}");
                    Err(ActorProcessingErr::from(e))
                }
            },
            Err(e) => Err(e),
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Canary: "RETURN 1" can only fail on auth/connectivity, never on
        // data content — this is what actually proves the graph is
        // reachable, as opposed to pre_start's pool config succeeding.
        match state
            .graph
            .execute(Query::new("RETURN 1".to_string()))
            .await
        {
            Ok(_) => {
                info!("GraphControllerActor: canary query succeeded, graph available");
                state.signal_port.send(GraphSignal::Available);
                // Before the first op: the actor takes no message until post_start returns.
                state.first_contact().await;
            }
            Err(e) => {
                let reason = format!("canary query failed: {e}");
                error!("GraphControllerActor: {reason}");
                state.signal_port.send(GraphSignal::OpFailed(reason));
            }
        }

        // The canary above only proves reachability at startup. Without a
        // recurring re-check, GraphSignal::Available only ever fires again
        // as a side effect of a real caller-initiated write succeeding --
        // on a quiet consumer with no new events to write, the healthcheck
        // deadline clock never resets and the actor self-exits on a
        // perfectly healthy graph, purely from idleness. Re-arm on a fixed
        // interval, well under the healthcheck deadline (120s default),
        // so idle time alone can never trip it.
        drop(myself.send_interval(Duration::from_secs(30), || GraphControllerMsg::Tick));

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GraphControllerMsg::Op(op) => match handle_op(&state.graph, &op).await {
                // Nothing was sent, so nothing was learned about the graph.
                Ok(0) => {}
                Ok(_) => {
                    state.signal_port.send(GraphSignal::Available);
                }
                Err(e) => {
                    let reason = format!("{e}");
                    error!("Failed to handle graph operation. {reason}");
                    state.signal_port.send(GraphSignal::OpFailed(reason));
                }
            },
            GraphControllerMsg::Tick => {
                match state
                    .graph
                    .execute(Query::new("RETURN 1".to_string()))
                    .await
                {
                    Ok(_) => {
                        state.signal_port.send(GraphSignal::Available);
                        state.first_contact().await;
                    }
                    Err(e) => {
                        let reason = format!("canary tick failed: {e}");
                        error!("GraphControllerActor: {reason}");
                        state.signal_port.send(GraphSignal::OpFailed(reason));
                    }
                }
            }
            GraphControllerMsg::Query(query, reply) => {
                let result = handle_query(&state.graph, query).await;
                let _ = reply.send(result);
            }
        }
        Ok(())
    }
}

/// The rustls configuration for a client certificate whose key the custody daemon holds: the server verified against
/// the CA at `ca`, the certificate at `cert` presented, and every handshake signed through the daemon.
fn custody_tls(ca: &str, cert: &str, key: &ClientKey) -> Result<rustls::ClientConfig, String> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject as _;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_file_iter(ca)
        .map_err(|e| format!("reading the Neo4j CA from {ca}: {e}"))?
    {
        roots
            .add(certificate.map_err(|e| format!("the Neo4j CA at {ca}: {e}"))?)
            .map_err(|e| format!("the Neo4j CA at {ca}: {e}"))?;
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| format!("the Neo4j server verifier: {e}"))?;
    let chain = CertificateDer::pem_file_iter(cert)
        .and_then(|certificates| certificates.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("reading the client certificate from {cert}: {e}"))?;
    polar_custody::client::client_config(verifier, chain, key)
}

/// The planner, without a graph: a batch's rows carry back exactly what was written, and the grouping never moves a
/// write past one it depends on.
#[cfg(test)]
mod plan_tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    /// A value as the graph keeps it, with maps ordered, for comparing what went into a row with what came out.
    fn value(v: &BoltType) -> GraphValue {
        match v {
            BoltType::String(s) => GraphValue::String(s.value.clone()),
            BoltType::Integer(i) => GraphValue::I64(i.value),
            BoltType::Boolean(b) => GraphValue::Bool(b.value),
            BoltType::Float(f) => GraphValue::F64(f.value),
            BoltType::Null(_) => GraphValue::Null,
            BoltType::Bytes(b) => GraphValue::Bytes(b.value.to_vec()),
            BoltType::List(l) => GraphValue::List(l.value.iter().map(value).collect()),
            BoltType::Map(m) => {
                let mut pairs: Vec<(String, GraphValue)> = m
                    .value
                    .iter()
                    .map(|(k, v)| (k.value.clone(), value(v)))
                    .collect();
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                GraphValue::Map(pairs)
            }
            other => panic!("not a value a row holds: {other:?}"),
        }
    }

    /// A row's key values or properties, by name.
    type Fields = BTreeMap<String, GraphValue>;

    fn field(row: &BoltType, name: &str) -> Fields {
        let BoltType::Map(map) = row else {
            panic!("a row is a map: {row:?}")
        };
        let BoltType::Map(inner) = &map.value[&neo4rs::BoltString::from(name)] else {
            panic!("{name} is a map")
        };
        inner
            .value
            .iter()
            .map(|(k, v)| (k.value.clone(), value(v)))
            .collect()
    }

    /// The labels and key names a batch statement merges on, read from its text: `MERGE (n:A:B { k: row.k.k })`.
    fn merged_shape(cypher: &str) -> NodeShape {
        let line = cypher
            .lines()
            .find(|l| l.starts_with("MERGE (n"))
            .expect("a node batch");
        let inner = line.trim_start_matches("MERGE (n").trim_end_matches(')');
        let (labels, keys) = inner.split_once(" {").unwrap_or((inner, ""));
        NodeShape {
            labels: labels
                .split(':')
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            keys: keys
                .trim_end_matches('}')
                .split(',')
                .filter_map(|p| p.split_once(':').map(|(k, _)| k.trim().to_string()))
                .collect(),
        }
    }

    /// One node of a generated batch: a label from a few (two key shapes among them), its key values, its properties.
    type Item = (usize, Vec<String>, BTreeMap<String, GraphValue>);

    const LABELS: [(&str, &[&str]); 3] = [("Alpha", &["k"]), ("Beta", &["k", "j"]), ("Gamma", &[])];

    fn key_of((label, values, _): &Item) -> Box<dyn GraphNodeKey> {
        let (name, keys) = LABELS[*label];
        Box::new(TestKey(NodeIdentity::fixed(
            &[name],
            keys.iter()
                .zip(values)
                .map(|(k, v)| (*k, BoltType::String(v.clone().into())))
                .collect(),
        )))
    }

    #[derive(Debug)]
    struct TestKey(NodeIdentity);

    impl GraphNodeKey for TestKey {
        fn identity(&self) -> NodeIdentity {
            self.0.clone()
        }
    }

    fn any_value() -> impl Strategy<Value = GraphValue> {
        let leaf = prop_oneof![
            ".{0,8}".prop_map(GraphValue::String),
            any::<i64>().prop_map(GraphValue::I64),
            any::<bool>().prop_map(GraphValue::Bool),
            Just(GraphValue::Null),
            (-1.0e9f64..1.0e9).prop_map(GraphValue::F64),
        ];
        prop_oneof![
            4 => leaf,
            1 => prop::collection::vec(".{0,4}".prop_map(GraphValue::String), 0..3)
                .prop_map(GraphValue::List),
        ]
    }

    fn any_item() -> impl Strategy<Value = Item> {
        (
            0..LABELS.len(),
            prop::collection::vec("[a-z0-9]{0,4}", 2),
            prop::collection::btree_map("p[a-z]{0,3}", any_value(), 0..4),
        )
    }

    fn batch(items: &[Item]) -> GraphOp {
        GraphOp::UpsertNodes {
            items: items
                .iter()
                .map(|item| {
                    (
                        key_of(item),
                        item.2
                            .iter()
                            .map(|(k, v)| Property(k.clone(), v.clone()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn an_empty_batch_compiles_to_nothing() {
        assert!(
            compile_graph_op(&GraphOp::UpsertNodes { items: vec![] })
                .unwrap()
                .is_empty()
        );
        assert!(
            compile_graph_op(&GraphOp::Transaction { ops: vec![] })
                .unwrap()
                .is_empty()
        );
        assert!(
            compile_graph_op(&GraphOp::Transaction {
                ops: vec![GraphOp::UpsertNodes { items: vec![] }]
            })
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn a_thousand_nodes_of_one_shape_are_one_statement() {
        let items: Vec<Item> = (0..1000)
            .map(|i| (0, vec![i.to_string(), String::new()], BTreeMap::new()))
            .collect();
        let queries = compile_graph_op(&batch(&items)).unwrap();
        assert_eq!(queries.len(), 1);
        assert_eq!(
            queries[0].query(),
            "UNWIND $rows AS row\nMERGE (n:Alpha { k: row.k.k })\nSET n += row.p"
        );
    }

    /// The state anchor of a batch binds its variable too, with no key.
    #[test]
    fn a_node_with_no_key_merges_on_its_labels() {
        let items = vec![(2, vec![], BTreeMap::new())];
        let queries = compile_graph_op(&batch(&items)).unwrap();
        assert_eq!(
            queries[0].query(),
            "UNWIND $rows AS row\nMERGE (n:Gamma)\nSET n += row.p"
        );
    }

    #[test]
    fn a_property_name_the_graph_refuses_refuses_the_batch() {
        let mut props = BTreeMap::new();
        props.insert("x}) DETACH DELETE (m".to_string(), GraphValue::Null);
        assert!(compile_graph_op(&batch(&[(0, vec!["a".into(), "".into()], props)])).is_err());
    }

    proptest! {
        /// Every item of a batch comes back from the plan's rows: under its own labels and key names, with its key
        /// values and its properties, in the order it was given among the items of its shape; one statement a shape.
        #[test]
        fn a_batch_s_rows_carry_back_every_item(items in prop::collection::vec(any_item(), 0..40)) {
            let plan = plan(&batch(&items)).unwrap();
            let mut expected: BTreeMap<NodeShape, Vec<(Fields, Fields)>> = BTreeMap::new();
            for item in &items {
                let identity = key_of(item).identity();
                let keys = identity.keys.iter().map(|(k, v)| (k.clone(), value(v))).collect();
                expected.entry(identity.grouping()).or_default().push((keys, item.2.clone()));
            }
            let mut found = BTreeMap::new();
            for (cypher, params) in &plan.statements {
                let BoltType::List(rows) = &params[0].1 else { panic!("rows are a list") };
                let rows: Vec<_> = rows.value.iter().map(|r| (field(r, "k"), field(r, "p"))).collect();
                prop_assert!(found.insert(merged_shape(cypher), rows).is_none(), "one statement a shape");
            }
            prop_assert_eq!(found, expected);
            prop_assert_eq!(plan.placed.len(), items.len());
        }

        /// The plan is a function of the op: the same statements each time.
        #[test]
        fn a_plan_is_deterministic(items in prop::collection::vec(any_item(), 0..20)) {
            let a: Vec<String> = compile_graph_op(&batch(&items)).unwrap().iter().map(|q| q.query().to_string()).collect();
            let b: Vec<String> = compile_graph_op(&batch(&items)).unwrap().iter().map(|q| q.query().to_string()).collect();
            prop_assert_eq!(a, b);
        }
    }

    /// One op of a generated transaction.
    #[derive(Debug, Clone)]
    enum Step {
        Node(usize),
        Edge(usize, usize, bool),
        Barrier,
    }

    fn any_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            3 => (0..6usize).prop_map(Step::Node),
            3 => (0..6usize, 0..6usize, any::<bool>()).prop_map(|(a, b, r)| Step::Edge(a, b, r)),
            1 => Just(Step::Barrier),
        ]
    }

    /// Node `i`: two shapes among the six.
    fn node(i: usize) -> Box<dyn GraphNodeKey> {
        key_of(&(i % 2, vec![i.to_string(), "j".into()], BTreeMap::new()))
    }

    fn op_of(step: &Step) -> GraphOp {
        match step {
            Step::Node(i) => GraphOp::UpsertNode {
                key: node(*i),
                props: vec![Property("seen".into(), GraphValue::I64(*i as i64))],
            },
            Step::Edge(a, b, r) => GraphOp::EnsureEdge {
                from: node(*a),
                to: node(*b),
                rel_type: if *r { "R" } else { "S" }.to_string(),
                props: vec![],
            },
            Step::Barrier => GraphOp::RawMergeWrite {
                cypher: "MERGE (x:Barrier)".into(),
                params: vec![],
            },
        }
    }

    proptest! {
        /// However a transaction's ops are mixed and nested: every op is written once; a node is written before an
        /// edge that follows it and touches it; a barrier is written after every op before it and before every op
        /// after it; and there are never more statements than ops.
        #[test]
        fn grouping_keeps_what_depends_on_order_in_order(
            steps in prop::collection::vec(any_step(), 0..40),
            nest in (0..40usize, 0..40usize),
        ) {
            let mut ops: Vec<GraphOp> = steps.iter().map(op_of).collect();
            // A contiguous run of them nested in an inner transaction, which flattens in place.
            let (lo, hi) = (nest.0.min(nest.1).min(ops.len()), nest.0.max(nest.1).min(ops.len()));
            let inner: Vec<GraphOp> = ops.drain(lo..hi).collect();
            ops.insert(lo, GraphOp::Transaction { ops: inner });
            let plan = plan(&GraphOp::Transaction { ops }).unwrap();

            prop_assert_eq!(plan.placed.len(), steps.len());
            prop_assert!(plan.statements.len() <= steps.len());
            prop_assert!(plan.placed.iter().all(|s| *s < plan.statements.len()));
            for (i, earlier) in steps.iter().enumerate() {
                for (j, later) in steps.iter().enumerate().skip(i + 1) {
                    let (si, sj) = (plan.placed[i], plan.placed[j]);
                    match (earlier, later) {
                        (Step::Barrier, _) | (_, Step::Barrier) => prop_assert!(si < sj, "{i} {j}"),
                        (Step::Node(x), Step::Edge(a, b, _)) if x == a || x == b => {
                            prop_assert!(si < sj, "node {x} at {i} before its edge at {j}")
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;
    use crate::graph::environment::GraphSettings;
    use std::collections::HashMap;

    fn settings() -> GraphSettings {
        let env: HashMap<String, String> = [
            ("GRAPH_DB", "neo4j"),
            ("GRAPH_USER", "neo4j"),
            ("GRAPH_PASSWORD", "pw"),
            ("GRAPH_ENDPOINT", "bolt://given-by-the-builder:7687"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        GraphSettings::from_env(&env).unwrap()
    }

    fn args() -> GraphControllerArgs {
        GraphControllerArgs::new(std::sync::Arc::new(ractor::OutputPort::default()))
    }

    // The args hold an output port, whose fan-out task (ractor's `output-port-v2`) needs a runtime.
    #[tokio::test]
    async fn a_controller_given_its_settings_builds_its_connection_from_them() {
        // A developer shell may export GRAPH_ variables, so what shows which source was used
        // is the endpoint: only the settings given name this one.
        let config =
            GraphControllerActor::neo_config_for_args(&args().with_settings(settings())).unwrap();
        let shown = format!("{config:?}");
        assert!(shown.contains("given-by-the-builder"), "{shown}");
    }

    // The args hold an output port, whose fan-out task (ractor's `output-port-v2`) needs a runtime.
    #[tokio::test]
    async fn a_controller_given_nothing_reads_the_process_as_it_always_did() {
        // The legacy path for agents not yet on the builder. Pinned by what it says when the
        // process has none of the variables, which names them.
        if std::env::var("GRAPH_ENDPOINT").is_err() {
            let error = GraphControllerActor::neo_config_for_args(&args()).unwrap_err();
            assert!(error.to_string().contains("GRAPH_ENDPOINT"), "{error}");
        }
    }
}

// docs/architecture/key-custody.md, "The neo4rs patch": a graph writer whose key is the custody daemon's shows Neo4j the
// daemon's certificate, through the rustls configuration neo4rs takes. A stand-in for Neo4j that speaks TLS 1.3 and
// demands a client certificate, since container tests cannot publish ports on every host.
#[cfg(test)]
mod custody_tests {
    use super::*;
    use crate::graph::environment::{GraphSettings, GraphTls};
    use cassini_client::ClientKey;
    use polar_custody::client::CustodyClient;
    use polar_custody::custody::Custody;
    use polar_custody::protocol::Slot;
    use polar_custody::server::{Callers, bind, serve};
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::os::unix::fs::MetadataExt as _;
    use std::sync::{Arc, RwLock};

    const IDENTITY: &str = "kube-consumer.polar.serviceaccount.cluster.local";

    #[tokio::test(flavor = "multi_thread")]
    async fn a_graph_writer_shows_neo4j_the_custody_daemon_s_certificate() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca_pem = ca_params.self_signed(&ca_key).unwrap().pem();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);

        // The daemon, and polar-init's part: its CSR certified and installed, the certificate and the CA as files.
        let dir = tempfile::tempdir().unwrap();
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        let socket = dir.path().join("custody.sock");
        tokio::spawn(serve(
            bind(&socket).unwrap(),
            Arc::new(RwLock::new(Custody::new(&ca_pem).unwrap())),
            Arc::new(Callers {
                installer_uid: uid,
                workload_uids: vec![uid],
            }),
        ));
        let daemon = CustodyClient::new(&socket);
        let csr =
            tokio::task::block_in_place(|| daemon.make_csr(Slot::Client, IDENTITY, &[])).unwrap();
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        request.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let cert_pem = request.signed_by(&issuer).unwrap().pem();
        tokio::task::block_in_place(|| daemon.install(Slot::Client, vec![cert_pem.clone()]))
            .unwrap();
        let (cert_path, ca_path) = (dir.path().join("cert.pem"), dir.path().join("ca.pem"));
        std::fs::write(&cert_path, &cert_pem).unwrap();
        std::fs::write(&ca_path, &ca_pem).unwrap();

        // The stand-in: TLS 1.3, a client certificate under the CA demanded, and the one shown kept.
        let server_key = rcgen::KeyPair::generate().unwrap();
        let mut server_params =
            rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        server_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
            .unwrap();
        let server_config =
            rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_client_cert_verifier(
                    rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                        .build()
                        .unwrap(),
                )
                .with_single_cert(
                    vec![server_cert.der().clone()],
                    PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes()).unwrap(),
                )
                .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
        let seen = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.map_err(|e| e.to_string())?;
            let peer = tls
                .get_ref()
                .1
                .peer_certificates()
                .ok_or("no client certificate")?;
            Ok::<_, String>(peer[0].clone())
        });

        let settings = GraphSettings {
            database: "neo4j".to_string(),
            user: "neo4j".to_string(),
            password: "pw".to_string(),
            endpoint: format!("bolt+s://localhost:{port}"),
            tls: GraphTls::Mutual {
                ca: ca_path.display().to_string(),
                cert: cert_path.display().to_string(),
                key: ClientKey::Custody(socket.display().to_string()),
            },
        };
        let graph =
            Graph::connect(GraphControllerActor::neo_config_for(settings).unwrap()).unwrap();
        // The connection is made on first use. The stand-in speaks no Bolt, so the query fails after the handshake.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            graph.run(neo4rs::query("RETURN 1")),
        )
        .await;
        let shown = tokio::time::timeout(std::time::Duration::from_secs(10), seen)
            .await
            .expect("a handshake")
            .unwrap()
            .expect("the handshake, signed through the daemon");
        assert_eq!(
            shown.as_ref(),
            CertificateDer::from_pem_slice(cert_pem.as_bytes())
                .unwrap()
                .as_ref(),
            "the stand-in saw the daemon's certificate"
        );
    }
}
