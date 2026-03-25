use crate::graph::controller::{
    GraphNodeKey, GraphOp, GraphValue, IntoGraphKey, Property, handle_op,
};
use neo4rs::{BoltType, Graph};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::neo4j::{Neo4j, Neo4jImage};

// ── Shared fixture ─────────────────────────────────────────────────────────
//
// #[once] tells rstest to initialize this fixture exactly once for the
// entire test binary and reuse the result for every test that requests it.
// The return type must be Sync for this to work — Graph is Arc-backed so
// it satisfies that. The ContainerAsync handle is held inside the struct
// so it is never dropped while the fixture is alive.
//
// The fixture function is async — rstest handles the tokio runtime
// integration automatically when the test functions are also async.

pub struct Neo4jFixture {
    _container: ContainerAsync<Neo4jImage>,
    pub graph: Graph,
}

// Safety: ContainerAsync is not Sync by default but we never access it
// concurrently — we only hold it to prevent Drop. The graph field is the
// only thing tests interact with and Graph is Send + Sync.
unsafe impl Sync for Neo4jFixture {}

static NEO4J_FIXTURE: tokio::sync::OnceCell<Neo4jFixture> = tokio::sync::OnceCell::const_new();

async fn neo4j() -> &'static Neo4jFixture {
    NEO4J_FIXTURE
        .get_or_init(|| async {
            polar::init_logging("polar.graph.tests".to_string());

            let container = Neo4j::default()
                .start()
                .await
                .expect("failed to start Neo4j test container");

            let config = neo4rs::ConfigBuilder::new()
                .uri(format!(
                    "bolt://{}:{}",
                    container.get_host().await.expect("container host"),
                    container.image().bolt_port_ipv4().expect("bolt port"),
                ))
                .user(container.image().user().expect("user"))
                .password(container.image().password().expect("password"))
                .build()
                .expect("neo4rs config");

            let graph = neo4rs::Graph::connect(config).expect("neo4rs connect");

            Neo4jFixture {
                _container: container,
                graph,
            }
        })
        .await
}

// ── Graph assertion helpers ────────────────────────────────────────────────

async fn count_nodes(graph: &Graph, label: &str, where_clause: &str) -> i64 {
    let cypher = format!("MATCH (n:{label}) WHERE {where_clause} RETURN count(n) AS c");
    let mut result = graph.execute(neo4rs::Query::new(cypher)).await.unwrap();
    result
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>("c")
        .unwrap()
}

async fn count_edges(graph: &Graph, from_label: &str, rel: &str, to_label: &str) -> i64 {
    let cypher = format!("MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) RETURN count(*) AS c");
    let mut result = graph.execute(neo4rs::Query::new(cypher)).await.unwrap();
    result
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>("c")
        .unwrap()
}

/// Wipe the graph between tests. Because the container is shared, tests
/// would otherwise see each other's data. Call this at the start of any
/// test that needs a known-clean slate rather than relying on test ordering.
async fn clear_graph(graph: &Graph) {
    graph
        .run(neo4rs::Query::new("MATCH (n) DETACH DELETE n".to_string()))
        .await
        .unwrap();
}

// ── Test node keys ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct PodKey {
    uid: String,
}

impl GraphNodeKey for PodKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        let k = format!("{prefix}_uid");
        (
            format!("({prefix}:Pod {{ uid: ${k} }})"),
            vec![(k, BoltType::String(self.uid.clone().into()))],
        )
    }
}

#[derive(Debug, Clone)]
struct BuildJobKey {
    build_id: String,
}

impl GraphNodeKey for BuildJobKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        let k = format!("{prefix}_build_id");
        (
            format!("({prefix}:BuildJob {{ build_id: ${k} }})"),
            vec![(k, BoltType::String(self.build_id.clone().into()))],
        )
    }
}

#[derive(Debug, Clone)]
struct BuildJobStateKey {
    build_id: String,
    valid_from: String,
}

impl GraphNodeKey for BuildJobStateKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        let id_k = format!("{prefix}_build_id");
        let vf_k = format!("{prefix}_valid_from");
        (
            format!("({prefix}:BuildJobState {{ build_id: ${id_k}, valid_from: ${vf_k} }})"),
            vec![
                (id_k, BoltType::String(self.build_id.clone().into())),
                (vf_k, BoltType::String(self.valid_from.clone().into())),
            ],
        )
    }
}

#[derive(Debug, Clone)]
struct StateTypeKey {
    name: String,
}

impl GraphNodeKey for StateTypeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        let k = format!("{prefix}_name");
        (
            format!("({prefix}:State {{ name: ${k} }})"),
            vec![(k, BoltType::String(self.name.clone().into()))],
        )
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────
//
// Each test receives the shared fixture by naming it as a parameter.
// rstest injects it automatically — no manual setup needed.
// Tests run sequentially within the module to avoid clear_graph races.
// If you need parallelism, scope data to unique IDs per test instead
// of relying on clear_graph.

#[tokio::test]
async fn upsert_node_creates_node_with_properties() {
    let graph = &neo4j().await.graph;
    clear_graph(graph).await;

    handle_op(
        graph,
        &GraphOp::UpsertNode {
            key: BuildJobKey {
                build_id: "build-001".into(),
            }
            .into_key(),
            props: vec![
                Property(
                    "repo_url".into(),
                    GraphValue::String("https://github.com/example/repo".into()),
                ),
                Property(
                    "commit_sha".into(),
                    GraphValue::String("abc123def456".into()),
                ),
            ],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'build-001'").await,
        1
    );

    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (n:BuildJob { build_id: 'build-001' }) RETURN n.repo_url AS url".to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>("url").unwrap(),
        "https://github.com/example/repo"
    );
}

