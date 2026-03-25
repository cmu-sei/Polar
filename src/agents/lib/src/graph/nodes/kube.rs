use crate::graph::controller::GraphNodeKey;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use neo4rs::BoltType;

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
    PodContainerState {
        pod_uid: String,
        name: String,
        valid_from: String,
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
    Job {
        uid: String,
    },
    JobState {
        uid: String,
        valid_from: String,
    },
}
impl GraphNodeKey for KubeNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            KubeNodeKey::Cluster { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("({prefix}:KubernetesCluster {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.clone().into()))],
                )
            }
            KubeNodeKey::State => ("(:State)".to_string(), vec![]),

            KubeNodeKey::Namespace { name, cluster_uid } => {
                let name_k = format!("{prefix}_name");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:Namespace {{ name: ${name_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
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
                    format!("({prefix}:KubernetesDeployment {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.to_string().into()))],
                )
            }
            KubeNodeKey::DeploymentState { uid, valid_from } => {
                let uid_k = format!("{prefix}_uid");
                let valid_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:DeploymentState {{ deployment_uid: ${uid_k}, valid_from: ${valid_k} }}"
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
                    format!("({prefix}:ReplicaSet {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.to_string().into()))],
                )
            }
            KubeNodeKey::ReplicaSetState { uid, valid_from } => {
                let uid_k = format!("{prefix}_uid");
                let valid_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:ReplicaSetState {{ deployment_uid: ${uid_k}, valid_from: ${valid_k} }}"
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
                    format!("({prefix}:Pod {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.to_string().into()))],
                )
            }
            KubeNodeKey::Job { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("({prefix}:KubernetesJob {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.clone().into()))],
                )
            }

            KubeNodeKey::JobState { uid, valid_from } => {
                let uid_k = format!("{prefix}_uid");
                let vf_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:KubernetesJobState {{ uid: ${uid_k}, valid_from: ${vf_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                    ],
                )
            }
            KubeNodeKey::PodState {
                pod_uid,
                valid_from,
            } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let valid_from_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:PodState {{ {prefix}_uid: ${pod_uid_k}, {prefix}_valid_from: ${valid_from_k} }}"
                    ),
                    vec![
                        (pod_uid_k, BoltType::String(pod_uid.to_string().into())),
                        (
                            valid_from_k,
                            BoltType::String(valid_from.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::PodContainer { pod_uid, name } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let name_k = format!("{prefix}_name");
                (
                    format!("({prefix}:PodContainer {{ pod_uid: ${pod_uid_k}, name: ${name_k} }})"),
                    vec![
                        (pod_uid_k, BoltType::String(pod_uid.to_string().into())),
                        (name_k, BoltType::String(name.clone().into())),
                    ],
                )
            }
            KubeNodeKey::PodContainerState {
                pod_uid,
                name,
                valid_from,
            } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let name_k = format!("{prefix}_name");
                let valid_from_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:PodContainerState {{ pod_uid: ${pod_uid_k}, name: ${name_k}, valid_from: ${valid_from_k} }})"
                    ),
                    vec![
                        (pod_uid_k, BoltType::String(pod_uid.to_string().into())),
                        (name_k, BoltType::String(name.clone().into())),
                        (
                            valid_from_k,
                            BoltType::String(valid_from.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::Volume { name, namespace } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                (
                    format!("({prefix}:Volume {{ name: ${name_k}, namespace: ${namespace_k} }})"),
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
                        "({prefix}:PersistentVolumeClaim {{ name: ${name_k}, namespace: ${namespace_k} }})"
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
                    format!("({prefix}:Secret {{ name: ${name_k}, namespace: ${namespace_k} }})"),
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
                    format!(
                        "({prefix}:ConfigMap {{ name: ${name_k}, namespace: ${namespace_k} }})"
                    ),
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
    pub fn from_owner_reference(owner: &OwnerReference) -> Option<KubeNodeKey> {
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
                tracing::warn!("Unknown owner key");
                None
            }
        }
    }
}
