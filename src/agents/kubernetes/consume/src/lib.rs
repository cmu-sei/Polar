//! Kubernetes consumer: projects observed cluster resources into the Polar
//! knowledge graph.
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
//! TRANSITIONED_TO edge, the OF_TYPE taxonomy edge, and the HAS_STATE
//! current-status pointer. No impl may hand-roll state nodes with
//! UpsertNode/EnsureEdge — doing so bypasses the HAS_STATE pointer
//! maintenance and leaves deleted resources appearing live to any
//! "what's deployed right now" query.
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

use std::collections::HashMap;

use chrono::Utc;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::ContainerStatus;
use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::OwnerReference};
use kube_common::flux::{kustomization::Kustomization, oci_repositories::OciRepository};
use neo4rs::BoltType;
use polar::cassini::{CassiniClient, TcpClient};
use polar::graph::controller::IntoGraphKey;
use polar::{DiscoverySourceRef, emit_provenance_event};
use polar::{
    ProvenanceEvent,
    graph::{
        controller::{
            GraphController, GraphControllerMsg, GraphOp, GraphValue, NULL_FIELD, Property,
        },
        nodes::kube::KubeNodeKey,
    },
};
use ractor::{ActorProcessingErr, ActorRef};

pub mod supervisor;

pub const BROKER_CLIENT_NAME: &str = "kubernetes.cluster.cassini.client";

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
    fn project_into_graph(
        self,
        graph: &GraphController,
        tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr>;

    /// Record the resource's deletion as a terminal state transition.
    /// Deletion is a state, not an erasure -- anchor nodes and history are
    /// never removed, per the append-only provenance model.
    ///
    /// `cluster_uid` must match whatever project_into_graph originally
    /// scoped this resource's key with, or the delete matches nothing.
    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
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
/// therefore updates the HAS_STATE current-status pointer — the previous
/// hand-rolled UpsertNode+EnsureEdge paths bypassed that pointer and left
/// deleted resources appearing live to current-state queries — and (c) no
/// unattested values (e.g. fabricated zero replica counts) are recorded.
/// Deletion attests exactly one fact: the resource is gone.
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

// ---------------------------------------------------------------------------
// Job
// ---------------------------------------------------------------------------

impl GraphOperable for Job {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        // A resource without a uid has no identity to project. Defaulting to
        // "" would merge every uid-less resource into one node keyed on the
        // empty string — silently corrupting, so skip instead.
        // TODO: emit a tracing::warn! here once logging is wired into this
        // crate; a uid-less watch event is worth knowing about.
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        // Surface the cyclops.build/id label so the Cyclops graph processor
        // can write EXECUTED_IN edges by uid without knowing it came from k8s.
        // Other labels are not individually surfaced — they're noise at this level.
        let cyclops_build_id = self
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("cyclops.build/id"))
            .cloned()
            .unwrap_or_default();

        let job_key = KubeNodeKey::Job {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: job_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                Property("namespace".into(), GraphValue::String(namespace.clone())),
                Property(
                    "cyclops_build_id".into(),
                    GraphValue::String(cyclops_build_id),
                ),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- Owner refs (e.g. CronJob owns Job) ----

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, job_key.clone(), graph, cluster_uid)?;
        }

        // ---- State ----

        let status = self.status.as_ref();

        let active = status.and_then(|s| s.active).unwrap_or(0);
        let succeeded = status.and_then(|s| s.succeeded).unwrap_or(0);
        let failed = status.and_then(|s| s.failed).unwrap_or(0);

        // Derive a human-readable phase from the same conditions the
        // orchestrator's interpret_job_status uses, for consistency.
        let phase = if succeeded > 0 {
            "Succeeded"
        } else if failed > 0 && active == 0 {
            "Failed"
        } else if active > 0 {
            "Running"
        } else {
            "Pending"
        };

        let failure_reason = status
            .and_then(|s| s.conditions.as_ref())
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Failed"))
            .and_then(|c| c.message.clone())
            .unwrap_or_default();

        // completion_time is k8s-attested; a job still running has none, so
        // fall back to observation time for the interim state instance. The
        // terminal (completed) observation will carry the attested time.
        let valid_from_ms = status
            .and_then(|s| s.completion_time.as_ref())
            .map(k8s_time_ms)
            .unwrap_or_else(now_ms);

        let state_key = KubeNodeKey::JobState {
            uid: uid.clone(),
            // State keys embed the same epoch-ms value as valid_from so the
            // key remains deterministic and collision-free per transition.
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: job_key.clone().into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: state_key.into_key(),
            state_instance_props: vec![
                Property("phase".into(), GraphValue::String(phase.into())),
                Property("active".into(), GraphValue::I64(active as i64)),
                Property("succeeded".into(), GraphValue::I64(succeeded as i64)),
                Property("failed".into(), GraphValue::I64(failed as i64)),
                Property("failure_reason".into(), GraphValue::String(failure_reason)),
                ts("valid_from", valid_from_ms),
                ts("observed_at", now_ms()),
            ],
        }))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        project_deletion_state(
            graph,
            KubeNodeKey::Job {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::JobState {
                uid,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )
    }
}

