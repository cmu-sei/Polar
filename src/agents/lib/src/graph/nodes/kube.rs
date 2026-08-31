use crate::graph::controller::GraphNodeKey;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use neo4rs::BoltType;

/// Kubernetes has no first-class API object for "a cluster." Every variant
/// below that carries `cluster_uid` is scoped by the UID of that cluster's
/// `kube-system` namespace instead -- a permanent fixture, collision-safe
/// across clusters by construction (real UUID), requiring no curated
/// configuration. There is deliberately no KubernetesCluster node; see
/// issue #236. Cluster-level identity in this schema is Namespace, and
/// eventually Node -- nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KubeNodeKey {
    State,
    Namespace {
        name: String,
        cluster_uid: String,
    },
    NamespaceState {
        name: String,
        valid_from: String,
        cluster_uid: String,
    },
    Deployment {
        uid: String,
        cluster_uid: String,
    },
    DeploymentState {
        uid: String,
        valid_from: String,
        cluster_uid: String,
    },
    ReplicaSet {
        uid: String,
        cluster_uid: String,
    },
    ReplicaSetState {
        uid: String,
        valid_from: String,
        cluster_uid: String,
    },
    Pod {
        uid: String,
        cluster_uid: String,
    },
    PodState {
        pod_uid: String,
        valid_from: String,
        cluster_uid: String,
    },
    PodContainer {
        pod_uid: String,
        name: String,
        cluster_uid: String,
    },
    PodContainerState {
        pod_uid: String,
        name: String,
        valid_from: String,
        cluster_uid: String,
    },
    Volume {
        name: String,
        namespace: String,
        cluster_uid: String,
    },
    PersistentVolumeClaim {
        name: String,
        namespace: String,
        cluster_uid: String,
    },
    Secret {
        name: String,
        namespace: String,
        cluster_uid: String,
    },
    ConfigMap {
        name: String,
        namespace: String,
        cluster_uid: String,
    },
    GenericOwner {
        uid: String,
        kind: String,
        namespace: String,
        cluster_uid: String,
    },
    Job {
        uid: String,
        cluster_uid: String,
    },
    JobState {
        uid: String,
        valid_from: String,
        cluster_uid: String,
    },
    FluxOciRepository {
        uid: String,
        cluster_uid: String,
    },
    FluxOciRepositoryState {
        uid: String,
        valid_from: String,
        cluster_uid: String,
    },
    FluxKustomization {
        uid: String,
        cluster_uid: String,
    },
    FluxKustomizationState {
        uid: String,
        valid_from: String,
        cluster_uid: String,
    },
    FluxOciRepositoryRef {
        name: String,
        namespace: String,
        cluster_uid: String,
    },
}

