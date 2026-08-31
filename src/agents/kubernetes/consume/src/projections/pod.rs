use std::collections::HashMap;

use k8s_openapi::api::core::v1::{ContainerStatus, Pod};
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, NULL_FIELD, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use polar::{DiscoverySourceRef, ProvenanceEvent, emit_provenance_event};
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{
    GraphOperable, handle_owner_refs, k8s_time_ms, now_ms, opt_bool, opt_json, opt_string,
    opt_string_vec, project_deletion_state, state_signature, ts,
};

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
        cache: &mut ProjectionCache,
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
                Property("sa_name".into(), GraphValue::String(sa_name)),
                ts("observed_at", now_ms()),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: pod_key.clone().into_key(),
            rel_type: "IN_NAMESPACE".into(),
            to: KubeNodeKey::Namespace {
                name: namespace.clone(),
                cluster_uid: cluster_uid.to_string(),
            }
            .into_key(),
            props: Vec::new(),
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

        let meaningful_props = vec![
            Property("phase".into(), GraphValue::String(phase)),
            Property("ready".into(), GraphValue::Bool(ready)),
        ];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            "Pod".to_string(),
            cluster_uid.to_string(),
            uid.clone(),
            signature,
            pod_state_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!("Pod {uid} state unchanged since last observation, suppressing write");
            }
            EmitDecision::Emit { .. } => {
                let mut props = meaningful_props;
                // Previously absent on PodState instances — valid_from
                // existed only inside the state key, making the temporal
                // model unqueryable for pods. Now explicit, like every
                // other state instance.
                props.push(ts("valid_from", pod_state_ms));
                props.push(ts("observed_at", pod_state_ms));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: pod_key.clone().into_key(),
                    state_type_key: KubeNodeKey::State.into_key(),
                    state_instance_key: new_state_key.into_key(),
                    state_instance_props: props,
                }))?;
            }
        }

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
                    // we key the PodContainer node on — both must agree
                    // (including cluster_uid now), otherwise the USES_IMAGE
                    // join in the resolution handler will never match.
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

                let signature = state_signature(&state_props);

                match cache.should_emit(
                    "PodContainer".to_string(),
                    cluster_uid.to_string(),
                    format!("{uid}/{}", container.name),
                    signature,
                    valid_from_ms.to_string(),
                ) {
                    EmitDecision::Suppress => {
                        debug!(
                            "Container {} on pod {uid} state unchanged since last observation, suppressing write",
                            container.name
                        );
                    }
                    EmitDecision::Emit { .. } => {
                        // Previously absent as a property (key-only) — the
                        // lead-time query's `min(s.valid_from)` depends on
                        // this being an explicit, uniformly-encoded value.
                        state_props.push(ts("valid_from", valid_from_ms));
                        state_props.push(ts("observed_at", now_ms()));

                        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                            resource_key: container_key.clone().into_key(),
                            state_type_key: KubeNodeKey::State.into_key(), // abstract taxonomy
                            state_instance_key,
                            state_instance_props: state_props,
                        }))?;
                    }
                }
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