// ---------------------------------------------------------------------------
// Pod
// ---------------------------------------------------------------------------

/// Index kubelet-reported container statuses by container name, covering
/// BOTH regular and init containers.
///
/// The previous implementation built its image_id lookup from
/// `container_statuses` only, while the projection loop below iterates
/// `spec.containers` chained with `spec.init_containers` — so init
/// containers could never receive kubelet attestation even when their
/// imageID was present in `init_container_statuses`. Init containers are a
/// classic supply-chain injection point; they get the same attestation
/// ladder as everything else. Container names are unique across both lists
/// (API-enforced), so a single map is safe.
///
/// Ephemeral containers are intentionally out of scope for now.
fn container_status_index(pod: &Pod) -> HashMap<String, ContainerStatus> {
    pod.status
        .as_ref()
        .map(|s| {
            s.container_statuses
                .clone()
                .unwrap_or_default()
                .into_iter()
                .chain(s.init_container_statuses.clone().unwrap_or_default())
                .map(|cs| (cs.name.clone(), cs))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract (phase, valid_from_ms, state props) from a kubelet container
/// status. The valid_from source differs by state, and that difference is
/// the point:
///
///   - Running:    `state.running.started_at` — kubelet-attested. This is
///                 the timestamp lead-time queries anchor t_final on.
///   - Terminated: `state.terminated.finished_at` — kubelet-attested.
///   - Waiting:    no attested transition time exists; observation time is
///                 the honest fallback and the phase makes that legible.
fn container_lifecycle_state(cs: &ContainerStatus) -> (&'static str, i64, Vec<Property>) {
    if let Some(waiting) = cs.state.as_ref().and_then(|s| s.waiting.as_ref()) {
        (
            "Waiting",
            now_ms(),
            vec![
                Property(
                    "reason".into(),
                    GraphValue::String(waiting.reason.clone().unwrap_or_default()),
                ),
                Property(
                    "message".into(),
                    GraphValue::String(waiting.message.clone().unwrap_or_default()),
                ),
                Property(
                    "restart_count".into(),
                    GraphValue::I64(cs.restart_count as i64),
                ),
            ],
        )
    } else if let Some(running) = cs.state.as_ref().and_then(|s| s.running.as_ref()) {
        (
            "Running",
            running
                .started_at
                .as_ref()
                .map(k8s_time_ms)
                .unwrap_or_else(now_ms),
            vec![
                Property(
                    "started".into(),
                    GraphValue::Bool(cs.started.unwrap_or(false)),
                ),
                Property("ready".into(), GraphValue::Bool(cs.ready)),
                Property(
                    "restart_count".into(),
                    GraphValue::I64(cs.restart_count as i64),
                ),
            ],
        )
    } else if let Some(term) = cs.state.as_ref().and_then(|s| s.terminated.as_ref()) {
        (
            "Terminated",
            term.finished_at
                .as_ref()
                .map(k8s_time_ms)
                .unwrap_or_else(now_ms),
            vec![
                Property("exit_code".into(), GraphValue::I64(term.exit_code as i64)),
                Property(
                    "reason".into(),
                    GraphValue::String(term.reason.clone().unwrap_or_default()),
                ),
                Property(
                    "restart_count".into(),
                    GraphValue::I64(cs.restart_count as i64),
                ),
            ],
        )
    } else {
        (
            NULL_FIELD,
            now_ms(),
            vec![Property(
                "restart_count".into(),
                GraphValue::I64(cs.restart_count as i64),
            )],
        )
    }
}

impl GraphOperable for Pod {
    fn project_into_graph(
        self,
        graph: &GraphController,
        tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };

        let pod_name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        let sa_name = self
            .spec
            .as_ref()
            .and_then(|s| s.service_account_name.clone())
            .unwrap_or_default();

        let pod_key = KubeNodeKey::Pod {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----
        //
        // Anchor before state: UpdateState may MERGE the resource node, but
        // ordering anchor-first keeps this impl consistent with every other
        // impl in this file and independent of controller MERGE semantics.

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: pod_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(pod_name)),
                Property("namespace".into(), GraphValue::String(namespace.clone())),
                Property("sa_name".into(), GraphValue::String(sa_name)),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- Owner refs ----

        if let Some(owners) = self.metadata.owner_references.clone() {
            handle_owner_refs(&owners, pod_key.clone(), graph, cluster_uid)?;
        }

        // ---- Pod state ----
        //
        // k8s does not attest a transition time for pod *phase* directly
        // (conditions carry lastTransitionTime, phase does not), so
        // observation time is the honest valid_from here. If finer fidelity
        // is ever needed, the max of condition lastTransitionTimes is a
        // defensible approximation — deliberately not done silently now.

        let phase = self
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| NULL_FIELD.into());

        let ready = self
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .map(|conds| {
                conds
                    .iter()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
            })
            .unwrap_or(false);

        let pod_state_ms = now_ms();
        let new_state_key = KubeNodeKey::PodState {
            pod_uid: uid.clone(),
            valid_from: pod_state_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: pod_key.clone().into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: new_state_key.into_key(),
            state_instance_props: vec![
                Property("phase".into(), GraphValue::String(phase)),
                Property("ready".into(), GraphValue::Bool(ready)),
                // Previously absent on PodState instances — valid_from
                // existed only inside the state key, making the temporal
                // model unqueryable for pods. Now explicit, like every
                // other state instance.
                ts("valid_from", pod_state_ms),
                ts("observed_at", pod_state_ms),
            ],
        }))?;

        let Some(spec) = self.spec.clone() else {
            // No spec: identity and state are recorded; nothing structural
            // to project.
            return Ok(());
        };

        // ---- Volumes ----

        if let Some(volumes) = spec.volumes {
            for volume in volumes {
                let vol_key = KubeNodeKey::Volume {
                    name: volume.name.clone(),
                    namespace: namespace.clone(),
                    cluster_uid: cluster_uid.to_string(),
                };

                // TODO: pretty much every other field for volumes are optional, but there are surely some things we're gonna want to know about them
                // // What cloud resources are they pointing to? Where are they in the host path,
                // // We don't want to just blast the yaml structure in the graph, but we're gonna have to keep this in mind

                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: vol_key.clone().into_key(),
                    props: Vec::new(),
                }))?;

                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: pod_key.clone().into_key(),
                    rel_type: "USES_VOLUME".into(),
                    to: vol_key.clone().into_key(),
                    props: Vec::new(),
                }))?;

                if let Some(cm) = volume.config_map {
                    let cm_key = KubeNodeKey::ConfigMap {
                        name: cm.name,
                        namespace: namespace.clone(),
                        cluster_uid: cluster_uid.to_string(),
                    };

                    graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: cm_key.clone().into_key(),
                        props: Vec::new(),
                    }))?;

                    graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                        from: vol_key.clone().into_key(),
                        rel_type: "BACKED_BY".into(),
                        to: cm_key.into_key(),
                        props: Vec::new(),
                    }))?;
                }

                if let Some(secret) = volume.secret
                    && let Some(secret_name) = secret.secret_name
                {
                    let s_key = KubeNodeKey::Secret {
                        name: secret_name,
                        namespace: namespace.clone(),
                        cluster_uid: cluster_uid.to_string(),
                    };

                    graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: s_key.clone().into_key(),
                        props: Vec::new(),
                    }))?;

                    graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                        from: vol_key.clone().into_key(),
                        rel_type: "BACKED_BY".into(),
                        to: s_key.into_key(),
                        props: Vec::new(),
                    }))?;
                }

                if let Some(pvc) = volume.persistent_volume_claim {
                    let pvc_key = KubeNodeKey::PersistentVolumeClaim {
                        name: pvc.claim_name,
                        namespace: namespace.clone(),
                        cluster_uid: cluster_uid.to_string(),
                    };

                    graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: pvc_key.clone().into_key(),
                        props: Vec::new(),
                    }))?;

                    graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                        from: vol_key.into_key(),
                        rel_type: "BACKED_BY".into(),
                        to: pvc_key.into_key(),
                        props: Vec::new(),
                    }))?;
                }
            }
        }

        // ---- Image pull secrets ----
        //
        // These are secret consumption too: any pod that can pull with them
        // can exfiltrate registry credentials. Without this edge, the
        // secret-exposure query silently under-reports.

        if let Some(pull_secrets) = spec.image_pull_secrets {
            for ps in pull_secrets {
                // NOTE: if your k8s_openapi version types
                // LocalObjectReference.name as Option<String>, adapt this
                // to unwrap it — the selector names elsewhere in this file
                // are String in the pinned version.
                let s_key = KubeNodeKey::Secret {
                    name: ps.name,
                    namespace: namespace.clone(),
                    cluster_uid: cluster_uid.to_string(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: s_key.clone().into_key(),
                    props: Vec::new(),
                }))?;

                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: pod_key.clone().into_key(),
                    rel_type: "USES_SECRET".into(),
                    to: s_key.into_key(),
                    props: Vec::new(),
                }))?;
            }
        }

        // ---- Containers ----

        // Build a lookup of container_name -> kubelet-reported status before
        // iterating containers. status.containerStatuses[].image_id is the
        // digest-pinned reference to what the kubelet actually pulled and is
        // running — not what was requested in spec.containers[].image.
        //
        // This closes a TOCTOU window: spec.image is a tag, and a tag can be
        // repointed in the registry between the kubelet's pull and any later
        // observation. If we discover from the tag, the resolver's registry
        // query can return a different digest than what is actually running,
        // and the graph would attest a wrong artifact with full confidence —
        // worse than no attestation, since everything downstream inherits it.
        //
        // image_id is empty string while a container is Waiting (not yet
        // pulled). Those containers get NO discovery event — see module docs
        // ("Attestation ladder"). The watch redelivers this Pod when status
        // changes, and image_id is populated before Running, which is the
        // earliest point any provenance query cares. Emitting a tag-based
        // discovery here would create a wrong-attestation window plus a
        // stale-edge reconciliation problem when the kubelet-attested edge
        // arrives, for no query-visible benefit.
        //
        // This index also serves lifecycle-state extraction below, replacing
        // a per-container clone of both status vectors (O(containers ×
        // statuses)) with one O(statuses) pass.

        let status_index = container_status_index(&self);

        let containers = spec
            .containers
            .into_iter()
            .chain(spec.init_containers.unwrap_or_default());

        for container in containers {
            // A container without a spec image cannot be projected as an
            // image consumer; skip (required-in-practice, optional-in-type).
            let Some(image_uri) = container.image.clone() else {
                continue;
            };

            let container_key = KubeNodeKey::PodContainer {
                pod_uid: uid.clone(),
                name: container.name.clone(),
                cluster_uid: cluster_uid.to_string(),
            };

            let props = vec![
                Property("name".into(), GraphValue::String(container.name.clone())),
                // The *requested* image reference (tag) — recorded as intent,
                // never used for attestation. See module docs.
                Property("image".into(), GraphValue::String(image_uri.clone())),
                Property(
                    "image_pull_policy".into(),
                    opt_string(&container.image_pull_policy),
                ),
                Property(
                    "restart_policy".into(),
                    opt_string(&container.restart_policy),
                ),
                Property("working_dir".into(), opt_string(&container.working_dir)),
                Property("stdin".into(), opt_bool(&container.stdin)),
                Property("stdin_once".into(), opt_bool(&container.stdin_once)),
                Property("tty".into(), opt_bool(&container.tty)),
                Property(
                    "termination_message_path".into(),
                    opt_string(&container.termination_message_path),
                ),
                Property(
                    "termination_message_policy".into(),
                    opt_string(&container.termination_message_policy),
                ),
                Property("args".into(), opt_string_vec(&container.args)),
                Property("command".into(), opt_string_vec(&container.command)),
                // These are complex structs — serialize them wholesale
                Property("env".into(), opt_json(&container.env)),
                Property("env_from".into(), opt_json(&container.env_from)),
                Property("ports".into(), opt_json(&container.ports)),
                Property("resources".into(), opt_json(&container.resources)),
                Property("resize_policy".into(), opt_json(&container.resize_policy)),
                Property(
                    "security_context".into(),
                    opt_json(&container.security_context),
                ),
                Property("lifecycle".into(), opt_json(&container.lifecycle)),
                Property("liveness_probe".into(), opt_json(&container.liveness_probe)),
                Property(
                    "readiness_probe".into(),
                    opt_json(&container.readiness_probe),
                ),
                Property("startup_probe".into(), opt_json(&container.startup_probe)),
                Property("volume_devices".into(), opt_json(&container.volume_devices)),
                Property("volume_mounts".into(), opt_json(&container.volume_mounts)),
            ];

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: container_key.clone().into_key(),
                props,
            }))?;

            if let Some(mounts) = container.volume_mounts.clone() {
                for mount in mounts {
                    let volume_key = KubeNodeKey::Volume {
                        name: mount.name.clone(),
                        namespace: namespace.clone(),
                        cluster_uid: cluster_uid.to_string(),
                    };

                    let op = GraphOp::EnsureEdge {
                        from: container_key.clone().into_key(),
                        to: volume_key.into_key(),
                        rel_type: "USES_VOLUME".into(),
                        props: vec![
                            // Mount-specific metadata belongs on the edge.
                            // This is important: the same volume can be mounted
                            // differently by different containers.
                            Property(
                                "mount_path".into(),
                                GraphValue::String(mount.mount_path.clone()),
                            ),
                            Property(
                                "read_only".into(),
                                GraphValue::Bool(mount.read_only.unwrap_or(false)),
                            ),
                            Property("name".into(), GraphValue::String(mount.name.clone())),
                            ts("observed_at", now_ms()),
                        ],
                    };

                    graph.cast(GraphControllerMsg::Op(op))?;
                }
            }

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: pod_key.clone().into_key(),
                rel_type: "HAS_CONTAINER".into(),
                to: container_key.clone().into_key(),
                props: Vec::new(),
            }))?;

            // ---- Image discovery: kubelet-attested only ----
            //
            // Emit if and only if the kubelet has reported an image_id for
            // this container (regular OR init — the index covers both).
            //
            // Contract with the resolver: `image_id` is the authoritative
            // digest pin; `uri` is the fetch path (the spec tag, which is
            // what's reliably pullable). If the registry resolution of `uri`
            // yields a digest that disagrees with `image_id`, the resolver
            // must trust the kubelet and flag the disagreement — that
            // mismatch IS a tag repoint being observed live. Resolver must
            // also parse image_id defensively: containerd yields
            // `repo@sha256:...`, locally-loaded images can yield a bare
            // `sha256:...`, and legacy runtimes prefixed `docker-pullable://`.
            //
            // Emission recurs on every observation; discovery is idempotent
            // by design (resolver MERGEs on digest).
            match status_index
                .get(&container.name)
                .filter(|cs| !cs.image_id.is_empty())
            {
                Some(cs) => {
                    // discovery_ref is what we emit for resolution AND what
                    // we key the PodContainer node on — both must agree,
                    // otherwise the USES_IMAGE join in the resolution
                    // handler will never match.
                    let source_ref = DiscoverySourceRef::KubernetesPodContainer {
                        pod_uid: uid.clone(),
                        image_id: Some(cs.image_id.clone()),
                        container_name: container.name.clone(),
                        cluster_uid: cluster_uid.to_string(),
                    };

                    emit_provenance_event(
                        ProvenanceEvent::OCIArtifactDiscovered {
                            uri: image_uri.clone(),
                            source_ref,
                        },
                        tcp_client,
                    )?;
                }
                None => {
                    // Deliberately no fallback to the spec tag. The pod will
                    // be redelivered when the kubelet populates image_id;
                    // until then this container has run nothing, so there is
                    // no runtime provenance to attest.
                }
            }

            // ---- Container lifecycle ----

            if let Some(cs) = status_index.get(&container.name) {
                let (state_type, valid_from_ms, mut state_props) = container_lifecycle_state(cs);

                // Deterministic state instance key.
                // Still keyed on (pod_uid, container name, valid_from) —
                // lifecycle state tracking is orthogonal to image discovery
                // and does not need to change.
                let state_instance_key = KubeNodeKey::PodContainerState {
                    pod_uid: uid.clone(),
                    name: container.name.clone(),
                    valid_from: valid_from_ms.to_string(),
                    cluster_uid: cluster_uid.to_string(),
                }
                .into_key();

                state_props.push(Property(
                    "phase".into(),
                    GraphValue::String(state_type.into()),
                ));
                // Previously absent as a property (key-only) — the lead-time
                // query's `min(s.valid_from)` depends on this being an
                // explicit, uniformly-encoded value.
                state_props.push(ts("valid_from", valid_from_ms));
                state_props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: container_key.clone().into_key(),
                    state_type_key: KubeNodeKey::State.into_key(), // abstract taxonomy
                    state_instance_key,
                    state_instance_props: state_props,
                }))?;
            }

            // ---- Env-sourced config/secret consumption ----

            if let Some(envs) = container.env {
                for env in envs {
                    if let Some(value_from) = env.value_from {
                        if let Some(cm_ref) = value_from.config_map_key_ref {
                            let cm_key = KubeNodeKey::ConfigMap {
                                name: cm_ref.name,
                                namespace: namespace.clone(),
                                cluster_uid: cluster_uid.to_string(),
                            };

                            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                                from: pod_key.clone().into_key(),
                                rel_type: "USES_CONFIGMAP".into(),
                                to: cm_key.into_key(),
                                props: Vec::new(),
                            }))?;
                        }

                        if let Some(secret_ref) = value_from.secret_key_ref {
                            let s_key = KubeNodeKey::Secret {
                                name: secret_ref.name,
                                namespace: namespace.clone(),
                                cluster_uid: cluster_uid.to_string(),
                            };

                            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                                from: pod_key.clone().into_key(),
                                rel_type: "USES_SECRET".into(),
                                to: s_key.into_key(),
                                props: Vec::new(),
                            }))?;
                        }
                    }
                }
            }

            // ---- envFrom-sourced config/secret consumption ----
            //
            // envFrom imports an entire ConfigMap/Secret as environment.
            // Previously this was only serialized as a JSON blob on the
            // container node, which made it invisible to the USES_SECRET /
            // USES_CONFIGMAP exposure queries — a silent gap in exactly the
            // blast-radius question those edges exist to answer.
            if let Some(env_from) = container.env_from {
                for source in env_from {
                    if let Some(cm_ref) = source.config_map_ref {
                        let cm_key = KubeNodeKey::ConfigMap {
                            name: cm_ref.name,
                            namespace: namespace.clone(),
                            cluster_uid: cluster_uid.to_string(),
                        };

                        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                            from: pod_key.clone().into_key(),
                            rel_type: "USES_CONFIGMAP".into(),
                            to: cm_key.into_key(),
                            props: Vec::new(),
                        }))?;
                    }

                    if let Some(secret_ref) = source.secret_ref {
                        let s_key = KubeNodeKey::Secret {
                            name: secret_ref.name,
                            namespace: namespace.clone(),
                            cluster_uid: cluster_uid.to_string(),
                        };

                        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                            from: pod_key.clone().into_key(),
                            rel_type: "USES_SECRET".into(),
                            to: s_key.into_key(),
                            props: Vec::new(),
                        }))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        // Goes through UpdateState (via the shared helper) rather than the
        // previous hand-rolled UpsertNode + EnsureEdge — the manual path
        // never updated the HAS_STATE current-status pointer, so deleted
        // pods appeared to be in their last live state to any current-state
        // query. Deletion is a state transition like any other.
        project_deletion_state(
            graph,
            KubeNodeKey::Pod {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::PodState {
                pod_uid: uid,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )

        // NOTE: container state instances are deliberately NOT transitioned
        // here. The kubelet reports Terminated states for containers via the
        // normal status path before/at pod deletion; fabricating terminal
        // container states from the delete event would write unattested
        // values.
    }
}

