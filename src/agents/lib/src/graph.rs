use neo4rs::{Graph, Query};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;
/// Minimal typed intent that processors emit.
/// Keep this small and stable; extend later with versions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeKey {
    OCIRegistry {
        hostname: String,
    },
    ContainerImageRef {
        normalized: String, /* e.g. registry/repo:tag or @sha */
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Property(pub String, pub Value);

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
                let (cypher, params) = compile_graph_op(&op);

                let mut q = Query::new(cypher);
                for (k, v) in params {
                    match v {
                        Value::String(s) => q = q.param(&k, s),
                        Value::Bool(b) => q = q.param(&k, b),
                        Value::Number(n) if n.is_i64() => q = q.param(&k, n.as_i64().unwrap()),
                        Value::Number(n) if n.is_f64() => q = q.param(&k, n.as_f64().unwrap()),
                        Value::Null => {}
                        _ => panic!("unsupported param type"),
                    }
                }
                debug!("{q:?}");
                state.graph.run(q).await.map_err(|e| {
                    ActorProcessingErr::from(format!("neo4j execution failed: {:?}", e))
                })?;
            }
        }
        Ok(())
    }
}

/// Compile GraphOp to Cypher string and parameters.
/// Kept pure for unit testing.
pub fn compile_graph_op(op: &GraphOp) -> (String, Vec<(String, serde_json::Value)>) {
    match op {
        GraphOp::UpsertNode { key, props } => match key {
            NodeKey::OCIRegistry { hostname } => {
                let cypher = "MERGE (n:OCIRegistry { hostname: $hostname })\nSET n.last_seen = coalesce(n.last_seen, timestamp())".to_string();
                let mut params = vec![("hostname".to_string(), serde_json::json!(hostname))];
                for Property(k, v) in props {
                    params.push((k.clone(), v.clone()));
                }
                (cypher, params)
            }
            NodeKey::ContainerImageRef { normalized } => {
                let cypher = "MERGE (n:ContainerImageReference { normalized: $normalized })\nSET n.last_resolved = coalesce(n.last_resolved, timestamp())".to_string();
                let mut params = vec![("normalized".to_string(), serde_json::json!(normalized))];
                for Property(k, v) in props {
                    params.push((k.clone(), v.clone()));
                }
                (cypher, params)
            }
        },
        GraphOp::EnsureEdge {
            from,
            to,
            rel_type,
            props,
        } => {
            // Only two node types for this epic; expand as needed.
            let (from_match, mut params) = match from {
                NodeKey::OCIRegistry { hostname } => (
                    "(a:OCIRegistry { hostname: $from_hostname })".to_string(),
                    vec![("from_hostname".to_string(), serde_json::json!(hostname))],
                ),
                NodeKey::ContainerImageRef { normalized } => (
                    "(a:ContainerImageReference { normalized: $from_normalized })".to_string(),
                    vec![("from_normalized".to_string(), serde_json::json!(normalized))],
                ),
            };
            let (to_match, mut to_params) = match to {
                NodeKey::OCIRegistry { hostname } => (
                    "(b:OCIRegistry { hostname: $to_hostname })".to_string(),
                    vec![("to_hostname".to_string(), serde_json::json!(hostname))],
                ),
                NodeKey::ContainerImageRef { normalized } => (
                    "(b:ContainerImageReference { normalized: $to_normalized })".to_string(),
                    vec![("to_normalized".to_string(), serde_json::json!(normalized))],
                ),
            };
            params.append(&mut to_params);

            let mut set_clauses = Vec::new();
            for Property(k, _) in props {
                set_clauses.push(format!("r.{} = ${}", k, k));
            }
            let mut cypher = format!(
                "MERGE {from_match}\nMERGE {to_match}\nMERGE (a)-[r:{rel}]->(b)",
                from_match = from_match,
                to_match = to_match,
                rel = rel_type,
            );
            if !set_clauses.is_empty() {
                cypher = format!("{}\nSET {}", cypher, set_clauses.join(", "));
            }
            for Property(k, v) in props {
                params.push((k.clone(), v.clone()));
            }
            (cypher, params)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn test_compile_graph_op_upsert_oci_registry() {
        let op = GraphOp::UpsertNode {
            key: NodeKey::OCIRegistry {
                hostname: "registry.example.com".to_string(),
            },
            props: vec![Property("extra".into(), Value::String("foo".into()))],
        };

        let (cypher, params) = compile_graph_op(&op);

        assert!(cypher.contains("MERGE (n:OCIRegistry { hostname: $hostname })"));
        assert!(cypher.contains("SET n.last_seen"));
        assert_eq!(
            params,
            vec![
                (
                    "hostname".to_string(),
                    Value::String("registry.example.com".into())
                ),
                ("extra".to_string(), Value::String("foo".into()))
            ]
        );
    }

    #[test]
    fn test_compile_graph_op_upsert_container_image_ref() {
        let op = GraphOp::UpsertNode {
            key: NodeKey::ContainerImageRef {
                normalized: "registry.example.com/app@sha256:deadbeef".to_string(),
            },
            props: vec![Property("tag".into(), Value::String("latest".into()))],
        };

        let (cypher, params) = compile_graph_op(&op);

        assert!(cypher.contains("MERGE (n:ContainerImageReference { normalized: $normalized })"));
        assert!(cypher.contains("SET n.last_resolved"));
        // Ensure no extra quotes creep in
        let normalized_param = params.iter().find(|(k, _)| k == "normalized").unwrap();
        assert_eq!(
            normalized_param.1,
            Value::String("registry.example.com/app@sha256:deadbeef".into())
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
            props: vec![Property("verified".into(), Value::Bool(true))],
        };

        let (cypher, params) = compile_graph_op(&op);

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
                    Value::String("registry.example.com".into())
                ),
                (
                    "to_normalized".to_string(),
                    Value::String("registry.example.com/app@sha256:deadbeef".into())
                ),
                ("verified".to_string(), Value::Bool(true))
            ]
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use neo4rs::{Graph, Query};
    use ractor::{concurrency::Duration, Actor};
    use std::sync::Once;
    use testcontainers::runners::AsyncRunner;
    use testcontainers::ContainerAsync;
    use testcontainers_modules::neo4j::{Neo4j, Neo4jImage};
    use tracing::debug;

    static INIT_TRACING: Once = Once::new();

    fn init_test_logging() {
        INIT_TRACING.call_once(|| {
            crate::init_logging("graph-controller-tests".to_string());
        });
    }

    /// Start Neo4j and return the container and graph handle
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
                serde_json::json!("application/vnd.oci.image.manifest.v1+json"),
            )],
        };

        controller.send_message(GraphControllerMsg::Op(op)).unwrap();

        // Give async actor time to run
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Validate node exists in Neo4j
        let query = Query::new(
            "MATCH (n:ContainerImageReference { normalized: $normalized }) RETURN n.digest AS digest, n.media_type AS media_type".to_string()
        )
        .param("normalized", normalized.clone());

        let mut result = graph.execute(query).await.unwrap();
        let row = result.next().await.unwrap().unwrap();

        // Only media_type prop was set
        assert_eq!(
            row.get::<String>("media_type").unwrap(),
            "application/vnd.oci.image.manifest.v1+json"
        );
    }

    #[tokio::test]
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
                props: vec![Property("verified".into(), serde_json::json!(true))],
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
