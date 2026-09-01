//! Kubernetes consumer: projects observed cluster resources into the Polar
//! knowledge graph.
//!
//! Each resource's `GraphOperable` impl lives in its own sibling module,
//! named after the resource (`pod`, `job`, `namespace`, etc.). This file
//! holds only what's genuinely shared: the trait itself, and the handful of
//! helpers every impl leans on.
//!
//! # Projection philosophy
//!
//! Every impl here follows the same shape, in the same order:
//!
//!   1. anchor node upsert (identity + slow-changing descriptive props)
//!   2. ownership edges (OWNS, from owner references)
//!   3. state instance via `GraphOp::UpdateState` (the *only* way state is
//!      ever written — see below)
//!   4. structural edges derived from spec (volumes, secrets, containers)
//!
//! `UpdateState` is the single door for temporal state. It owns the
//! TRANSITIONED_TO edge (append-only history — every transition gets its
//! own instance, forever) and the OF_TYPE taxonomy edge on each new
//! instance, linking it to the shared, unvarying state-type anchor
//! (`KubeNodeKey::State`). There is no separate "current state" pointer
//! relationship, and deliberately so: `resource -[:TRANSITIONED_TO]->
//! instance` already carries the full history, and current state is just
//! whichever instance has the latest `valid_from` — a value that's easy to
//! derive and would drift from the truth immediately if instead
//! materialized as a separately-maintained edge.
//!
//! No impl may hand-roll state nodes with UpsertNode/EnsureEdge — doing so
//! skips writing a new TRANSITIONED_TO instance for that transition, so a
//! "what's deployed right now" query (latest TRANSITIONED_TO target by
//! valid_from) keeps returning the last real instance as current even
//! after deletion.
//!
//! # Timestamp convention
//!
//! Every temporal property written by this consumer is **milliseconds since
//! the Unix epoch, UTC, as a signed 64-bit integer** (`GraphValue::I64`).
//! No RFC3339 strings, no chrono Display output, no Neo4j temporals.
//! Kubernetes-attested times (condition transitions, container start/finish)
//! are converted at the boundary; observation times use `now_ms()`.
//!
//! Two property names, two meanings:
//!   - `valid_from`   — when the fact became true according to the *source*
//!                      (kubelet, controller condition). Falls back to
//!                      observation time only when the source doesn't attest
//!                      one, and that fallback is always commented at the
//!                      call site.
//!   - `observed_at`  — when this consumer ingested the fact. The delta from
//!                      `valid_from` is observer lag, a diagnostic.
//!
//! NOTE: `GraphController::UpdateState` previously injected `valid_from`
//! into state instances from the state key. This consumer now writes
//! `valid_from` explicitly as I64 in `state_instance_props`. There must be
//! exactly one writer — remove the controller-side injection when this
//! lands, or state instances will carry conflicting encodings.
//!
//! # Attestation ladder for container images
//!
//! `status.containerStatuses[].image_id` is the digest-pinned reference to
//! what the kubelet actually pulled and is running — not what was requested
//! in `spec.containers[].image`. The spec image is a tag; a tag can be
//! repointed in the registry between the kubelet's pull and any later
//! observation. Discovering from the tag lets the resolver's registry query
//! return a different digest than what is actually running, and the graph
//! would then attest a wrong artifact with full confidence — worse than no
//! attestation, since everything downstream inherits it.
//!
//! Policy: **we only emit OCIArtifactDiscovered when the kubelet has
//! reported an image_id.** While a container is Waiting, image_id is empty
//! and we defer — the watch redelivers the Pod when status changes, and
//! image_id is populated before the container can be Running, which is the
//! earliest moment any provenance query cares about it. A container that
//! never runs never gets a USES_IMAGE edge; that is honest, because nothing
//! executed. The requested tag is still recorded as the `image` property on
//! the PodContainer node so intent remains investigable.

use chrono::Utc;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;

use crate::supervisor::ProjectionCache;

mod deployment;
mod job;
mod kustomization;
mod namespace;
mod node;
mod oci_repository;
mod pod;
mod replicaset;

// ---------------------------------------------------------------------------
// Time helpers — the only place timestamps are constructed.
// ---------------------------------------------------------------------------

/// Current wall clock as milliseconds since the Unix epoch, UTC.
/// Use for `observed_at`, and as the documented fallback for `valid_from`
/// when the source does not attest a transition time.
fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Convert a k8s API `Time` (metav1.Time) to epoch milliseconds.
fn k8s_time_ms(t: &k8s_openapi::apimachinery::pkg::apis::meta::v1::Time) -> i64 {
    t.0.as_millisecond()
}

/// Parse an RFC3339 string timestamp (as carried by some Flux CRD status
/// fields) to epoch milliseconds. Returns None on parse failure — callers
/// decide the fallback and must comment it.
fn rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Shorthand for an epoch-ms temporal property.
fn ts(name: &str, ms: i64) -> Property {
    Property(name.into(), GraphValue::I64(ms))
}