// ---------------------------------------------------------------------------
// Deployment
// ---------------------------------------------------------------------------

impl GraphOperable for Deployment {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        let status = self.status.unwrap_or_default();

        let available = status.available_replicas.unwrap_or(0);
        let updated = status.updated_replicas.unwrap_or(0);
        let unavailable = status.unavailable_replicas.unwrap_or(0);

        let progressing_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .find(|c| c.type_ == "Progressing")
                    .map(|c| c.status.clone())
            })
            .unwrap_or_else(|| NULL_FIELD.into());

        let available_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .find(|c| c.type_ == "Available")
                    .map(|c| c.status.clone())
            })
            .unwrap_or_else(|| NULL_FIELD.into());

        // Prefer the k8s-attested transition time of the most recently
        // transitioned condition as valid_from; fall back to observation
        // time when no conditions exist yet.
        let valid_from_ms = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .filter_map(|c| c.last_transition_time.as_ref().map(k8s_time_ms))
                    .max()
            })
            .unwrap_or_else(now_ms);

        let deployment_key = KubeNodeKey::Deployment {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: deployment_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name)),
                Property("namespace".into(), GraphValue::String(namespace)),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- Owner refs ----

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, deployment_key.clone(), graph, cluster_uid)?;
        }

        // ---- Immutable DeploymentState ----

        let state_key = KubeNodeKey::DeploymentState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: deployment_key.clone().into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: state_key.into_key(),
            state_instance_props: vec![
                Property(
                    "available_replicas".into(),
                    GraphValue::I64(available as i64),
                ),
                Property("updated_replicas".into(), GraphValue::I64(updated as i64)),
                Property(
                    "unavailable_replicas".into(),
                    GraphValue::I64(unavailable as i64),
                ),
                Property(
                    "progressing_condition".into(),
                    GraphValue::String(progressing_condition),
                ),
                Property(
                    "available_condition".into(),
                    GraphValue::String(available_condition),
                ),
                ts("valid_from", valid_from_ms),
                ts("observed_at", now_ms()),
            ],
        }))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        // The previous implementation wrote a final ordinary state (replica
        // counts, conditions) with no "Deleted" phase — so a deleted
        // Deployment was indistinguishable from a live one that happened to
        // stop receiving events. Deletion is now recorded as the terminal
        // phase, and no replica counts are fabricated for it.
        project_deletion_state(
            graph,
            KubeNodeKey::Deployment {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::DeploymentState {
                cluster_uid: cluster_uid.to_string(),
                uid,
                valid_from: now.to_string(),
            },
            now,
        )
    }
}

