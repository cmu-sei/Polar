use cassini_client::{TcpClient, TcpClientMessage};
use chrono::Utc;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::OwnerReference};
use neo4rs::BoltType;
use polar::{
    graph::{GraphController, GraphControllerMsg, GraphNodeKey, GraphOp, GraphValue, Property},
    impl_graph_controller, ProvenanceEvent, RkyvError, PROVENANCE_DISCOVERY_TOPIC,
};
use ractor::{ActorProcessingErr, ActorRef};
use rkyv::to_bytes;
use std::fmt::Debug;
use tracing::warn;

pub mod supervisor;

pub const BROKER_CLIENT_NAME: &str = "kubernetes.cluster.cassini.client";

///applies to domain-identifiable entities, not arbitrary spec fragments.
pub trait GraphOperable {
    fn project_into_graph(
        self,
        graph: &GraphController<KubeNodeKey>,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr>;

    fn project_delete(self, graph: &GraphController<KubeNodeKey>)
        -> Result<(), ActorProcessingErr>;
}

fn handle_owner_refs(
    owners: &Vec<OwnerReference>,
    node_key: KubeNodeKey,
    graph: &GraphController<KubeNodeKey>,
) -> Result<(), ActorProcessingErr> {
    for owner in owners {
        if let Some(owner_key) = KubeNodeKey::from_owner_reference(&owner) {
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: owner_key,
                rel_type: "OWNS".into(),
                to: node_key.clone(),
                props: Vec::new(),
            }))?;
        }
    }

    Ok(())
}

impl GraphOperable for Pod {
    fn project_into_graph(
        self,
        graph: &GraphController<KubeNodeKey>,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.unwrap_or_default();

        let phase = self
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Unknown".into());

        let _ready = self
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
            valid_from: transition_time,
        };

        let op = GraphOp::UpdateState {
            resource_key: pod_key.clone(),
            state_type_key: KubeNodeKey::State,
            state_instance_key: new_state_key,
            state_instance_props: vec![Property("phase".into(), GraphValue::String(phase))],
        };

        graph.cast(GraphControllerMsg::Op(op))?;

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, pod_key.clone(), graph)?;
        }

        // ---- Node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: pod_key.clone(),
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
                // TODO: At the moment,
                // this enables multiple pods that use the same volume spec to connect to the same node
                // Not sure that's desirable
                let vol_key = KubeNodeKey::Volume {
                    name: volume.name.clone(),
                    namespace: namespace.clone(),
                };

                // TODO: pretty much every other field for volumes are optional, but there are surely some things we're gonna want to know about them
                // // What cloud resources are they pointing to? Where are theyin the host path,
                // // We don't want to just blast the yaml structure in the graph, but we're gonna have to keep this in mind

                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: vol_key.clone(),
                    props: Vec::new(),
                }))?;

                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: pod_key.clone(),
                    rel_type: "USES_VOLUME".into(),
                    to: vol_key.clone(),
                    props: Vec::new(),
                }))?;

                if let Some(cm) = volume.config_map {
                    let cm_key = KubeNodeKey::ConfigMap {
                        name: cm.name,
                        namespace: namespace.clone(),
                    };

                    graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: cm_key.clone(),
                        props: Vec::new(),
                    }))?;

                    graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                        from: vol_key.clone(),
                        rel_type: "BACKED_BY".into(),
                        to: cm_key,
                        props: Vec::new(),
                    }))?;
                }

                if let Some(secret) = volume.secret {
                    if let Some(secret_name) = secret.secret_name {
                        let s_key = KubeNodeKey::Secret {
                            name: secret_name,
                            namespace: namespace.clone(),
                        };

                        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                            key: s_key.clone(),
                            props: Vec::new(),
                        }))?;

                        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                            from: vol_key.clone(),
                            rel_type: "BACKED_BY".into(),
                            to: s_key,
                            props: Vec::new(),
                        }))?;
                    }
                }

                if let Some(pvc) = volume.persistent_volume_claim {
                    let pvc_key = KubeNodeKey::PersistentVolumeClaim {
                        name: pvc.claim_name,
                        namespace: namespace.clone(),
                    };

                    graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: pvc_key.clone(),
                        props: Vec::new(),
                    }))?;

                    graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                        from: vol_key,
                        rel_type: "BACKED_BY".into(),
                        to: pvc_key,
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

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: container_key.clone(),
                props: Vec::new(),
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: pod_key.clone(),
                rel_type: "HAS_CONTAINER".into(),
                to: container_key,
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

            if let Some(envs) = container.env {
                for env in envs {
                    if let Some(value_from) = env.value_from {
                        if let Some(cm_ref) = value_from.config_map_key_ref {
                            let cm_key = KubeNodeKey::ConfigMap {
                                name: cm_ref.name,
                                namespace: namespace.clone(),
                            };

                            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                                from: pod_key.clone(),
                                rel_type: "USES_CONFIGMAP".into(),
                                to: cm_key,
                                props: Vec::new(),
                            }))?;
                        }

                        if let Some(secret_ref) = value_from.secret_key_ref {
                            let s_key = KubeNodeKey::Secret {
                                name: secret_ref.name,
                                namespace: namespace.clone(),
                            };

                            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                                from: pod_key.clone(),
                                rel_type: "USES_SECRET".into(),
                                to: s_key,
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
        graph: &ActorRef<GraphControllerMsg<KubeNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.clone().unwrap();
        let pod_key = KubeNodeKey::Pod { uid: uid.clone() };

        let now = Utc::now().to_rfc3339();

        let state_key = KubeNodeKey::PodState {
            pod_uid: uid.clone(),
            valid_from: now.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: state_key.clone(),
            props: vec![
                Property("phase".into(), GraphValue::String("Deleted".into())),
                Property("valid_from".into(), GraphValue::String(now.clone())),
                Property("observed_at".into(), GraphValue::String(now.clone())),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: pod_key,
            to: state_key,
            rel_type: "TRANSITIONED_TO".into(),
            props: vec![Property("at".into(), GraphValue::String(now))],
        }))?;

        Ok(())
    }
}

