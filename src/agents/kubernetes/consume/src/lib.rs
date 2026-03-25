use cassini_client::{TcpClient, TcpClientMessage};
use chrono::Utc;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::OwnerReference};
use polar::graph::controller::IntoGraphKey;
use polar::{
    PROVENANCE_DISCOVERY_TOPIC, ProvenanceEvent, RkyvError,
    graph::{
        controller::{
            GraphController, GraphControllerMsg, GraphOp, GraphValue, NULL_FIELD, Property,
        },
        nodes::kube::KubeNodeKey,
    },
};
use ractor::{ActorProcessingErr, ActorRef};
use rkyv::to_bytes;

pub mod supervisor;

pub const BROKER_CLIENT_NAME: &str = "kubernetes.cluster.cassini.client";

///applies to domain-identifiable entities, not arbitrary spec fragments.
pub trait GraphOperable {
    fn project_into_graph(
        self,
        graph: &GraphController,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr>;

    fn project_delete(self, graph: &GraphController) -> Result<(), ActorProcessingErr>;
}

fn handle_owner_refs(
    owners: &Vec<OwnerReference>,
    node_key: KubeNodeKey,
    graph: &GraphController,
) -> Result<(), ActorProcessingErr> {
    for owner in owners {
        if let Some(owner_key) = KubeNodeKey::from_owner_reference(owner) {
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

fn opt_json<T: serde::Serialize>(v: &Option<T>) -> GraphValue {
    match v {
        Some(inner) => {
            GraphValue::String(serde_json::to_string(inner).expect("serialization should not fail"))
        }
        None => GraphValue::Null,
    }
}

impl GraphOperable for Job {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.clone().unwrap_or_default();
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

        let job_key = KubeNodeKey::Job { uid: uid.clone() };

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
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        }))?;

        // ---- Owner refs (e.g. CronJob owns Job) ----

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, job_key.clone(), graph)?;
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