// ---------------------------------------------------------------------------
// ReplicaSet
// ---------------------------------------------------------------------------

impl GraphOperable for ReplicaSet {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        let status = self.status.unwrap_or_default();

        let replicas = status.replicas;
        let ready = status.ready_replicas.unwrap_or(0);
        let available = status.available_replicas.unwrap_or(0);

        // ReplicaSet conditions are sparse in practice; observation time is
        // the usual valid_from here, upgraded to the attested condition time
        // when one exists.
        let valid_from_ms = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .filter_map(|c| c.last_transition_time.as_ref().map(k8s_time_ms))
                    .max()
            })
            .unwrap_or_else(now_ms);

        let rs_key = KubeNodeKey::ReplicaSet {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: rs_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name)),
                Property("namespace".into(), GraphValue::String(namespace)),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- Owner refs ----

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, rs_key.clone(), graph, cluster_uid)?;
        }

        // ---- Immutable ReplicaSetState ----
        //
        // Written exclusively through UpdateState. The previous version cast
        // UpdateState AND a manual TRANSITIONED_TO EnsureEdge — if the
        // controller creates that edge itself (it does, per the schema),
        // the manual edge was at best redundant and at worst attached a
        // duplicate with divergent props.

        let state_key = KubeNodeKey::ReplicaSetState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: rs_key.into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: state_key.into_key(),
            state_instance_props: vec![
                Property("replicas".into(), GraphValue::I64(replicas as i64)),
                Property("ready_replicas".into(), GraphValue::I64(ready as i64)),
                Property(
                    "available_replicas".into(),
                    GraphValue::I64(available as i64),
                ),
                ts("valid_from", valid_from_ms),
                ts("observed_at", now_ms()),
            ],
        }))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        // Previously wrote fabricated zero replica counts via a manual
        // UpsertNode + EnsureEdge path (bypassing HAS_STATE maintenance) and
        // no "Deleted" phase. Deletion attests one fact only.
        project_deletion_state(
            graph,
            KubeNodeKey::ReplicaSet {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::ReplicaSetState {
                uid,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )
    }
}