/// Deterministic fingerprint of a state node's *meaningful* properties --
/// deliberately excludes valid_from/observed_at. Both are timestamps that
/// differ on every single observation for any resource whose valid_from
/// falls back to now_ms() (Namespace, Job, Deployment, ReplicaSet, Pod --
/// see the fallback comments throughout the sibling modules), so including
/// them in the signature would make it differ every time regardless of
/// whether the actual state changed, defeating suppression entirely for
/// exactly the resources that produce the most redundant writes on a
/// relist.
///
/// Relies on Property/GraphValue deriving Debug with stable, order-
/// preserving formatting -- true of every #[derive(Debug)] struct/enum,
/// and each call site builds its props in the same fixed order every time,
/// so this is a deterministic fingerprint of identical state.
fn state_signature(props: &[Property]) -> String {
    format!("{props:?}")
}

// ---------------------------------------------------------------------------
// Graph value helpers
// ---------------------------------------------------------------------------

fn opt_string(v: &Option<String>) -> GraphValue {
    match v {
        Some(s) => GraphValue::String(s.clone()),
        None => GraphValue::Null,
    }
}

fn opt_bool(v: &Option<bool>) -> GraphValue {
    match v {
        Some(b) => GraphValue::Bool(*b),
        None => GraphValue::Null,
    }
}

fn opt_string_vec(v: &Option<Vec<String>>) -> GraphValue {
    match v {
        Some(vec) => GraphValue::List(vec.iter().map(|s| GraphValue::String(s.clone())).collect()),
        None => GraphValue::Null,
    }
}

/// Serialize a complex struct wholesale as a JSON string property.
///
/// Serialization of k8s_openapi types cannot realistically fail (no
/// non-string map keys), but this runs inside an actor: a panic here is a
/// supervisor restart, and a restart loop on one weird object is a worse
/// failure mode than a null property. Degrade to Null instead of expecting.
fn opt_json<T: serde::Serialize>(v: &Option<T>) -> GraphValue {
    match v {
        Some(inner) => match serde_json::to_string(inner) {
            Ok(s) => GraphValue::String(s),
            Err(_) => GraphValue::Null,
        },
        None => GraphValue::Null,
    }
}

// ---------------------------------------------------------------------------
// Shared projection helpers
// ---------------------------------------------------------------------------

/// Applies to domain-identifiable entities, not arbitrary spec fragments.
pub trait GraphOperable {
    /// Project this resource's identity, ownership, state, and structural
    /// edges into the graph. Must be idempotent: the watch redelivers.
    ///
    /// `cluster_uid` is the UID of the kube-system namespace in the cluster
    /// this resource was observed in (issue #236). Every KubeNodeKey this
    /// impl constructs must be scoped with it.
    ///
    /// `cache` gates the state-write specifically: impls check
    /// ProjectionCache::should_emit before casting GraphOp::UpdateState and
    /// skip the write on Suppress, so a relist of unchanged resources
    /// doesn't produce a new state node per observation. Anchor upserts and
    /// structural edges (owner refs, IN_NAMESPACE, etc.) are unaffected --
    /// UpsertNode/EnsureEdge are already idempotent MERGEs, so there's
    /// nothing to suppress there.
    fn project_into_graph(
        self,
        graph: &GraphController,
        tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr>;

    /// Record the resource's deletion as a terminal state transition.
    /// Deletion is a state, not an erasure -- anchor nodes and history are
    /// never removed, per the append-only provenance model.
    ///
    /// `cluster_uid` must match whatever project_into_graph originally
    /// scoped this resource's key with, or the delete matches nothing.
    ///
    /// `cache` must be evicted here, using the exact same (kind, uid) this
    /// resource's project_into_graph used with should_emit -- the deletion
    /// state above is written once and is final, so there is no future
    /// observation left to suppress against. Leaving the entry in place
    /// after this point only costs memory for the rest of this process's
    /// life.
    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr>;
}

fn handle_owner_refs(
    owners: &[OwnerReference],
    node_key: KubeNodeKey,
    graph: &GraphController,
    cluster_uid: &str,
) -> Result<(), ActorProcessingErr> {
    for owner in owners {
        if let Some(owner_key) = KubeNodeKey::from_owner_reference(owner, cluster_uid) {
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: owner_key.into_key(),
                rel_type: "OWNS".into(),
                to: node_key.clone().into_key(),
                props: Vec::new(),
            }))?;
        }
    }

    Ok(())
}

/// Standardized terminal state write for deletions.
///
/// Every project_delete funnels through here so that (a) the phase is
/// always exactly "Deleted", (b) the write goes through UpdateState and
/// therefore records a real TRANSITIONED_TO instance for the deletion —
/// the previous hand-rolled UpsertNode+EnsureEdge paths wrote no such
/// instance, so a "what's deployed right now" query (latest
/// TRANSITIONED_TO target by valid_from) kept returning the last live
/// instance as current even after deletion — and (c) no unattested values
/// (e.g. fabricated zero replica counts) are recorded. Deletion attests
/// exactly one fact: the resource is gone.
fn project_deletion_state(
    graph: &GraphController,
    resource_key: KubeNodeKey,
    state_instance_key: KubeNodeKey,
    deleted_at_ms: i64,
) -> Result<(), ActorProcessingErr> {
    graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
        resource_key: resource_key.into_key(),
        state_type_key: KubeNodeKey::State.into_key(),
        state_instance_key: state_instance_key.into_key(),
        state_instance_props: vec![
            Property("phase".into(), GraphValue::String("Deleted".into())),
            // Deletion has no source-attested transition time — the watch
            // event itself is the attestation. valid_from == observed_at
            // here by construction.
            ts("valid_from", deleted_at_ms),
            ts("observed_at", deleted_at_ms),
        ],
    }))?;

    Ok(())
}