impl GraphOperable for Deployment {
    fn project_into_graph(
        self,
        graph: &GraphController<KubeNodeKey>,
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
            .unwrap_or_else(|| "Unknown".into());

        let available_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .find(|c| c.type_ == "Available")
                    .map(|c| c.status.clone())
            })
            .unwrap_or_else(|| "Unknown".into());

        let deployment_key = KubeNodeKey::Deployment { uid: uid.clone() };

        // ---- Upsert anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: deployment_key.clone(),
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
            resource_key: deployment_key.clone(),
            state_type_key: KubeNodeKey::State,
            state_instance_key: state_key,
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
        graph: &ActorRef<GraphControllerMsg<KubeNodeKey>>,
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
            .unwrap_or_else(|| "Unknown".into());

        let available_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .find(|c| c.type_ == "Available")
                    .map(|c| c.status.clone())
            })
            .unwrap_or_else(|| "Unknown".into());

        let deployment_key = KubeNodeKey::Deployment { uid: uid.clone() };

        let now = Utc::now().to_rfc3339();

        let state_key = KubeNodeKey::DeploymentState {
            uid: uid.clone(),
            valid_from: now.clone(),
        };

        let transition_time = Utc::now().to_rfc3339();
        let op = GraphOp::UpdateState {
            resource_key: deployment_key.clone(),
            state_type_key: KubeNodeKey::State,
            state_instance_key: state_key,
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
        graph: &GraphController<KubeNodeKey>,
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
            key: rs_key.clone(),
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
            resource_key: rs_key.clone(),
            state_type_key: KubeNodeKey::State,
            state_instance_key: state_key.clone(),
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
            from: rs_key,
            to: state_key,
            rel_type: "TRANSITIONED_TO".into(),
            props: vec![Property("at".into(), GraphValue::String(transition_time))],
        }))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &ActorRef<GraphControllerMsg<KubeNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        let uid = self.metadata.uid.clone().unwrap();
        let rs_key = KubeNodeKey::ReplicaSet { uid: uid.clone() };

        let now = Utc::now().to_rfc3339();

        let state_key = KubeNodeKey::ReplicaSetState {
            uid: uid.clone(),
            valid_from: now.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: state_key.clone(),
            props: vec![
                Property("replicas".into(), GraphValue::I64(0)),
                Property("ready_replicas".into(), GraphValue::I64(0)),
                Property("available_replicas".into(), GraphValue::I64(0)),
                Property("valid_from".into(), GraphValue::String(now.clone())),
                Property("observed_at".into(), GraphValue::String(now.clone())),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: rs_key,
            to: state_key,
            rel_type: "TRANSITIONED_TO".into(),
            props: vec![Property("at".into(), GraphValue::String(now))],
        }))?;

        Ok(())
    }
}

pub struct KubeConsumerState {
    pub graph_controller: ActorRef<GraphOp<KubeNodeKey>>,
    pub broker_client: ActorRef<TcpClientMessage>,
}

