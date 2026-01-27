use neo4rs::{BoltNull, BoltType};
use neo4rs::{Graph, Query};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};
/// Minimal typed intent that processors emit.
/// Keep this small and stable; extend later with versions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeKey {
    OCIRegistry {
        hostname: String,
    },
    /// A human-facing ref (tag or digest), not identity.
    ContainerImageRef {
        normalized: String,
    },
    /// Canonical OCI artifact (manifest / index)
    OCIArtifact {
        digest: String, // sha256:...
    },
    /// Content-addressed filesystem layer
    OCILayer {
        digest: String, // sha256:...
    },
    /// OCI config object
    OCIConfig {
        digest: String, // sha256:...
        os: String,
        arch: String,
        created: String,
        entrypoint: String,
        cmd: String,
    },
    /// Observed / asserted software component
    ComponentClaim {
        claim_type: String, // "rpm", "deb", "pip", "file", ...
        name: String,
        version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GraphValue {
    String(String),
    Bool(bool),
    I64(i64),
    F64(f64),

    Bytes(Vec<u8>),

    List(Vec<GraphValue>),
    Map(Vec<(String, GraphValue)>),

    Null,
}

impl From<GraphValue> for BoltType {
    fn from(v: GraphValue) -> Self {
        match v {
            GraphValue::String(s) => s.into(),
            GraphValue::Bool(b) => b.into(),
            GraphValue::I64(i) => i.into(),
            GraphValue::F64(f) => f.into(),
            GraphValue::Bytes(b) => b.into(),
            GraphValue::List(xs) => xs
                .into_iter()
                .map(BoltType::from)
                .collect::<Vec<_>>()
                .into(),
            GraphValue::Map(kvs) => kvs
                .into_iter()
                .map(|(k, v)| (k, BoltType::from(v)))
                .collect::<std::collections::HashMap<_, _>>()
                .into(),
            GraphValue::Null => BoltType::Null(BoltNull),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Property(pub String, pub GraphValue);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GraphOp {
    /// Upsert a canonical node with properties.
    UpsertNode { key: NodeKey, props: Vec<Property> },

    /// Ensure a directed relationship exists between two canonical nodes.
    EnsureEdge {
        from: NodeKey,
        to: NodeKey,
        rel_type: String,
        props: Vec<Property>,
    },
}

// Message wrapper forGraphController
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphControllerMsg {
    Op(GraphOp),
}

pub struct GraphControllerState {
    pub graph: Graph,
}

pub struct GraphController;

impl GraphController {
    fn node_match_and_params(key: &NodeKey, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match key {
            NodeKey::OCIRegistry { hostname } => (
                format!("(:OCIRegistry {{ hostname: ${prefix}_hostname }})"),
                vec![(format!("{prefix}_hostname"), hostname.clone().into())],
            ),

            NodeKey::ContainerImageRef { normalized } => (
                format!("(:ContainerImageReference {{ normalized: ${prefix}_normalized }})"),
                vec![(format!("{prefix}_normalized"), normalized.clone().into())],
            ),

            NodeKey::OCIArtifact { digest } => (
                format!("(:OCIArtifact {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),


            NodeKey::OCILayer { digest } => (
                format!("(:OCILayer {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),

            // TODO: Add other types to the cypher
            NodeKey::OCIConfig { digest, ..} => (
                format!("(:OCIConfig {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),

            NodeKey::ComponentClaim {
                claim_type,
                name,
                version,
            } => (
                format!(
                    "(:ComponentClaim {{ type: ${prefix}_type, name: ${prefix}_name, version: ${prefix}_version }})"
                ),
                vec![
                    (format!("{prefix}_type"), claim_type.clone().into()),
                    (format!("{prefix}_name"), name.clone().into()),
                    (format!("{prefix}_version"), version.clone().into()),
                ],
            ),
        }
    }

    /// Compile GraphOp to Cypher string and Bolt parameters.
    /// Pure and deterministic.
    pub fn compile_graph_op(op: &GraphOp) -> (String, Vec<(String, BoltType)>) {
        match op {
            GraphOp::UpsertNode { key, props } => {
                let (node_pattern, mut params) = Self::node_match_and_params(key, "n");

                let mut cypher = format!(
                    "MERGE (n {})",
                    node_pattern.trim_start_matches('(').trim_end_matches(')')
                );

                if !props.is_empty() {
                    let sets = props
                        .iter()
                        .map(|Property(k, _)| format!("n.{k} = ${k}"))
                        .collect::<Vec<_>>()
                        .join(", ");

                    cypher.push_str(&format!("\nSET {sets}"));
                }

                for Property(k, v) in props {
                    params.push((k.clone(), v.clone().into()));
                }

                (cypher, params)
            }

            GraphOp::EnsureEdge {
                from,
                to,
                rel_type,
                props,
            } => {
                let (from_pat, mut params) = Self::node_match_and_params(from, "from");
                let (to_pat, mut to_params) = Self::node_match_and_params(to, "to");
                params.append(&mut to_params);

                let mut cypher = format!(
                    "MERGE (a {})\nMERGE (b {})\nMERGE (a)-[r:{}]->(b)",
                    from_pat.trim_start_matches('(').trim_end_matches(')'),
                    to_pat.trim_start_matches('(').trim_end_matches(')'),
                    rel_type
                );

                if !props.is_empty() {
                    let sets = props
                        .iter()
                        .map(|Property(k, _)| format!("r.{k} = ${k}"))
                        .collect::<Vec<_>>()
                        .join(", ");

                    cypher.push_str(&format!("\nSET {sets}"));
                }

                for Property(k, v) in props {
                    params.push((k.clone(), v.clone().into()));
                }

                (cypher, params)
            }
        }
    }
}
#[async_trait]
impl Actor for GraphController {
    type Msg = GraphControllerMsg;
    type State = GraphControllerState;
    type Arguments = Graph;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        graph: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting. Connecting to neo4j.");

        Ok(GraphControllerState { graph })
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GraphControllerMsg::Op(op) => {
                let span = tracing::trace_span!("GraphController.handle_op");
                let _guard = span.enter();
                let (cypher, params) = Self::compile_graph_op(&op);

                let mut q = Query::new(cypher);
                for (k, v) in params {
                    q = q.param(&k, v);
                }
                let mut txn = state.graph.start_txn().await?;
                debug!("{q:?}");
                txn.run(q).await.map_err(|e| {
                    ActorProcessingErr::from(format!("neo4j execution failed: {:?}", e))
                })?;
                txn.commit().await?;
                trace!("transaction committed")
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod unittests {
    use neo4rs::BoltBoolean;

    use crate::graph::*;
    #[test]
    fn test_compile_graph_op_upsert_container_image_ref() {
        let op = GraphOp::UpsertNode {
            key: NodeKey::ContainerImageRef {
                normalized: "registry.example.com/app@sha256:deadbeef".to_string(),
            },
            props: vec![Property("tag".into(), GraphValue::String("latest".into()))],
        };

        let (cypher, params) = GraphController::compile_graph_op(&op);
        println!("{cypher}");
        println!("{params:?}");

        assert!(cypher.contains("MERGE (n:ContainerImageReference { normalized: $normalized })"));
        assert!(cypher.contains("SET n.last_resolved"));
        // Ensure no extra quotes creep in
        let normalized_param = params.iter().find(|(k, _)| k == "normalized").unwrap();
        assert_eq!(
            normalized_param.1,
            BoltType::String("registry.example.com/app@sha256:deadbeef".into())
        );
    }

    #[test]
    fn test_compile_graph_op_ensure_edge() {
        let op = GraphOp::EnsureEdge {
            from: NodeKey::OCIRegistry {
                hostname: "registry.example.com".to_string(),
            },
            to: NodeKey::ContainerImageRef {
                normalized: "registry.example.com/app@sha256:deadbeef".to_string(),
            },
            rel_type: "HOSTS".to_string(),
            props: vec![Property("verified".into(), GraphValue::Bool(true))],
        };

        let (cypher, params) = GraphController::compile_graph_op(&op);

        println!("{cypher}");
        println!("{params:?}");

        assert!(cypher.contains("MERGE (a:OCIRegistry { hostname: $from_hostname })"));
        assert!(cypher.contains("MERGE (b:ContainerImageReference { normalized: $to_normalized })"));
        assert!(cypher.contains("MERGE (a)-[r:HOSTS]->(b)"));
        assert!(cypher.contains("SET r.verified = $verified"));

        // Check parameters
        assert_eq!(
            params,
            vec![
                (
                    "from_hostname".to_string(),
                    BoltType::String("registry.example.com".into())
                ),
                (
                    "to_normalized".to_string(),
                    BoltType::String("registry.example.com/app@sha256:deadbeef".into())
                ),
                (
                    "verified".to_string(),
                    BoltType::Boolean(BoltBoolean { value: true })
                )
            ]
        );
    }
}

#[cfg(test)]
/// GraphControllers's integration test suite.
/// TODO: I can't why, but these tests fail despite the data showing up in the graph. The issue is the row returned is always empty, but this isn't the case in cypher-shell
/// I have ruled out timing, and the connection is clearly successful. More investigation is needed.
mod integration_tests {
    use super::*;
    use neo4rs::{Graph, Query};
    use ractor::{concurrency::Duration, Actor};
    use std::sync::Once;
    use testcontainers::runners::AsyncRunner;
    use testcontainers::ContainerAsync;
    use testcontainers_modules::neo4j::{Neo4j, Neo4jImage};
    use tracing::{debug, instrument};

    static INIT_TRACING: Once = Once::new();

    fn init_test_logging() {
        INIT_TRACING.call_once(|| {
            crate::init_logging("graph-controller-tests".to_string());
        });
    }

    /// Start Neo4j and return the container and graph handle
    #[instrument(level = "debug")]
    async fn start_neo4j() -> (ContainerAsync<Neo4jImage>, Graph) {
        debug!("Starting neo4j container");
        let container = Neo4j::default().start().await.unwrap();

        let uri = format!(
            "bolt://{}:{}",
            container.get_host().await.unwrap(),
            container.image().bolt_port_ipv4().unwrap()
        );
        let config = neo4rs::ConfigBuilder::new()
            .uri(uri)
            .user(container.image().user().unwrap())
            .password(container.image().password().unwrap())
            .build()
            .unwrap();

        let graph = Graph::connect(config).unwrap();
        (container, graph)
    }

    #[tokio::test]
    #[instrument(level = "debug")]
    async fn graph_controller_upserts_container_image_ref() {
        init_test_logging();

        let (_container, graph) = start_neo4j().await;

        let (controller, _) = Actor::spawn(None, GraphController, graph.clone())
            .await
            .unwrap();

        // Send a GraphOp to upsert a ContainerImageReference
        let normalized = "registry.example.com/app@sha256:deadbeef".to_string();
        let op = GraphOp::UpsertNode {
            key: NodeKey::ContainerImageRef {
                normalized: normalized.clone(),
            },
            props: vec![Property(
                "media_type".into(),
                GraphValue::String("application/vnd.oci.image.manifest.v1+json".to_string()),
            )],
        };

        controller.send_message(GraphControllerMsg::Op(op)).unwrap();
        ractor::concurrency::sleep(Duration::from_secs(1)).await;
        //kill actor since we're done with it
        // also frees up the neo4j connection pool.
        // Give async actor time to run
        controller.kill();
        // Validate node exists in Neo4j
        let mut q = Query::new(
            "MATCH (n:ContainerImageReference)
             WHERE n.normalized = $normalized
             RETURN n.normalized AS reference"
                .to_string(),
        );
        q = q.param("normalized", normalized.clone());

        debug!("{q:?}");
        let mut result = graph.execute(q).await.unwrap();
        let row = result.next().await.unwrap().unwrap();
        debug!("{row:?}");
        // Only media_type prop was set
        assert_eq!(row.get::<String>("reference").unwrap(), normalized);
    }

    #[tokio::test]
    #[instrument(level = "debug")]
    async fn graph_controller_creates_edge() {
        init_test_logging();

        let (_container, graph) = start_neo4j().await;

        let (controller, _) = Actor::spawn(None, GraphController, graph.clone())
            .await
            .unwrap();

        // Upsert two nodes first
        let registry = "registry.example.com";
        let image_ref = "registry.example.com/app@sha256:deadbeef";

        controller
            .send_message(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: NodeKey::OCIRegistry {
                    hostname: registry.into(),
                },
                props: vec![],
            }))
            .unwrap();

        controller
            .send_message(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: NodeKey::ContainerImageRef {
                    normalized: image_ref.into(),
                },
                props: vec![],
            }))
            .unwrap();

        // Create edge
        controller
            .send_message(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: NodeKey::OCIRegistry {
                    hostname: registry.into(),
                },
                to: NodeKey::ContainerImageRef {
                    normalized: image_ref.into(),
                },
                rel_type: "HOSTS".into(),
                props: vec![Property("verified".into(), GraphValue::Bool(true))],
            }))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        // Validate edge exists
        let query = Query::new(
             "MATCH (a:OCIRegistry { hostname: $hostname })-[r:HOSTS]->(b:ContainerImageReference { normalized: $normalized }) RETURN r.verified AS verified".to_string()
         )
         .param("hostname", registry)
         .param("normalized", image_ref);

        let mut result = graph.execute(query).await.unwrap();
        let row = result.next().await.unwrap().unwrap();

        assert_eq!(row.get::<bool>("verified").unwrap(), true);
    }
}