// ---------------------------------------------------------------------------
// Flux: OCIRepository
// TODO: Create a node linking this OCI registry to a canonical representation.
// Something like - FluxOCIRepository -[:RECONCILES_FROM]->(OCIRegistry)
// ---------------------------------------------------------------------------

impl GraphOperable for OciRepository {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        let repo_key = KubeNodeKey::FluxOciRepository {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----
        //
        // spec.url is the only mandatory field that meaningfully identifies
        // this source — it's the registry address being polled.

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: repo_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                Property("namespace".into(), GraphValue::String(namespace.clone())),
                Property("url".into(), GraphValue::String(self.spec.url.clone())),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- State ----
        //
        // We only emit a state node when status.artifact is present — the
        // resource exists in the API before its first successful sync, and
        // there is nothing meaningful to record until Flux has actually
        // resolved the artifact.

        let Some(status) = self.status else {
            return Ok(());
        };

        let ready_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"))
            .map(|c| c.reason.clone())
            .unwrap_or_else(|| "Unknown".into());

        let Some(artifact) = status.artifact else {
            return Ok(());
        };

        // Use artifact.last_update_time as valid_from so the state timeline
        // reflects when Flux actually resolved the artifact, not when the
        // observer happened to receive the event. The CRD carries it as an
        // RFC3339 string; a parse failure falls back to observation time,
        // which keeps the timeline monotonic rather than silently dropping
        // the state.
        let valid_from_ms = rfc3339_ms(&artifact.last_update_time).unwrap_or_else(now_ms);