#[tokio::test]
async fn upsert_node_is_idempotent() {
    let graph = &neo4j().await.graph;
    clear_graph(graph).await;

    for _ in 0..3 {
        handle_op(
            graph,
            &GraphOp::UpsertNode {
                key: BuildJobKey {
                    build_id: "build-idem".into(),
                }
                .into_key(),
                props: vec![Property(
                    "repo_url".into(),
                    GraphValue::String("https://github.com".into()),
                )],
            },
        )
        .await
        .unwrap();
    }

    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'build-idem'").await,
        1,
        "MERGE should produce exactly one node regardless of call count"
    );
}

#[tokio::test]
async fn ensure_edge_cross_vocabulary() {
    let graph = &neo4j().await.graph;
    clear_graph(graph).await;

    handle_op(
        graph,
        &GraphOp::EnsureEdge {
            from: BuildJobKey {
                build_id: "build-xvocab".into(),
            }
            .into_key(),
            rel_type: "RUNNING_IN".into(),
            to: PodKey {
                uid: "pod-xvocab".into(),
            }
            .into_key(),
            props: vec![Property(
                "observed_at".into(),
                GraphValue::String("2024-01-01T00:00:00Z".into()),
            )],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'build-xvocab'").await,
        1
    );
    assert_eq!(count_nodes(graph, "Pod", "n.uid = 'pod-xvocab'").await, 1);
    assert_eq!(count_edges(graph, "BuildJob", "RUNNING_IN", "Pod").await, 1);
}

#[tokio::test]
async fn ensure_edge_is_idempotent() {
    let graph = &neo4j().await.graph;
    clear_graph(graph).await;

    for _ in 0..3 {
        handle_op(
            graph,
            &GraphOp::EnsureEdge {
                from: BuildJobKey {
                    build_id: "build-eidem".into(),
                }
                .into_key(),
                rel_type: "BUILT_BY".into(),
                to: PodKey {
                    uid: "pod-eidem".into(),
                }
                .into_key(),
                props: vec![],
            },
        )
        .await
        .unwrap();
    }

    assert_eq!(
        count_edges(graph, "BuildJob", "BUILT_BY", "Pod").await,
        1,
        "MERGE on the edge should produce exactly one relationship"
    );
}

#[tokio::test]
async fn update_state_creates_temporal_chain() {
    let graph = &neo4j().await.graph;
    clear_graph(graph).await;

    handle_op(
        graph,
        &GraphOp::UpdateState {
            resource_key: BuildJobKey {
                build_id: "build-state".into(),
            }
            .into_key(),
            state_type_key: StateTypeKey {
                name: "scheduled".into(),
            }
            .into_key(),
            state_instance_key: BuildJobStateKey {
                build_id: "build-state".into(),
                valid_from: "2024-01-01T00:00:00Z".into(),
            }
            .into_key(),
            state_instance_props: vec![
                Property("phase".into(), GraphValue::String("scheduled".into())),
                Property(
                    "valid_from".into(),
                    GraphValue::String("2024-01-01T00:00:00Z".into()),
                ),
            ],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'build-state'").await,
        1
    );
    assert_eq!(
        count_nodes(graph, "BuildJobState", "n.build_id = 'build-state'").await,
        1
    );
    assert_eq!(
        count_edges(graph, "BuildJob", "TRANSITIONED_TO", "BuildJobState").await,
        1
    );
    assert_eq!(
        count_edges(graph, "BuildJob", "HAS_STATE", "State").await,
        1
    );

    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (:BuildJob { build_id: 'build-state' })-[:TRANSITIONED_TO]->(s:BuildJobState) \
                 RETURN s.phase AS phase"
                .to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>("phase").unwrap(), "scheduled");
}

#[tokio::test]
async fn update_state_moves_has_state_pointer_on_transition() {
    let graph = &neo4j().await.graph;
    clear_graph(graph).await;

    for (phase, valid_from) in [
        ("scheduled", "2024-01-01T00:00:00Z"),
        ("running", "2024-01-01T00:01:00Z"),
    ] {
        handle_op(
            graph,
            &GraphOp::UpdateState {
                resource_key: BuildJobKey {
                    build_id: "build-tstate".into(),
                }
                .into_key(),
                state_type_key: StateTypeKey { name: phase.into() }.into_key(),
                state_instance_key: BuildJobStateKey {
                    build_id: "build-tstate".into(),
                    valid_from: valid_from.into(),
                }
                .into_key(),
                state_instance_props: vec![
                    Property("phase".into(), GraphValue::String(phase.into())),
                    Property("valid_from".into(), GraphValue::String(valid_from.into())),
                ],
            },
        )
        .await
        .unwrap();
    }

    // History is append-only — both state instances survive
    assert_eq!(
        count_nodes(graph, "BuildJobState", "n.build_id = 'build-tstate'").await,
        2,
        "both state instances must be preserved"
    );
    assert_eq!(
        count_edges(graph, "BuildJob", "TRANSITIONED_TO", "BuildJobState").await,
        2
    );

    // HAS_STATE is replaced, not accumulated
    assert_eq!(
        count_edges(graph, "BuildJob", "HAS_STATE", "State").await,
        1,
        "HAS_STATE must point at exactly one current state"
    );

    // HAS_STATE points at the latest state
    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (:BuildJob { build_id: 'build-tstate' })-[:HAS_STATE]->(s:State) \
                 RETURN s.name AS name"
                .to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>("name").unwrap(), "running");
}