        let transition_time = status
            .and_then(|s| s.completion_time.as_ref())
            .map(|t| t.0.to_rfc3339())
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        let state_key = KubeNodeKey::JobState {
            uid: uid.clone(),
            valid_from: transition_time.clone(),
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
                Property("valid_from".into(), GraphValue::String(transition_time)),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        }))?;

        Ok(())
    }

    fn project_delete(self, graph: &GraphController) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.clone().unwrap_or_default();
        let job_key = KubeNodeKey::Job { uid: uid.clone() };
        let now = Utc::now().to_rfc3339();

        let state_key = KubeNodeKey::JobState {
            uid: uid.clone(),
            valid_from: now.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: job_key.into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: state_key.into_key(),
            state_instance_props: vec![
                Property("phase".into(), GraphValue::String("Deleted".into())),
                Property("valid_from".into(), GraphValue::String(now.clone())),
                Property("observed_at".into(), GraphValue::String(now.clone())),
            ],
        }))?;

        Ok(())
    }
}
impl GraphOperable for Pod {
    fn project_into_graph(
        self,
        graph: &GraphController,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.unwrap_or_default();

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

        let pod_name = self.metadata.name.unwrap_or_default();
        let namespace = self.metadata.namespace.unwrap_or_else(|| "default".into());

        let sa_name = self
            .spec
            .as_ref()
            .and_then(|s| s.service_account_name.clone())
            .unwrap_or_default();

        let pod_key = KubeNodeKey::Pod { uid: uid.clone() };

        // Canonical signature

        let transition_time = Utc::now().to_rfc3339();
        // deterministic state node key
        let new_state_key = KubeNodeKey::PodState {
            pod_uid: uid.clone(),
            valid_from: transition_time.clone(),
        };

        let op = GraphOp::UpdateState {
            resource_key: pod_key.clone().into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: new_state_key.into_key(),
            state_instance_props: vec![
                Property("phase".into(), GraphValue::String(phase)),
                Property("ready".into(), GraphValue::Bool(ready)),
            ],
        };

        graph.cast(GraphControllerMsg::Op(op))?;

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, pod_key.clone(), graph)?;
        }

        // ---- Node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: pod_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(pod_name)),
                Property("namespace".into(), GraphValue::String(namespace.clone())),
                Property("sa_name".into(), GraphValue::String(sa_name)),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        }))?;

        let Some(spec) = self.spec else {
            // if no spec just return
            return Ok(());
        };

        // ---- Volumes ----

        if let Some(volumes) = spec.volumes {
            for volume in volumes {
                let vol_key = KubeNodeKey::Volume {
                    name: volume.name.clone(),
                    namespace: namespace.clone(),
                };

                // TODO: pretty much every other field for volumes are optional, but there are surely some things we're gonna want to know about them
                // // What cloud resources are they pointing to? Where are theyin the host path,
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

        // ---- Containers ----

        let containers = spec
            .containers
            .into_iter()
            .chain(spec.init_containers.unwrap_or_default());

        for container in containers {
            let Some(image) = container.image.clone() else {
                continue;
            };

            let container_key = KubeNodeKey::PodContainer {
                pod_uid: uid.clone(),
                name: container.name.clone(),
            };

            let props = vec![
                Property("name".into(), GraphValue::String(container.name.clone())),
                Property("image".into(), opt_string(&container.image)),
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

            if let Some(mounts) = container.volume_mounts {
                for mount in mounts {
                    let volume_name = &mount.name;

                    let volume_key = KubeNodeKey::Volume {
                        name: volume_name.clone(),
                        namespace: namespace.clone(),
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
                            Property(
                                "observed_at".into(),
                                GraphValue::String(transition_time.clone()),
                            ),
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

            // provenance side channel
            let event = ProvenanceEvent::ImageRefDiscovered { uri: image };

            let payload = to_bytes::<RkyvError>(&event)?;
            tcp_client.cast(TcpClientMessage::Publish {
                topic: PROVENANCE_DISCOVERY_TOPIC.into(),
                payload: payload.into(),
                trace_ctx: None,
            })?;

            // ---- Container Lifecycle ----

            if let Some(status) = self.status.as_ref() {
                let statuses = status
                    .container_statuses
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .chain(status.init_container_statuses.clone().unwrap_or_default());

                for cs in statuses {
                    if cs.name != container.name {
                        continue;
                    }

                    let (state_type, valid_from, mut state_props) = if let Some(waiting) =
                        cs.state.as_ref().and_then(|s| s.waiting.as_ref())
                    {
                        (
                            "Waiting",
                            Utc::now().to_rfc3339(),
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
                    } else if let Some(running) = cs.state.as_ref().and_then(|s| s.running.as_ref())
                    {
                        (
                            "Running",
                            running
                                .clone()
                                .started_at
                                .map(|t| t.0.to_rfc3339())
                                .unwrap_or_else(|| Utc::now().to_rfc3339()),
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
                    } else if let Some(term) = cs.state.as_ref().and_then(|s| s.terminated.as_ref())
                    {
                        (
                            "Terminated",
                            term.clone()
                                .finished_at
                                .map(|t| t.0.to_rfc3339())
                                .unwrap_or_else(|| Utc::now().to_rfc3339()),
                            vec![
                                Property(
                                    "exit_code".into(),
                                    GraphValue::I64(term.exit_code as i64),
                                ),
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
                            Utc::now().to_rfc3339(),
                            vec![Property(
                                "restart_count".into(),
                                GraphValue::I64(cs.restart_count as i64),
                            )],
                        )
                    };

                    // Deterministic state instance key
                    let state_instance_key = KubeNodeKey::PodContainerState {
                        pod_uid: uid.clone(),
                        name: container.name.clone(),
                        valid_from: valid_from.clone(),
                    }
                    .into_key();

                    let op = GraphOp::UpdateState {
                        resource_key: container_key.clone().into_key(),
                        state_type_key: KubeNodeKey::State.into_key(), // abstract taxonomy
                        state_instance_key,
                        state_instance_props: {
                            state_props.push(Property(
                                "phase".into(),
                                GraphValue::String(state_type.into()),
                            ));
                            state_props
                        },
                    };

                    graph.cast(GraphControllerMsg::Op(op))?;
                }
            }

            if let Some(envs) = container.env {
                for env in envs {
                    if let Some(value_from) = env.value_from {
                        if let Some(cm_ref) = value_from.config_map_key_ref {
                            let cm_key = KubeNodeKey::ConfigMap {
                                name: cm_ref.name,
                                namespace: namespace.clone(),
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

            // TODO: Get container volume mounts and tie them to volumes
        }

        Ok(())
    }

    fn project_delete(
        self,
        graph: &ActorRef<GraphControllerMsg>,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.clone().unwrap();
        let pod_key = KubeNodeKey::Pod { uid: uid.clone() };

        let now = Utc::now().to_rfc3339();

        let state_key = KubeNodeKey::PodState {
            pod_uid: uid.clone(),
            valid_from: now.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: state_key.clone().into_key(),
            props: vec![
                Property("phase".into(), GraphValue::String("Deleted".into())),
                Property("valid_from".into(), GraphValue::String(now.clone())),
                Property("observed_at".into(), GraphValue::String(now.clone())),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: pod_key.into_key(),
            to: state_key.into_key(),
            rel_type: "TRANSITIONED_TO".into(),
            props: vec![Property("at".into(), GraphValue::String(now))],
        }))?;

        let Some(_spec) = self.spec else {
            // if no spec just return
            return Ok(());
        };

        Ok(())
    }
}

impl GraphOperable for Deployment {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        let _kind = "Deployment";
        let uid = self.metadata.uid.clone().unwrap_or_default();
        let name = self.metadata.name.unwrap_or_default();
        let namespace = self.metadata.namespace.unwrap_or_else(|| "default".into());

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

        let deployment_key = KubeNodeKey::Deployment { uid: uid.clone() };

        // ---- Upsert anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: deployment_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name)),
                Property("namespace".into(), GraphValue::String(namespace)),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        }))?;

        // ---- Immutable DeploymentState ----

        let transition_time = Utc::now().to_rfc3339();
        let state_key = KubeNodeKey::DeploymentState {
            uid: uid.clone(),
            valid_from: transition_time.clone(),
        };

        let op = GraphOp::UpdateState {
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
                Property(
                    "valid_from".into(),
                    GraphValue::String(transition_time.clone()),
                ),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        };

        graph.cast(GraphControllerMsg::Op(op))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &ActorRef<GraphControllerMsg>,
    ) -> Result<(), ActorProcessingErr> {
        let _uid = self.metadata.uid.clone().unwrap();
        let uid = self.metadata.uid.clone().unwrap_or_default();

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

        let deployment_key = KubeNodeKey::Deployment { uid: uid.clone() };

        let now = Utc::now().to_rfc3339();

        let state_key = KubeNodeKey::DeploymentState {
            uid: uid.clone(),
            valid_from: now.clone(),
        };

        let transition_time = Utc::now().to_rfc3339();
        let op = GraphOp::UpdateState {
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
                Property(
                    "valid_from".into(),
                    GraphValue::String(transition_time.clone()),
                ),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        };

        graph.cast(GraphControllerMsg::Op(op))?;
        Ok(())
    }
}

use k8s_openapi::api::apps::v1::ReplicaSet;

impl GraphOperable for ReplicaSet {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.clone().unwrap_or_default();
        let name = self.metadata.name.unwrap_or_default();
        let namespace = self.metadata.namespace.unwrap_or_else(|| "default".into());

        let status = self.status.unwrap_or_default();

        let replicas = status.replicas;
        let ready = status.ready_replicas.unwrap_or(0);
        let available = status.available_replicas.unwrap_or(0);

        let transition_time = Utc::now().to_rfc3339();

        let rs_key = KubeNodeKey::ReplicaSet { uid: uid.clone() };
        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, rs_key.clone(), graph)?;
        }

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: rs_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name)),
                Property("namespace".into(), GraphValue::String(namespace)),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        }))?;

        // ---- Immutable ReplicaSetState ----

        let state_key = KubeNodeKey::ReplicaSetState {
            uid: uid.clone(),
            valid_from: transition_time.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
            resource_key: rs_key.clone().into_key(),
            state_type_key: KubeNodeKey::State.into_key(),
            state_instance_key: state_key.clone().into_key(),
            state_instance_props: vec![
                Property("replicas".into(), GraphValue::I64(replicas as i64)),
                Property("ready_replicas".into(), GraphValue::I64(ready as i64)),
                Property(
                    "available_replicas".into(),
                    GraphValue::I64(available as i64),
                ),
                Property(
                    "valid_from".into(),
                    GraphValue::String(transition_time.clone()),
                ),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: rs_key.into_key(),
            to: state_key.into_key(),
            rel_type: "TRANSITIONED_TO".into(),
            props: vec![Property("at".into(), GraphValue::String(transition_time))],
        }))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &ActorRef<GraphControllerMsg>,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.clone().unwrap();
        let rs_key = KubeNodeKey::ReplicaSet { uid: uid.clone() };

        let now = Utc::now().to_rfc3339();

        let state_key = KubeNodeKey::ReplicaSetState {
            uid: uid.clone(),
            valid_from: now.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: state_key.clone().into_key(),
            props: vec![
                Property("replicas".into(), GraphValue::I64(0)),
                Property("ready_replicas".into(), GraphValue::I64(0)),
                Property("available_replicas".into(), GraphValue::I64(0)),
                Property("valid_from".into(), GraphValue::String(now.clone())),
                Property("observed_at".into(), GraphValue::String(now.clone())),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: rs_key.into_key(),
            to: state_key.into_key(),
            rel_type: "TRANSITIONED_TO".into(),
            props: vec![Property("at".into(), GraphValue::String(now))],
        }))?;

        Ok(())
    }
}

pub struct KubeConsumerState {
    pub graph_controller: ActorRef<GraphOp>,
    pub broker_client: ActorRef<TcpClientMessage>,
}

pub struct KubeConsumerArgs {
    pub graph_controller: ActorRef<GraphOp>,
    pub broker_client: ActorRef<TcpClientMessage>,
}

pub struct ResourceConsumerState {
    pub graph_controller: GraphController,
    pub kind: &'static str,
}