pub struct KubeConsumerArgs {
    pub graph_controller: ActorRef<GraphOp<KubeNodeKey>>,
    pub broker_client: ActorRef<TcpClientMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KubeNodeKey {
    /// Kubernetes does not have a built-in, first-class API object specifically for a cluster-wide unique ID
    /// Instead, the universally unique identifier (UID) of the kube-system namespace is commonly used as a reliable proxy for a cluster ID,
    /// as this namespace is a permanent fixture in the cluster and its UID is unique across all clusters
    State,
    Cluster {
        uid: String,
    },
    Namespace {
        name: String,
        cluster_uid: String,
    },
    Deployment {
        uid: String,
    },
    DeploymentState {
        uid: String,
        valid_from: String,
    },
    ReplicaSet {
        uid: String,
    },
    ReplicaSetState {
        uid: String,
        valid_from: String,
    },
    Pod {
        uid: String,
    },
    PodState {
        pod_uid: String,
        valid_from: String,
    },

    PodContainer {
        pod_uid: String,
        name: String,
    },
    Volume {
        name: String,
        namespace: String,
    },
    PersistentVolumeClaim {
        name: String,
        namespace: String,
    },
    Secret {
        name: String,
        namespace: String,
    },
    ConfigMap {
        name: String,
        namespace: String,
    },
    GenericOwner {
        uid: String,
        kind: String,
        namespace: String,
    },
}
impl GraphNodeKey for KubeNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            KubeNodeKey::Cluster { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("(:KubernetesCluster {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.clone().into()))],
                )
            }
            KubeNodeKey::State => (format!("(:State)"), vec![]),

            KubeNodeKey::Namespace { name, cluster_uid } => {
                let name_k = format!("{prefix}_name");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!("(:Namespace {{ name: ${name_k}, cluster_uid: ${cluster_uid_k} }})"),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::Deployment { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("(:KubernetesDeployment {{ uid: ${uid_k} }}"),
                    vec![(uid_k, BoltType::String(uid.to_string().into()))],
                )
            }
            KubeNodeKey::DeploymentState { uid, valid_from } => {
                let uid_k = format!("{prefix}_uid");
                let valid_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "(:DeploymentState {{ deployment_uid: ${uid_k}, valid_from: ${valid_k} }}"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.to_string().into())),
                        (valid_k, BoltType::String(valid_from.to_string().into())),
                    ],
                )
            }
            KubeNodeKey::ReplicaSet { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("(:ReplicaSet {{ uid: ${uid_k} }}"),
                    vec![(uid_k, BoltType::String(uid.to_string().into()))],
                )
            }
            KubeNodeKey::ReplicaSetState { uid, valid_from } => {
                let uid_k = format!("{prefix}_uid");
                let valid_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "(:ReplicaSetState {{ deployment_uid: ${uid_k}, valid_from: ${valid_k} }}"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.to_string().into())),
                        (valid_k, BoltType::String(valid_from.to_string().into())),
                    ],
                )
            }
            KubeNodeKey::Pod { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("(:Pod {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.to_string().into()))],
                )
            }
            KubeNodeKey::PodState {
                pod_uid,
                valid_from,
            } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let valid_from_k = format!("{prefix}_valid_from");
                (
                format!("(:PodState {{ {prefix}_uid: ${pod_uid_k}, {prefix}_valid_from: ${valid_from_k} }}"),
                vec![
                    (pod_uid_k,BoltType::String(pod_uid.to_string().into())),
                    (valid_from_k, BoltType::String(valid_from.to_string().into()))
                ],
                )
            }
            KubeNodeKey::PodContainer { pod_uid, name } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let name_k = format!("{prefix}_name");
                (
                    format!("(:PodContainer {{ pod_uid: ${pod_uid_k}, name: ${name_k} }})"),
                    vec![
                        (pod_uid_k, BoltType::String(pod_uid.to_string().into())),
                        (name_k, BoltType::String(name.clone().into())),
                    ],
                )
            }
            KubeNodeKey::Volume { name, namespace } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                (
                    format!("(:Volume {{ name: ${name_k}, namespace: ${namespace_k} }})"),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                    ],
                )
            }

            KubeNodeKey::PersistentVolumeClaim { name, namespace } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                (
                    format!(
                        "(:PersistentVolumeClaim {{ name: ${name_k}, namespace: ${namespace_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                    ],
                )
            }
            KubeNodeKey::Secret { name, namespace } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                (
                    format!("(:Secret {{ name: ${name_k}, namespace: ${namespace_k} }})"),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                    ],
                )
            }

            KubeNodeKey::ConfigMap { name, namespace } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                (
                    format!("(:ConfigMap {{ name: ${name_k}, namespace: ${namespace_k} }})"),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                    ],
                )
            }
            _ => todo!("Implement queries for {self:?}"),
        }
    }
}
impl KubeNodeKey {
    fn from_owner_reference(owner: &OwnerReference) -> Option<KubeNodeKey> {
        match owner.kind.as_str() {
            "ReplicaSet" => KubeNodeKey::ReplicaSet {
                uid: owner.uid.clone(),
            }
            .into(),
            "Deployment" => KubeNodeKey::Deployment {
                uid: owner.uid.clone(),
            }
            .into(),
            _ => {
                warn!("Unknown owner key");
                None
            }
        }
    }
}

