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
    fn identity(&self) -> NodeIdentity {
        let s = |v: &String| BoltType::String(v.clone().into());
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
            KubeNodeKey::Deployment { uid } => {
                NodeIdentity::fixed(&["KubernetesDeployment"], vec![("uid", s(uid))])
            }
            KubeNodeKey::DeploymentState { uid, valid_from } => NodeIdentity::fixed(
                &["DeploymentState"],
                vec![("deployment_uid", s(uid)), ("valid_from", s(valid_from))],
            ),
            KubeNodeKey::ReplicaSet { uid } => {
                NodeIdentity::fixed(&["ReplicaSet"], vec![("uid", s(uid))])
            }
            KubeNodeKey::ReplicaSetState { uid, valid_from } => NodeIdentity::fixed(
                &["ReplicaSetState"],
                vec![("deployment_uid", s(uid)), ("valid_from", s(valid_from))],
            ),
            KubeNodeKey::Pod { uid } => NodeIdentity::fixed(&["Pod"], vec![("uid", s(uid))]),
            KubeNodeKey::StatefulSet { uid } => {
                NodeIdentity::fixed(&["StatefulSet"], vec![("uid", s(uid))])
            }
            KubeNodeKey::DaemonSet { uid } => {
                NodeIdentity::fixed(&["DaemonSet"], vec![("uid", s(uid))])
            }
            KubeNodeKey::Node { uid } => NodeIdentity::fixed(&["Node"], vec![("uid", s(uid))]),
            KubeNodeKey::Job { uid } => {
                NodeIdentity::fixed(&["KubernetesJob"], vec![("uid", s(uid))])
            }
            KubeNodeKey::JobState { uid, valid_from } => NodeIdentity::fixed(
                &["KubernetesJobState"],
                vec![("uid", s(uid)), ("valid_from", s(valid_from))],
            ),
            // Keyed on `pod_uid` and `valid_from`, as PodContainerState is. Its pattern once named its properties
            // after the variable it was given (`sinst_uid`, `sinst_valid_from`), so the same key matched different
            // properties in different ops; the nodes already written that way keep those names.
            KubeNodeKey::PodState {
                pod_uid,
                valid_from,
            } => NodeIdentity::fixed(
                &["PodState"],
                vec![("pod_uid", s(pod_uid)), ("valid_from", s(valid_from))],
            ),
            KubeNodeKey::PodContainer { pod_uid, name } => NodeIdentity::fixed(
                &["PodContainer"],
                vec![("pod_uid", s(pod_uid)), ("name", s(name))],
            ),
            KubeNodeKey::PodContainerState {
                pod_uid,
                name,
                valid_from,
            } => NodeIdentity::fixed(
                &["PodContainerState"],
                vec![
                    ("pod_uid", s(pod_uid)),
                    ("name", s(name)),
                    ("valid_from", s(valid_from)),
                ],
            ),
            KubeNodeKey::Volume { name, namespace } => NodeIdentity::fixed(
                &["Volume"],
                vec![("name", s(name)), ("namespace", s(namespace))],
            ),
            KubeNodeKey::PersistentVolumeClaim { name, namespace } => NodeIdentity::fixed(
                &["PersistentVolumeClaim"],
                vec![("name", s(name)), ("namespace", s(namespace))],
            ),
            KubeNodeKey::Secret { name, namespace } => NodeIdentity::fixed(
                &["Secret"],
                vec![("name", s(name)), ("namespace", s(namespace))],
            ),
            KubeNodeKey::ConfigMap { name, namespace } => NodeIdentity::fixed(
                &["ConfigMap"],
                vec![("name", s(name)), ("namespace", s(namespace))],
            ),
            // ----------------------------------------------------------------
            // Flux source-controller: OCIRepository
            //
            // Anchor node keyed by uid — same pattern as KubernetesJob.
            // The uid comes from metadata.uid on the OCIRepository object,
            // guaranteed unique within the cluster.
            // ----------------------------------------------------------------
            KubeNodeKey::FluxOciRepository { uid } => {
                NodeIdentity::fixed(&["FluxOCIRepository"], vec![("uid", s(uid))])
            }
            // State node keyed by (uid, valid_from).
            // valid_from is sourced from status.artifact.last_update_time —
            // the moment Flux resolved a new digest from the registry — not
            // observer wall-clock time. This makes the state timeline reflect
            // actual source-controller reconciliation events.
            KubeNodeKey::FluxOciRepositoryState { uid, valid_from } => NodeIdentity::fixed(
                &["FluxOCIRepositoryState"],
                vec![("uid", s(uid)), ("valid_from", s(valid_from))],
            ),
            KubeNodeKey::FluxOciRepositoryRef { name, namespace } => NodeIdentity::fixed(
                &["FluxOCIRepository"],
                vec![("name", s(name)), ("namespace", s(namespace))],
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
            KubeNodeKey::FluxKustomization { uid } => {
                NodeIdentity::fixed(&["FluxKustomization"], vec![("uid", s(uid))])
            }
            KubeNodeKey::FluxKustomizationState { uid, valid_from } => NodeIdentity::fixed(
                &["FluxKustomizationState"],
                vec![("uid", s(uid)), ("valid_from", s(valid_from))],
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
            KubeNodeKey::Deployment { uid: s() },
            KubeNodeKey::DeploymentState {
                uid: s(),
                valid_from: s(),
            },
            KubeNodeKey::ReplicaSet { uid: s() },
            KubeNodeKey::ReplicaSetState {
                uid: s(),
                valid_from: s(),
            },
            KubeNodeKey::Pod { uid: s() },
            KubeNodeKey::PodState {
                pod_uid: s(),
                valid_from: s(),
            },
            KubeNodeKey::PodContainer {
                pod_uid: s(),
                name: s(),
            },
            KubeNodeKey::PodContainerState {
                pod_uid: s(),
                name: s(),
                valid_from: s(),
            },
            KubeNodeKey::Volume {
                name: s(),
                namespace: s(),
            },
            KubeNodeKey::PersistentVolumeClaim {
                name: s(),
                namespace: s(),
            },
            KubeNodeKey::Secret {
                name: s(),
                namespace: s(),
            },
            KubeNodeKey::ConfigMap {
                name: s(),
                namespace: s(),
            },
            KubeNodeKey::Job { uid: s() },
            KubeNodeKey::StatefulSet { uid: s() },
            KubeNodeKey::DaemonSet { uid: s() },
            KubeNodeKey::Node { uid: s() },
            KubeNodeKey::JobState {
                uid: s(),
                valid_from: s(),
            },
            KubeNodeKey::FluxOciRepository { uid: s() },
            KubeNodeKey::FluxOciRepositoryState {
                uid: s(),
                valid_from: s(),
            },
            KubeNodeKey::FluxKustomization { uid: s() },
            KubeNodeKey::FluxKustomizationState {
                uid: s(),
                valid_from: s(),
            },
            KubeNodeKey::FluxOciRepositoryRef {
                name: s(),
                namespace: s(),
            },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                KubeNodeKey::State
                | KubeNodeKey::Cluster { .. }
                | KubeNodeKey::Namespace { .. }
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
                | KubeNodeKey::JobState { .. }
                | KubeNodeKey::FluxOciRepository { .. }
                | KubeNodeKey::FluxOciRepositoryState { .. }
                | KubeNodeKey::FluxKustomization { .. }
                | KubeNodeKey::FluxKustomizationState { .. }
                | KubeNodeKey::FluxOciRepositoryRef { .. } => {}
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
