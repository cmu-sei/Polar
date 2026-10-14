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
    Job {
        uid: String,
    },
    /// Owners nothing in this workspace watches, so the node is an anchor: a uid, the kind's label and (from
    /// the owner reference on the pod) a name. `is_anchor_only` is true for these.
    StatefulSet {
        uid: String,
    },
    DaemonSet {
        uid: String,
    },
    Node {
        uid: String,
    },
    JobState {
        uid: String,
        valid_from: String,
    },
    FluxOciRepository {
        uid: String,
    },
    FluxOciRepositoryState {
        uid: String,
        valid_from: String,
    },
    FluxKustomization {
        uid: String,
    },
    FluxKustomizationState {
        uid: String,
        valid_from: String,
    },
    FluxOciRepositoryRef {
        name: String,
        namespace: String,
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
            KubeNodeKey::StatefulSet { uid }
            | KubeNodeKey::DaemonSet { uid }
            | KubeNodeKey::Node { uid } => {
                let label = match self {
                    KubeNodeKey::StatefulSet { .. } => "StatefulSet",
                    KubeNodeKey::DaemonSet { .. } => "DaemonSet",
                    _ => "Node",
                };
                let uid_k = format!("{prefix}_uid");
                (
                    format!("({prefix}:{label} {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.clone().into()))],
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
                        "({prefix}:PodState {{ {prefix}_uid: ${pod_uid_k}, {prefix}_valid_from: ${valid_from_k} }})"
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
            // ----------------------------------------------------------------
            // Flux source-controller: OCIRepository
            //
            // Anchor node keyed by uid — same pattern as KubernetesJob.
            // The uid comes from metadata.uid on the OCIRepository object,
            // guaranteed unique within the cluster.
            // ----------------------------------------------------------------
            KubeNodeKey::FluxOciRepository { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("({prefix}:FluxOCIRepository {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.clone().into()))],
                )
            }

            // State node keyed by (uid, valid_from).
            // valid_from is sourced from status.artifact.last_update_time —
            // the moment Flux resolved a new digest from the registry — not
            // observer wall-clock time. This makes the state timeline reflect
            // actual source-controller reconciliation events.
            KubeNodeKey::FluxOciRepositoryState { uid, valid_from } => {
                let uid_k = format!("{prefix}_uid");
                let vf_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:FluxOCIRepositoryState {{ uid: ${uid_k}, valid_from: ${vf_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                    ],
                )
            }
            KubeNodeKey::FluxOciRepositoryRef { name, namespace } => {
                let name_k = format!("{prefix}_name");
                let ns_k = format!("{prefix}_namespace");
                (
                    format!(
                        "({prefix}:FluxOCIRepository {{ name: ${name_k}, namespace: ${ns_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (ns_k, BoltType::String(namespace.clone().into())),
                    ],
                )
            }
            // ----------------------------------------------------------------
            // Flux kustomize-controller: Kustomization
            //
            // Anchor node keyed by uid. The state node carries the revision
            // fields that close the lead time chain:
            //   last_applied_revision        — OCI content digest, join key
            //                                  to FluxOCIRepositoryState
            //   last_applied_origin_revision — org.opencontainers.image.revision
            //                                  annotation, join key back to
            //                                  the pipeline commit / SCM event
            //
            // valid_from is sourced from the Ready condition's
            // last_transition_time, not wall clock. This is when
            // kustomize-controller actually finished reconciling, which is
            // what makes the state timeline meaningful for lead time queries.
            // ----------------------------------------------------------------
            KubeNodeKey::FluxKustomization { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("({prefix}:FluxKustomization {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.clone().into()))],
                )
            }

            KubeNodeKey::FluxKustomizationState { uid, valid_from } => {
                let uid_k = format!("{prefix}_uid");
                let vf_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:FluxKustomizationState {{ uid: ${uid_k}, valid_from: ${vf_k} }})"
                    ),
                    vec![
                        (uid_k, BoltType::String(uid.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                    ],
                )
            }
        }
    }
}

impl KubeNodeKey {
    /// Whether nothing projects this node itself, so the only thing known about it is what an owner reference
    /// on a pod says (its name). The pod's handler records that name; the other owners have their own handlers.
    pub fn is_anchor_only(&self) -> bool {
        matches!(
            self,
            KubeNodeKey::StatefulSet { .. }
                | KubeNodeKey::DaemonSet { .. }
                | KubeNodeKey::Node { .. }
        )
    }

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
            "Job" => KubeNodeKey::Job {
                uid: owner.uid.clone(),
            }
            .into(),
            "StatefulSet" => KubeNodeKey::StatefulSet {
                uid: owner.uid.clone(),
            }
            .into(),
            "DaemonSet" => KubeNodeKey::DaemonSet {
                uid: owner.uid.clone(),
            }
            .into(),
            "Node" => KubeNodeKey::Node {
                uid: owner.uid.clone(),
            }
            .into(),
            _ => {
                tracing::warn!(
                    owner_kind = %owner.kind,
                    owner_name = %owner.name,
                    owner_uid = %owner.uid,
                    "Unknown owner key: no graph node for this kind of owner, so its edge is not written"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod owner_reference_tests {
    use super::*;

    fn owner(kind: &str) -> OwnerReference {
        OwnerReference {
            kind: kind.to_string(),
            name: "x".to_string(),
            uid: "u-1".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn replicasets_and_deployments_are_owners_the_graph_knows() {
        assert!(matches!(
            KubeNodeKey::from_owner_reference(&owner("ReplicaSet")),
            Some(KubeNodeKey::ReplicaSet { .. })
        ));
        assert!(matches!(
            KubeNodeKey::from_owner_reference(&owner("Deployment")),
            Some(KubeNodeKey::Deployment { .. })
        ));
    }

    #[test]
    fn a_job_owner_is_the_job_node_the_consumer_already_projects() {
        assert_eq!(
            KubeNodeKey::from_owner_reference(&owner("Job")),
            Some(KubeNodeKey::Job { uid: "u-1".into() })
        );
    }

    #[test]
    fn statefulsets_daemonsets_and_nodes_get_anchors_named_for_their_kind() {
        for (kind, label) in [
            ("StatefulSet", "StatefulSet"),
            ("DaemonSet", "DaemonSet"),
            ("Node", "Node"),
        ] {
            let key = KubeNodeKey::from_owner_reference(&owner(kind))
                .unwrap_or_else(|| panic!("{kind} should map"));
            let (pattern, params) = key.cypher_match("n");
            assert_eq!(pattern, format!("(n:{label} {{ uid: $n_uid }})"), "{kind}");
            assert_eq!(params.len(), 1, "{kind}");
            assert!(key.is_anchor_only(), "{kind}");
        }
    }

    #[test]
    fn only_owners_nothing_else_projects_are_anchor_only() {
        for kind in ["ReplicaSet", "Deployment", "Job"] {
            let key = KubeNodeKey::from_owner_reference(&owner(kind)).expect(kind);
            assert!(
                !key.is_anchor_only(),
                "{kind} is projected by its own handler"
            );
        }
    }

    #[test]
    fn a_kind_the_graph_has_no_node_for_is_skipped() {
        for kind in ["CronJob", "Cluster", "Widget"] {
            assert!(
                KubeNodeKey::from_owner_reference(&owner(kind)).is_none(),
                "{kind}"
            );
        }
    }
}