        let state_key = KubeNodeKey::FluxOciRepositoryState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: repo_key.clone().into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: state_key.into_key(),
            state_instance_props: vec![
                Property("digest".into(), GraphValue::String(artifact.digest.clone())),
                Property(
                    "revision".into(),
                    GraphValue::String(artifact.revision.clone()),
                ),
                Property("ready_reason".into(), GraphValue::String(ready_condition)),
                // OCI annotations — org.opencontainers.image.revision etc.
                // Serialized wholesale; the processor graph queries can
                // extract individual annotation values as needed.
                Property("annotations".into(), opt_json(&artifact.metadata)),
                ts("valid_from", valid_from_ms),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- RECONCILED edge to the canonical OCIArtifact ----
        //
        // Anchored on THIS repository's identity properties (which this impl
        // writes above), not a label-wide scan of FluxOCIRepositoryState by
        // digest — the previous form matched every state in the graph per
        // event, which is O(all states) with no index (constraints are
        // deferred) and merged edges for unrelated repositories that had
        // ever seen the same digest.
        //
        // MERGE is a no-op when the OCIArtifact doesn't exist yet (the MATCH
        // yields nothing); the next observation retries. Digest format must
        // match what the OCI resolver writes on OCIArtifact.digest —
        // "sha256:<hex>" — verify once against real data.

        // NOTE: this is a natural-key match, not a KubeNodeKey::cypher_match
        // -- see the TODO on the Kustomization impl below for why. Same
        // collision exposure applies here: {name, namespace} alone is not
        // unique across clusters, so cluster_uid is added to the MATCH,
        // matching the anchor node this same impl writes it onto above.
        graph.cast(GraphControllerMsg::Op(GraphOp::RawQuery {
            cypher: "
                MATCH (repo:FluxOCIRepository {name: $name, namespace: $namespace, cluster_uid: $cluster_uid})
                MATCH (oci:OCIArtifact {digest: $digest})
                MERGE (repo)-[:RECONCILED]->(oci)
            "
            .into(),
            params: vec![
                ("name".into(), BoltType::String(name.into())),
                ("namespace".into(), BoltType::String(namespace.into())),
                (
                    "cluster_uid".into(),
                    BoltType::String(cluster_uid.to_string().into()),
                ),
                (
                    "digest".into(),
                    BoltType::String(artifact.digest.clone().into()),
                ),
            ],
        }))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        project_deletion_state(
            graph,
            KubeNodeKey::FluxOciRepository {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::FluxOciRepositoryState {
                uid,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )
    }
}

// ---------------------------------------------------------------------------
// Flux: Kustomization
// ---------------------------------------------------------------------------

impl GraphOperable for Kustomization {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        let ks_key = KubeNodeKey::FluxKustomization {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----
        //
        // source_ref is serialized wholesale so the graph retains the full
        // pointer — kind, name, namespace — without requiring a separate
        // edge resolution at write time. The RECONCILES_FROM edge (see TODO
        // below) will handle the structural link; this is for query
        // convenience.

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: ks_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                Property("namespace".into(), GraphValue::String(namespace.clone())),
                Property(
                    "source_ref".into(),
                    GraphValue::String(
                        serde_json::to_string(&self.spec.source_ref)
                            .unwrap_or_else(|_| "{}".into()),
                    ),
                ),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- RECONCILES_FROM edge: honest TODO, not dead code ----
        //
        // Blocked, precisely: KubeNodeKey::FluxOciRepository is keyed by
        // uid, but source_ref only gives (kind, name, namespace) — with the
        // Flux-specified default of the Kustomization's own namespace when
        // source_ref.namespace is None. Building the edge therefore needs
        // either (a) a natural-key RawQuery matching FluxOCIRepository on
        // {name, namespace} (the properties this consumer writes), or (b) a
        // natural-key KubeNodeKey variant. Option (a) is a four-line
        // RawQuery mirroring the RECONCILED merge above. Deprioritized: the
        // provenance chain no longer routes through Flux (runtime join is
        // PodContainer-USES_IMAGE->OCIArtifact), so this edge is
        // explanatory, not load-bearing. GitRepository and Bucket sources
        // remain intentionally out of scope (see spike #201 notes).

        // ---- State ----
        //
        // Guard on status presence — Kustomizations start life with no status.

        let Some(status) = self.status else {
            return Ok(());
        };

        // Extract the Ready condition. last_transition_time is the authoritative
        // valid_from — it's when kustomize-controller actually finished, not
        // when we observed it.
        let ready_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"));

        let valid_from_ms = ready_condition
            .map(|c| c.last_transition_time.0.as_millisecond())
            .unwrap_or_else(now_ms);

        let ready_reason = ready_condition
            .map(|c| c.reason.clone())
            .unwrap_or_else(|| "Unknown".into());

        // Only record state when we have at least one of the revision fields.
        // A Kustomization that has never successfully reconciled has nothing
        // meaningful to write.
        if status.last_applied_revision.is_none() && status.last_applied_origin_revision.is_none() {
            return Ok(());
        }

        let state_key = KubeNodeKey::FluxKustomizationState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: ks_key.clone().into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: state_key.into_key(),
            state_instance_props: vec![
                // last_applied_revision is the OCI content digest — the join
                // key that links this reconciliation back to what Flux fetched
                // from the registry.
                Property(
                    "last_applied_revision".into(),
                    opt_string(&status.last_applied_revision),
                ),
                // last_applied_origin_revision carries the value of
                // org.opencontainers.image.revision from the OCI annotations
                // — the git ref or commit SHA embedded by the pipeline.
                // This is the join key back to SCM events.
                Property(
                    "last_applied_origin_revision".into(),
                    opt_string(&status.last_applied_origin_revision),
                ),
                Property("ready_reason".into(), GraphValue::String(ready_reason)),
                ts("valid_from", valid_from_ms),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- DEPLOYED edge to the canonical OCIArtifact ----
        //
        // Anchored on THIS Kustomization's identity properties instead of
        // the previous label-wide `ENDS WITH` scan across every
        // FluxKustomizationState in the graph. The digest is extracted from
        // the Flux revision format `<tag>@sha256:<hex>` (bare `sha256:<hex>`
        // passes through the unwrap_or) and equality-matched — the suffix
        // match is gone entirely, along with its false-positive surface.

        if let Some(ref revision) = status.last_applied_revision {
            let digest = revision
                .split('@')
                .nth(1)
                .unwrap_or(revision.as_str())
                .to_string();

            // NOTE: natural-key match, same as FluxOCIRepository's RECONCILED
            // query above -- {name, namespace} alone is not unique across
            // clusters, so cluster_uid is added to the MATCH.
            graph.cast(GraphControllerMsg::Op(GraphOp::RawQuery {
                cypher: "
                    MATCH (ks:FluxKustomization {name: $name, namespace: $namespace, cluster_uid: $cluster_uid})
                    MATCH (oci:OCIArtifact {digest: $digest})
                    MERGE (ks)-[:DEPLOYED]->(oci)
                "
                .into(),
                params: vec![
                    ("name".into(), BoltType::String(name.into())),
                    ("namespace".into(), BoltType::String(namespace.into())),
                    (
                        "cluster_uid".into(),
                        BoltType::String(cluster_uid.to_string().into()),
                    ),
                    ("digest".into(), BoltType::String(digest.into())),
                ],
            }))?;
        }
        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        project_deletion_state(
            graph,
            KubeNodeKey::FluxKustomization {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::FluxKustomizationState {
                uid,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )
    }
}

// ---------------------------------------------------------------------------
// Actor state
// ---------------------------------------------------------------------------

pub struct KubeConsumerState {
    pub graph_controller: ActorRef<GraphOp>,
    pub broker_client: TcpClient,
}

pub struct KubeConsumerArgs {
    pub graph_controller: ActorRef<GraphOp>,
    pub broker_client: TcpClient,
}

pub struct ResourceConsumerState {
    pub graph_controller: GraphController,
    pub kind: &'static str,
}
