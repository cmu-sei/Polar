use cassini_client::TcpClientMessage;
use neo4rs::{BoltType, Graph};
use polar::graph::{GraphControllerState, GraphNodeKey, GraphOp};
use ractor::{ActorProcessingErr, ActorRef};

pub mod pods;
pub mod supervisor;

pub const BROKER_CLIENT_NAME: &str = "kubernetes.cluster.cassini.client";

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
    Cluster {
        uid: String,
    },
    Namespace {
        name: String,
        cluster_uid: String,
    },
    Pod {
        uid: String,
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
}

impl GraphNodeKey for KubeNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            KubeNodeKey::Cluster { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("MERGE ({prefix}_c:Cluster {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.clone().into()))],
                )
            }

            KubeNodeKey::Namespace { name, cluster_uid } => {
                let name_k = format!("{prefix}_name");
                let cluster_uid_k = format!("{prefix}_cluster_uid");
                (
                    format!(
                        "MERGE ({prefix}_n:Namespace {{ name: ${name_k}, cluster_uid: ${cluster_uid_k} }})"
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

            KubeNodeKey::Pod { uid } => {
                let uid_k = format!("{prefix}_uid");
                (
                    format!("MERGE ({prefix}_p:Pod {{ uid: ${uid_k} }})"),
                    vec![(uid_k, BoltType::String(uid.to_string().into()))],
                )
            }

            KubeNodeKey::PodContainer { pod_uid, name } => {
                let pod_uid_k = format!("{prefix}_pod_uid");
                let name_k = format!("{prefix}_name");
                (
                    format!(
                        "MERGE ({prefix}_c:PodContainer {{ pod_uid: ${pod_uid_k}, name: ${name_k} }})"
                    ),
                    vec![
                        (
                            pod_uid_k,
                            BoltType::String(pod_uid.to_string().into()),
                        ),
                        (name_k, BoltType::String(name.clone().into())),
                    ],
                )
            }

            KubeNodeKey::Volume { name, namespace } => {
                let name_k = format!("{prefix}_name");
                let namespace_k = format!("{prefix}_namespace");
                (
                    format!(
                        "MERGE ({prefix}_v:Volume {{ name: ${name_k}, namespace: ${namespace_k} }})"
                    ),
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
                "MERGE ({prefix}_v:PersistentVolumeClaim {{ name: ${name_k}, namespace: ${namespace_k} }})"
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
                    format!(
                        "MERGE ({prefix}_s:Secret {{ name: ${name_k}, namespace: ${namespace_k} }})"
                    ),
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
                        "MERGE ({prefix}_cm:ConfigMap {{ name: ${name_k}, namespace: ${namespace_k} }})"
                    ),
                    vec![
                        (name_k, BoltType::String(name.clone().into())),
                        (
                            namespace_k,
                            BoltType::String(namespace.clone().into()),
                        ),
                    ],
                )
            }
        }
    }
}
pub struct ClusterGraphController;

#[ractor::async_trait]
impl ractor::Actor for ClusterGraphController {
    type Msg = GraphOp<KubeNodeKey>;
    type State = GraphControllerState;
    type Arguments = Graph;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        graph: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(GraphControllerState { graph })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        op: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        polar::graph::handle_op::<KubeNodeKey>(&state.graph, &op).await?;
        Ok(())
    }
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