impl_graph_controller!(ClusterGraphController, node_key = KubeNodeKey);

pub struct ResourceConsumerState {
    pub graph_controller: GraphController<KubeNodeKey>,
    pub kind: &'static str,
}

#[cfg(test)]
mod graph_node_key_tests {
    use super::*;

    fn assert_param_keys_prefixed(params: &[(String, BoltType)], prefix: &str) {
        for (k, _) in params {
            assert!(
                k.starts_with(prefix),
                "param key `{k}` is not prefixed with `{prefix}`"
            );
        }
    }

    #[test]
    fn pod_key_cypher_match_is_prefixed_and_stable() {
        let key = KubeNodeKey::Pod {
            uid: "pod-123".into(),
        };

        let (query, params) = key.cypher_match("p1");

        assert!(query.contains("p1_p:Pod"));
        assert!(query.contains("$p1_uid"));
        assert_param_keys_prefixed(&params, "p1");

        assert_eq!(params.len(), 1);
        assert_eq!(
            params[0],
            ("p1_uid".into(), BoltType::String("pod-123".into()))
        );
    }

    #[test]
    fn pod_container_key_uses_composite_identity() {
        let key = KubeNodeKey::PodContainer {
            pod_uid: "pod-1".into(),
            name: "nginx".into(),
        };

        let (query, params) = key.cypher_match("pc");

        assert!(query.contains("pc_c:PodContainer"));
        assert!(query.contains("$pc_pod_uid"));
        assert!(query.contains("$pc_name"));
        assert_param_keys_prefixed(&params, "pc");

        assert_eq!(params.len(), 2);
    }

    #[test]
    fn namespace_key_includes_cluster_uid() {
        let key = KubeNodeKey::Namespace {
            name: "default".into(),
            cluster_uid: "cluster-1".into(),
        };

        let (query, params) = key.cypher_match("ns");

        assert!(query.contains("Namespace"));
        assert!(query.contains("$ns_name"));
        assert!(query.contains("$ns_cluster_uid"));
        assert_param_keys_prefixed(&params, "ns");
    }
}

#[cfg(test)]
mod compile_graph_op_tests {
    use super::*;
    use polar::graph::{compile_graph_op, GraphValue, Property};

    #[test]
    fn upsert_node_compiles_with_set_clause() {
        let key = KubeNodeKey::Pod {
            uid: "pod-1".into(),
        };

        let op = GraphOp::UpsertNode {
            key,
            props: vec![
                Property("name".into(), GraphValue::String("api".into())),
                Property("namespace".into(), GraphValue::String("default".into())),
            ],
        };

        let q = compile_graph_op(&op);
        let query = q.query();

        assert!(query.contains("MERGE (n"));
        assert!(query.contains("SET n.name = $name"));
        assert!(query.contains("n.namespace = $namespace"));
    }

    #[test]
    fn ensure_edge_compiles_distinct_from_and_to_nodes() {
        let from = KubeNodeKey::Pod { uid: "p1".into() };
        let to = KubeNodeKey::ConfigMap {
            name: "cfg".into(),
            namespace: "default".into(),
        };

        let op = GraphOp::EnsureEdge {
            from,
            to,
            rel_type: "USES_CONFIGMAP".into(),
            props: vec![],
        };

        let q = compile_graph_op(&op);
        let query = q.query();

        assert!(query.contains("MERGE (a"));
        assert!(query.contains("MERGE (b"));
        assert!(query.contains("MERGE (a)-[r:USES_CONFIGMAP]->(b)"));

        // critical: param namespaces must not collide
        assert!(query.contains("$from_uid"));
        assert!(query.contains("$to_name"));
        assert!(query.contains("$to_namespace"));
    }
}
