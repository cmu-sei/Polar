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

use crate::graph::controller::{GraphNodeKey, NodeIdentity, NodeKeyVocabulary};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use neo4rs::BoltType;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KubeNodeKey {
    /// Kubernetes does not have a built-in, first-class API object specifically for a cluster-wide unique ID
    /// Instead, the universally unique identifier (UID) of the kube-system namespace is commonly used as a reliable proxy for a cluster ID,
    /// as this namespace is a permanent fixture in the cluster and its UID is unique across all clusters
    ///
    /// Every key below carries that identifier as `cluster_uid` (cmu-sei/Polar issue 236), so that two clusters
    /// observed into one graph cannot meet on a name, and a node can be said to belong to the cluster it was seen in.
    State,
    Cluster {
        uid: String,
    },
    Namespace {
        name: String,
        cluster_uid: String,
    },
    NamespaceState {
        name: String,
        valid_from: i64,
        cluster_uid: String,
    },
    Deployment {
        uid: String,
        cluster_uid: String,
    },
    DeploymentState {
        uid: String,
        valid_from: i64,
        cluster_uid: String,
    },
    ReplicaSet {
        uid: String,
        cluster_uid: String,
    },
    ReplicaSetState {
        uid: String,
        valid_from: i64,
        cluster_uid: String,
    },
    Pod {
        uid: String,
        cluster_uid: String,
    },
    PodState {
        pod_uid: String,
        valid_from: i64,
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
        valid_from: i64,
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
    Job {
        uid: String,
        cluster_uid: String,
    },
    /// Owners nothing in this workspace watches, so the node is an anchor: a uid, the kind's label and (from
    /// the owner reference on the pod) a name. `is_anchor_only` is true for these.
    StatefulSet {
        uid: String,
        cluster_uid: String,
    },
    DaemonSet {
        uid: String,
        cluster_uid: String,
    },
    /// A watched, cluster-scoped Node. A pod is `RUNNING_ON` it, and the observer's node watcher writes its states.
    /// Deliberately UID-keyed like every other watched resource, even though that costs a resolution step for
    /// `RUNNING_ON` (a pod carries only the node's *name*): see `ProjectionCache` in the consumer crate.
    Node {
        uid: String,
        cluster_uid: String,
    },
    NodeState {
        uid: String,
        valid_from: i64,
        cluster_uid: String,
    },
    JobState {
        uid: String,
        valid_from: i64,
        cluster_uid: String,
    },
    FluxOciRepository {
        uid: String,
        cluster_uid: String,
    },
    FluxOciRepositoryState {
        uid: String,
        valid_from: i64,
        cluster_uid: String,
    },
    FluxKustomization {
        uid: String,
        cluster_uid: String,
    },
    FluxKustomizationState {
        uid: String,
        valid_from: i64,
        cluster_uid: String,
    },
    FluxOciRepositoryRef {
        name: String,
        namespace: String,
        cluster_uid: String,
    },
    /// The `PolarAgent` resource that declares an agent (polar-operator): the deployment of an agent on this cluster.
    PolarAgent {
        uid: String,
        cluster_uid: String,
    },
    PolarAgentResourceState {
        uid: String,
        valid_from: i64,
        cluster_uid: String,
    },
}
impl GraphNodeKey for KubeNodeKey {
    fn identity(&self) -> NodeIdentity {
        let s = |v: &String| BoltType::String(v.clone().into());
        // A state instance's time is the epoch milliseconds its UpdateState also SETs as `valid_from`, so the
        // MERGE finds the node the SET left (a text key never matched the integer written over it).
        let ms = |v: i64| BoltType::Integer(v.into());
        match self {
            KubeNodeKey::Cluster { uid } => {
                NodeIdentity::fixed(&["KubernetesCluster"], vec![("uid", s(uid))])
            }
            // The one taxonomy anchor every state instance is OF_TYPE, bound like any other node: an unbound `(:State)`
            // made a new empty node at every transition (docs/architecture/graph-write-path.md).
            KubeNodeKey::State => NodeIdentity::fixed(&["State"], vec![]),
            KubeNodeKey::Namespace { name, cluster_uid } => NodeIdentity::fixed(
                &["Namespace"],
                vec![("name", s(name)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::NamespaceState {
                name,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["NamespaceState"],
                vec![
                    ("namespace_name", s(name)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::Deployment { uid, cluster_uid } => NodeIdentity::fixed(
                &["KubernetesDeployment"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::DeploymentState {
                uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["DeploymentState"],
                vec![
                    ("deployment_uid", s(uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::ReplicaSet { uid, cluster_uid } => NodeIdentity::fixed(
                &["ReplicaSet"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::ReplicaSetState {
                uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["ReplicaSetState"],
                vec![
                    ("deployment_uid", s(uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::Pod { uid, cluster_uid } => NodeIdentity::fixed(
                &["Pod"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::StatefulSet { uid, cluster_uid } => NodeIdentity::fixed(
                &["StatefulSet"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::DaemonSet { uid, cluster_uid } => NodeIdentity::fixed(
                &["DaemonSet"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::Node { uid, cluster_uid } => NodeIdentity::fixed(
                &["KubernetesNode"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::NodeState {
                uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["KubernetesNodeState"],
                vec![
                    ("uid", s(uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::Job { uid, cluster_uid } => NodeIdentity::fixed(
                &["KubernetesJob"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::JobState {
                uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["KubernetesJobState"],
                vec![
                    ("uid", s(uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            // Keyed on `pod_uid` and `valid_from`, as PodContainerState is. Its pattern once named its properties
            // after the variable it was given (`sinst_uid`, `sinst_valid_from`), so the same key matched different
            // properties in different ops; the nodes already written that way keep those names.
            KubeNodeKey::PodState {
                pod_uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["PodState"],
                vec![
                    ("pod_uid", s(pod_uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::PodContainer {
                pod_uid,
                name,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["PodContainer"],
                vec![
                    ("pod_uid", s(pod_uid)),
                    ("name", s(name)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::PodContainerState {
                pod_uid,
                name,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["PodContainerState"],
                vec![
                    ("pod_uid", s(pod_uid)),
                    ("name", s(name)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::Volume {
                name,
                namespace,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["Volume"],
                vec![
                    ("name", s(name)),
                    ("namespace", s(namespace)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::PersistentVolumeClaim {
                name,
                namespace,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["PersistentVolumeClaim"],
                vec![
                    ("name", s(name)),
                    ("namespace", s(namespace)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::Secret {
                name,
                namespace,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["Secret"],
                vec![
                    ("name", s(name)),
                    ("namespace", s(namespace)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::ConfigMap {
                name,
                namespace,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["ConfigMap"],
                vec![
                    ("name", s(name)),
                    ("namespace", s(namespace)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            // ----------------------------------------------------------------
            // Flux source-controller: OCIRepository
            //
            // Anchor node keyed by uid — same pattern as KubernetesJob.
            // The uid comes from metadata.uid on the OCIRepository object,
            // guaranteed unique within the cluster.
            // ----------------------------------------------------------------
            KubeNodeKey::FluxOciRepository { uid, cluster_uid } => NodeIdentity::fixed(
                &["FluxOCIRepository"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            // State node keyed by (uid, valid_from).
            // valid_from is sourced from status.artifact.last_update_time —
            // the moment Flux resolved a new digest from the registry — not
            // observer wall-clock time. This makes the state timeline reflect
            // actual source-controller reconciliation events.
            KubeNodeKey::FluxOciRepositoryState {
                uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["FluxOCIRepositoryState"],
                vec![
                    ("uid", s(uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::FluxOciRepositoryRef {
                name,
                namespace,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["FluxOCIRepository"],
                vec![
                    ("name", s(name)),
                    ("namespace", s(namespace)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
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
            KubeNodeKey::FluxKustomization { uid, cluster_uid } => NodeIdentity::fixed(
                &["FluxKustomization"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::FluxKustomizationState {
                uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["FluxKustomizationState"],
                vec![
                    ("uid", s(uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
            KubeNodeKey::PolarAgent { uid, cluster_uid } => NodeIdentity::fixed(
                &["KubernetesPolarAgent"],
                vec![("uid", s(uid)), ("cluster_uid", s(cluster_uid))],
            ),
            KubeNodeKey::PolarAgentResourceState {
                uid,
                valid_from,
                cluster_uid,
            } => NodeIdentity::fixed(
                &["KubernetesPolarAgentState"],
                vec![
                    ("uid", s(uid)),
                    ("valid_from", ms(*valid_from)),
                    ("cluster_uid", s(cluster_uid)),
                ],
            ),
        }
    }
}

impl NodeKeyVocabulary for KubeNodeKey {
    fn every_variant() -> Vec<Self> {
        let s = || "x".to_string();
        let all = vec![
            KubeNodeKey::State,
            KubeNodeKey::Cluster { uid: s() },
            KubeNodeKey::Namespace {
                name: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::NamespaceState {
                name: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::Deployment {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::DeploymentState {
                uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::ReplicaSet {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::ReplicaSetState {
                uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::Pod {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::PodState {
                pod_uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::PodContainer {
                pod_uid: s(),
                name: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::PodContainerState {
                pod_uid: s(),
                name: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::Volume {
                name: s(),
                namespace: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::PersistentVolumeClaim {
                name: s(),
                namespace: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::Secret {
                name: s(),
                namespace: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::ConfigMap {
                name: s(),
                namespace: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::Job {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::StatefulSet {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::DaemonSet {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::Node {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::NodeState {
                uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::JobState {
                uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::FluxOciRepository {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::FluxOciRepositoryState {
                uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::FluxKustomization {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::FluxKustomizationState {
                uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
            KubeNodeKey::FluxOciRepositoryRef {
                name: s(),
                namespace: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::PolarAgent {
                uid: s(),
                cluster_uid: s(),
            },
            KubeNodeKey::PolarAgentResourceState {
                uid: s(),
                valid_from: 0,
                cluster_uid: s(),
            },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                KubeNodeKey::State
                | KubeNodeKey::Cluster { .. }
                | KubeNodeKey::Namespace { .. }
                | KubeNodeKey::NamespaceState { .. }
                | KubeNodeKey::Deployment { .. }
                | KubeNodeKey::DeploymentState { .. }
                | KubeNodeKey::ReplicaSet { .. }
                | KubeNodeKey::ReplicaSetState { .. }
                | KubeNodeKey::Pod { .. }
                | KubeNodeKey::PodState { .. }
                | KubeNodeKey::PodContainer { .. }
                | KubeNodeKey::PodContainerState { .. }
                | KubeNodeKey::Volume { .. }
                | KubeNodeKey::PersistentVolumeClaim { .. }
                | KubeNodeKey::Secret { .. }
                | KubeNodeKey::ConfigMap { .. }
                | KubeNodeKey::Job { .. }
                | KubeNodeKey::StatefulSet { .. }
                | KubeNodeKey::DaemonSet { .. }
                | KubeNodeKey::Node { .. }
                | KubeNodeKey::NodeState { .. }
                | KubeNodeKey::JobState { .. }
                | KubeNodeKey::FluxOciRepository { .. }
                | KubeNodeKey::FluxOciRepositoryState { .. }
                | KubeNodeKey::FluxKustomization { .. }
                | KubeNodeKey::FluxKustomizationState { .. }
                | KubeNodeKey::FluxOciRepositoryRef { .. }
                | KubeNodeKey::PolarAgent { .. }
                | KubeNodeKey::PolarAgentResourceState { .. } => {}
            }
        }
        all
    }
}

impl KubeNodeKey {
    /// Whether nothing projects this node itself, so the only thing known about it is what an owner reference
    /// on a pod says (its name). The pod's handler records that name; the other owners have their own handlers.
    pub fn is_anchor_only(&self) -> bool {
        matches!(
            self,
            KubeNodeKey::StatefulSet { .. } | KubeNodeKey::DaemonSet { .. }
        )
    }

    /// The node an owner reference on a pod names, in the cluster `cluster_uid` names.
    pub fn from_owner_reference(owner: &OwnerReference, cluster_uid: &str) -> Option<KubeNodeKey> {
        let uid = owner.uid.clone();
        let cluster_uid = cluster_uid.to_string();
        match owner.kind.as_str() {
            "ReplicaSet" => KubeNodeKey::ReplicaSet { uid, cluster_uid }.into(),
            "Deployment" => KubeNodeKey::Deployment { uid, cluster_uid }.into(),
            "Job" => KubeNodeKey::Job { uid, cluster_uid }.into(),
            "StatefulSet" => KubeNodeKey::StatefulSet { uid, cluster_uid }.into(),
            "DaemonSet" => KubeNodeKey::DaemonSet { uid, cluster_uid }.into(),
            "Node" => KubeNodeKey::Node { uid, cluster_uid }.into(),
            // The operator's pods, owned by the PolarAgent resource that declares them.
            "PolarAgent" => KubeNodeKey::PolarAgent { uid, cluster_uid }.into(),
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

    fn key(kind: &str) -> Option<KubeNodeKey> {
        KubeNodeKey::from_owner_reference(&owner(kind), "c-1")
    }

    #[test]
    fn replicasets_and_deployments_are_owners_the_graph_knows() {
        assert!(matches!(
            key("ReplicaSet"),
            Some(KubeNodeKey::ReplicaSet { .. })
        ));
        assert!(matches!(
            key("Deployment"),
            Some(KubeNodeKey::Deployment { .. })
        ));
    }

    #[test]
    fn a_job_owner_is_the_job_node_the_consumer_already_projects() {
        assert_eq!(
            key("Job"),
            Some(KubeNodeKey::Job {
                uid: "u-1".into(),
                cluster_uid: "c-1".into()
            })
        );
    }

    #[test]
    fn a_polar_agent_owner_is_the_resource_that_declares_the_pod() {
        assert_eq!(
            key("PolarAgent"),
            Some(KubeNodeKey::PolarAgent {
                uid: "u-1".into(),
                cluster_uid: "c-1".into()
            })
        );
    }

    #[test]
    fn a_node_owner_is_the_node_the_consumer_projects() {
        assert_eq!(
            key("Node"),
            Some(KubeNodeKey::Node {
                uid: "u-1".into(),
                cluster_uid: "c-1".into()
            })
        );
    }

    #[test]
    fn statefulsets_and_daemonsets_get_anchors_named_for_their_kind() {
        for (kind, label) in [("StatefulSet", "StatefulSet"), ("DaemonSet", "DaemonSet")] {
            let key = key(kind).unwrap_or_else(|| panic!("{kind} should map"));
            let (pattern, params) = key.cypher_match("n");
            assert_eq!(
                pattern,
                format!("(n:{label} {{ uid: $n_uid, cluster_uid: $n_cluster_uid }})"),
                "{kind}"
            );
            assert_eq!(params.len(), 2, "{kind}");
            assert!(key.is_anchor_only(), "{kind}");
        }
    }

    #[test]
    fn only_owners_nothing_else_projects_are_anchor_only() {
        for kind in ["ReplicaSet", "Deployment", "Job", "Node", "PolarAgent"] {
            let key = key(kind).expect(kind);
            assert!(
                !key.is_anchor_only(),
                "{kind} is projected by its own handler"
            );
        }
    }

    #[test]
    fn a_kind_the_graph_has_no_node_for_is_skipped() {
        for kind in ["CronJob", "Cluster", "Widget"] {
            assert!(key(kind).is_none(), "{kind}");
        }
    }
}