impl GraphNodeKey for KubeNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
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
            KubeNodeKey::NamespaceState {
                name,
                valid_from,
                cluster_uid,
            } => {
                let name_k = format!("{prefix}_name");
                let valid_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:NamespaceState {{ namespace_name: ${name_k}, valid_from: ${valid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.to_string().into())),
                        (valid_k, BoltType::String(valid_from.to_string().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::Deployment { uid, cluster_uid } => {
                let uid_k = format!("{prefix}_uid");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:KubernetesDeployment {{ uid: ${uid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.to_string().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::DeploymentState {
                uid,
                valid_from,
                cluster_uid,
            } => {
                let uid_k = format!("{prefix}_uid");
                let valid_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:DeploymentState {{ deployment_uid: ${uid_k}, valid_from: ${valid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.to_string().into())),
                        (valid_k, BoltType::String(valid_from.to_string().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::ReplicaSet { uid, cluster_uid } => {
                let uid_k = format!("{prefix}_uid");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:ReplicaSet {{ uid: ${uid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.to_string().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::ReplicaSetState {
                uid,
                valid_from,
                cluster_uid,
            } => {
                let uid_k = format!("{prefix}_uid");
                let valid_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:ReplicaSetState {{ deployment_uid: ${uid_k}, valid_from: ${valid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.to_string().into())),
                        (valid_k, BoltType::String(valid_from.to_string().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::Pod { uid, cluster_uid } => {
                let uid_k = format!("{prefix}_uid");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!("({prefix}:Pod {{ uid: ${uid_k}, cluster_uid: ${cluster_uid_k} }})"),
                    vec![
                        (uid_k, BoltType::String(uid.to_string().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::Job { uid, cluster_uid } => {
                let uid_k = format!("{prefix}_uid");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:KubernetesJob {{ uid: ${uid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::JobState {
                uid,
                valid_from,
                cluster_uid,
            } => {
                let uid_k = format!("{prefix}_uid");
                let vf_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:KubernetesJobState {{ uid: ${uid_k}, valid_from: ${vf_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::PodState {
                pod_uid,
                valid_from,
                cluster_uid,
            } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let valid_from_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:PodState {{ {prefix}_uid: ${pod_uid_k}, {prefix}_valid_from: ${valid_from_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (pod_uid_k, BoltType::String(pod_uid.to_string().into())),
                        (
                            valid_from_k,
                            BoltType::String(valid_from.to_string().into()),
                        ),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::PodContainer {
                pod_uid,
                name,
                cluster_uid,
            } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let name_k = format!("{prefix}_name");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:PodContainer {{ pod_uid: ${pod_uid_k}, name: ${name_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (pod_uid_k, BoltType::String(pod_uid.to_string().into())),
                        (name_k, BoltType::String(name.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::PodContainerState {
                pod_uid,
                name,
                valid_from,
                cluster_uid,
            } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let name_k = format!("{prefix}_name");
                let valid_from_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:PodContainerState {{ pod_uid: ${pod_uid_k}, name: ${name_k}, valid_from: ${valid_from_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (pod_uid_k, BoltType::String(pod_uid.to_string().into())),
                        (name_k, BoltType::String(name.clone().into())),
                        (
                            valid_from_k,
                            BoltType::String(valid_from.to_string().into()),
                        ),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::Volume {
                name,
                namespace,
                cluster_uid,
            } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:Volume {{ name: ${name_k}, namespace: ${namespace_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::PersistentVolumeClaim {
                name,
                namespace,
                cluster_uid,
            } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:PersistentVolumeClaim {{ name: ${name_k}, namespace: ${namespace_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::Secret {
                name,
                namespace,
                cluster_uid,
            } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:Secret {{ name: ${name_k}, namespace: ${namespace_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::ConfigMap {
                name,
                namespace,
                cluster_uid,
            } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:ConfigMap {{ name: ${name_k}, namespace: ${namespace_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (namespace_k, BoltType::String(namespace.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            // ----------------------------------------------------------------
            // Flux source-controller: OCIRepository
            // ----------------------------------------------------------------
            KubeNodeKey::FluxOciRepository { uid, cluster_uid } => {
                let uid_k = format!("{prefix}_uid");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:FluxOCIRepository {{ uid: ${uid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::FluxOciRepositoryState {
                uid,
                valid_from,
                cluster_uid,
            } => {
                let uid_k = format!("{prefix}_uid");
                let vf_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:FluxOCIRepositoryState {{ uid: ${uid_k}, valid_from: ${vf_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::FluxOciRepositoryRef {
                name,
                namespace,
                cluster_uid,
            } => {
                let name_k = format!("{prefix}_name");
                let ns_k = format!("{prefix}_namespace");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:FluxOCIRepository {{ name: ${name_k}, namespace: ${ns_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (ns_k, BoltType::String(namespace.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            // ----------------------------------------------------------------
            // Flux kustomize-controller: Kustomization
            // ----------------------------------------------------------------
            KubeNodeKey::FluxKustomization { uid, cluster_uid } => {
                let uid_k = format!("{prefix}_uid");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:FluxKustomization {{ uid: ${uid_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            KubeNodeKey::FluxKustomizationState {
                uid,
                valid_from,
                cluster_uid,
            } => {
                let uid_k = format!("{prefix}_uid");
                let vf_k = format!("{prefix}_valid_from");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "({prefix}:FluxKustomizationState {{ uid: ${uid_k}, valid_from: ${vf_k}, cluster_uid: ${cluster_uid_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                        (
                            cluster_uid_k,
                            BoltType::String(cluster_uid.to_string().into()),
                        ),
                    ],
                )
            }
            _ => todo!("Handle unimplemented types"),
        }
    }
}

impl KubeNodeKey {
    pub fn from_owner_reference(owner: &OwnerReference, cluster_uid: &str) -> Option<KubeNodeKey> {
        match owner.kind.as_str() {
            "ReplicaSet" => KubeNodeKey::ReplicaSet {
                uid: owner.uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            }
            .into(),
            "Deployment" => KubeNodeKey::Deployment {
                uid: owner.uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            }
            .into(),
            _ => {
                tracing::warn!("Unknown owner key");
                None
            }
        }
    }
}
