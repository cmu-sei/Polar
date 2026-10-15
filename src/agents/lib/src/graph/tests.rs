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

use crate::graph::controller::{
    CypherIdentifierError, DynamicNodeKey, GraphNodeKey, GraphOp, GraphValue, IntoGraphKey,
    Property, handle_op, handle_query, validate_cypher_identifier,
};
use crate::graph::nodes::builds::{BackendJobIdentity, BuildNodeKey};

use neo4rs::{BoltType, Graph};
use polar_test_graph::{clear_graph, neo4j};

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

async fn count_edges(
    graph: &Graph,
    from_label: &str,
    rel: &str,
    to_label: &str,
    where_clause: &str,
) -> i64 {
    let cypher = format!(
        "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) WHERE {where_clause} RETURN count(*) AS c"
    );
    let mut result = graph.execute(neo4rs::Query::new(cypher)).await.unwrap();
    result
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>("c")
        .unwrap()
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
async fn validate_cypher_identifier_accepts_normal_identifiers() {
    assert!(validate_cypher_identifier("JiraCustomField").is_ok());
    assert!(validate_cypher_identifier("story_points").is_ok());
    assert!(validate_cypher_identifier("field_1").is_ok());
}

#[tokio::test]
async fn validate_cypher_identifier_rejects_empty() {
    assert!(matches!(
        validate_cypher_identifier(""),
        Err(CypherIdentifierError::Empty)
    ));
}

#[tokio::test]
async fn validate_cypher_identifier_rejects_leading_digit() {
    assert!(matches!(
        validate_cypher_identifier("1field"),
        Err(CypherIdentifierError::StartsWithDigit(_))
    ));
}

#[tokio::test]
async fn validate_cypher_identifier_rejects_injection_attempt() {
    // The exact shape of attack this exists to prevent: a field name
    // engineered to close the current Cypher clause and inject a
    // destructive statement.
    let err = validate_cypher_identifier("}) DETACH DELETE (n) //")
        .expect_err("must reject Cypher-breaking characters");
    assert!(matches!(err, CypherIdentifierError::InvalidCharacters(_)));
}

#[tokio::test]
async fn validate_cypher_identifier_rejects_reserved_keywords() {
    assert!(matches!(
        validate_cypher_identifier("DELETE"),
        Err(CypherIdentifierError::ReservedKeyword(_))
    ));
    assert!(
        matches!(
            validate_cypher_identifier("merge"),
            Err(CypherIdentifierError::ReservedKeyword(_))
        ),
        "keyword check must be case-insensitive"
    );
}

#[tokio::test]
async fn dynamic_node_key_construction_rejects_unsafe_label() {
    assert!(DynamicNodeKey::new("Valid", "key", "some value").is_ok());
    assert!(DynamicNodeKey::new("}) DROP //", "key", "some value").is_err());
    assert!(DynamicNodeKey::new("Valid", "}) DROP //", "some value").is_err());
}

#[tokio::test]
async fn backend_job_identity_construction_rejects_unsafe_input() {
    assert!(
        BackendJobIdentity::new("GitLabRunner", vec![("runner_id".into(), "42".into())]).is_ok()
    );
    assert!(
        BackendJobIdentity::new("}) DROP //", vec![("runner_id".into(), "42".into())]).is_err()
    );
    assert!(
        BackendJobIdentity::new("GitLabRunner", vec![("}) DROP //".into(), "42".into())]).is_err()
    );
}

#[tokio::test]
async fn backend_job_upserts_with_runtime_determined_label() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    let identity = BackendJobIdentity::new(
        "GitLabRunner",
        vec![("runner_id".to_string(), "42".to_string())],
    )
    .expect("valid identifier");
    handle_op(
        graph,
        &GraphOp::UpsertNode {
            key: BuildNodeKey::BackendJob(identity).into_key(),
            props: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        count_nodes(graph, "GitLabRunner", "n.runner_id = '42'").await,
        1
    );
}

#[tokio::test]
async fn dynamic_node_key_upserts_with_runtime_determined_label() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    let key = DynamicNodeKey::new("JiraIssue_CustomField", "field_key", "field-value-001")
        .expect("valid identifier");
    handle_op(
        graph,
        &GraphOp::UpsertNode {
            key: key.into_key(),
            props: vec![Property(
                "label_text".into(),
                GraphValue::String("some custom field value".into()),
            )],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        count_nodes(
            graph,
            "JiraIssue_CustomField",
            "n.field_key = 'field-value-001'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn upsert_node_creates_node_with_properties() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
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
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
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
async fn upsert_nodes_creates_multiple_nodes_with_distinct_properties() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    handle_op(
        graph,
        &GraphOp::UpsertNodes {
            items: vec![
                (
                    BuildJobKey {
                        build_id: "batch-001".into(),
                    }
                    .into_key(),
                    vec![Property(
                        "repo_url".into(),
                        GraphValue::String("https://github.com/example/repo-a".into()),
                    )],
                ),
                (
                    BuildJobKey {
                        build_id: "batch-002".into(),
                    }
                    .into_key(),
                    vec![Property(
                        "repo_url".into(),
                        GraphValue::String("https://github.com/example/repo-b".into()),
                    )],
                ),
            ],
        },
    )
    .await
    .unwrap();

    // Deliberately checking each item ended up with *its own* property
    // value, not a neighbor's -- this is exactly the failure mode a bug
    // in the per-item parameter prefixing would produce (one item's
    // property value silently overwriting another's under the same
    // unprefixed $repo_url parameter name).
    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'batch-001'").await,
        1
    );
    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'batch-002'").await,
        1
    );

    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (n:BuildJob { build_id: 'batch-001' }) RETURN n.repo_url AS url".to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>("url").unwrap(),
        "https://github.com/example/repo-a"
    );

    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (n:BuildJob { build_id: 'batch-002' }) RETURN n.repo_url AS url".to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>("url").unwrap(),
        "https://github.com/example/repo-b"
    );
}

#[tokio::test]
async fn upsert_nodes_is_idempotent() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    for _ in 0..3 {
        handle_op(
            graph,
            &GraphOp::UpsertNodes {
                items: vec![
                    (
                        BuildJobKey {
                            build_id: "batch-idem-1".into(),
                        }
                        .into_key(),
                        vec![Property(
                            "repo_url".into(),
                            GraphValue::String("https://github.com".into()),
                        )],
                    ),
                    (
                        BuildJobKey {
                            build_id: "batch-idem-2".into(),
                        }
                        .into_key(),
                        vec![Property(
                            "repo_url".into(),
                            GraphValue::String("https://gitlab.com".into()),
                        )],
                    ),
                ],
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'batch-idem-1'").await,
        1,
        "MERGE should produce exactly one node regardless of call count"
    );
    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'batch-idem-2'").await,
        1,
        "MERGE should produce exactly one node regardless of call count"
    );
}

#[tokio::test]
async fn ensure_edge_cross_vocabulary() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
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
    assert_eq!(
        count_edges(
            graph,
            "BuildJob",
            "RUNNING_IN",
            "Pod",
            "a.build_id = 'build-xvocab' AND b.uid = 'pod-xvocab'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn ensure_edge_is_idempotent() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
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
        count_edges(
            graph,
            "BuildJob",
            "BUILT_BY",
            "Pod",
            "a.build_id = 'build-eidem' AND b.uid = 'pod-eidem'"
        )
        .await,
        1,
        "MERGE on the edge should produce exactly one relationship"
    );
}

#[tokio::test]
async fn update_state_creates_temporal_chain() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
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
        count_edges(
            graph,
            "BuildJob",
            "TRANSITIONED_TO",
            "BuildJobState",
            "a.build_id = 'build-state'"
        )
        .await,
        1
    );
    assert_eq!(
        count_edges(
            graph,
            "BuildJob",
            "HAS_STATE",
            "State",
            "a.build_id = 'build-state'"
        )
        .await,
        1
    );
    assert_eq!(
        count_edges(
            graph,
            "BuildJob",
            "TRANSITIONED_TO",
            "BuildJobState",
            "a.build_id = 'build-state'"
        )
        .await,
        1
    );
    assert_eq!(
        count_edges(
            graph,
            "BuildJob",
            "HAS_STATE",
            "State",
            "a.build_id = 'build-state'"
        )
        .await,
        1
    );

    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (:BuildJob { build_id: 'build-state' })-[:TRANSITIONED_TO]->(s:BuildJobState) RETURN s.phase AS phase".to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>("phase").unwrap(), "scheduled");
}

#[tokio::test]
async fn update_state_moves_has_state_pointer_on_transition() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
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
        count_edges(
            graph,
            "BuildJob",
            "TRANSITIONED_TO",
            "BuildJobState",
            "a.build_id = 'build-tstate'"
        )
        .await,
        2
    );

    // HAS_STATE is replaced, not accumulated
    assert_eq!(
        count_edges(
            graph,
            "BuildJob",
            "HAS_STATE",
            "State",
            "a.build_id = 'build-tstate'"
        )
        .await,
        1,
        "HAS_STATE must point at exactly one current state"
    );

    // HAS_STATE points at the latest state
    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (:BuildJob { build_id: 'build-tstate' })-[:HAS_STATE]->(s:State) RETURN s.name AS name".to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>("name").unwrap(), "running");
}

#[tokio::test]
async fn transaction_runs_mixed_ops_atomically() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    handle_op(
        graph,
        &GraphOp::Transaction {
            ops: vec![
                GraphOp::UpsertNode {
                    key: BuildJobKey {
                        build_id: "txn-parent".into(),
                    }
                    .into_key(),
                    props: vec![Property(
                        "repo_url".into(),
                        GraphValue::String("https://github.com/example/parent".into()),
                    )],
                },
                GraphOp::UpsertNode {
                    key: BuildJobKey {
                        build_id: "txn-child".into(),
                    }
                    .into_key(),
                    props: vec![Property(
                        "repo_url".into(),
                        GraphValue::String("https://github.com/example/child".into()),
                    )],
                },
                GraphOp::EnsureEdge {
                    from: BuildJobKey {
                        build_id: "txn-parent".into(),
                    }
                    .into_key(),
                    to: BuildJobKey {
                        build_id: "txn-child".into(),
                    }
                    .into_key(),
                    rel_type: "TRIGGERED".into(),
                    props: vec![],
                },
            ],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'txn-parent'").await,
        1
    );
    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'txn-child'").await,
        1
    );

    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (:BuildJob { build_id: 'txn-parent' })-[r:TRIGGERED]->(:BuildJob { build_id: 'txn-child' }) RETURN count(r) AS c".to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<i64>("c").unwrap(),
        1,
        "all three ops in the transaction should have committed together"
    );
}

#[tokio::test]
async fn ensure_edge_rejects_unsafe_rel_type_before_touching_the_graph() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    let result = handle_op(
        graph,
        &GraphOp::EnsureEdge {
            from: BuildJobKey {
                build_id: "reject-src".into(),
            }
            .into_key(),
            to: BuildJobKey {
                build_id: "reject-dst".into(),
            }
            .into_key(),
            // The exact shape of attack this backstop exists to catch:
            // a caller-supplied relationship type engineered to close
            // the current Cypher clause and inject a destructive
            // statement -- this is the same validate_cypher_identifier
            // used everywhere else, now also protecting EnsureEdge
            // callers that never validate their own rel_type.
            rel_type: "X]->() DETACH DELETE (n) //".into(),
            props: vec![],
        },
    )
    .await;

    assert!(
        result.is_err(),
        "an unsafe rel_type must be rejected, not silently reach a query"
    );
    // Confirm it was actually rejected pre-write, not partially applied.
    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'reject-src'").await,
        0
    );
}

#[tokio::test]
async fn upsert_node_rejects_unsafe_property_name_before_touching_the_graph() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    let result = handle_op(
        graph,
        &GraphOp::UpsertNode {
            key: BuildJobKey {
                build_id: "reject-prop".into(),
            }
            .into_key(),
            props: vec![Property(
                "x`}) DETACH DELETE (n) //".into(),
                GraphValue::String("whatever".into()),
            )],
        },
    )
    .await;

    assert!(
        result.is_err(),
        "an unsafe property name must be rejected, not silently reach a query"
    );
    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'reject-prop'").await,
        0
    );
}

#[tokio::test]
async fn raw_merge_write_allows_its_real_documented_scope() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;
    handle_op(
        graph,
        &GraphOp::RawMergeWrite {
            cypher: "
                MATCH (a:BuildJob { build_id: $a })
                MERGE (b:BuildJob { build_id: $b })
                MERGE (a)-[:DEPENDS_ON]->(b)
            "
            .to_string(),
            params: vec![
                ("a".to_string(), BoltType::String("raw-a".into())),
                ("b".to_string(), BoltType::String("raw-b".into())),
            ],
        },
    )
    .await
    .unwrap_or(()); // MATCH on a nonexistent "a" is a legitimate no-op, not an error

    // Create "a" first, then confirm the real MATCH+MERGE+edge shape works.
    handle_op(
        graph,
        &GraphOp::UpsertNode {
            key: BuildJobKey {
                build_id: "raw-a".into(),
            }
            .into_key(),
            props: vec![],
        },
    )
    .await
    .unwrap();
    handle_op(
        graph,
        &GraphOp::RawMergeWrite {
            cypher: "
                MATCH (a:BuildJob { build_id: $a })
                MERGE (b:BuildJob { build_id: $b })
                MERGE (a)-[:DEPENDS_ON]->(b)
            "
            .to_string(),
            params: vec![
                ("a".to_string(), BoltType::String("raw-a".into())),
                ("b".to_string(), BoltType::String("raw-b".into())),
            ],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'raw-b'").await,
        1
    );
    let mut result = graph
        .execute(neo4rs::Query::new(
            "MATCH (:BuildJob { build_id: 'raw-a' })-[r:DEPENDS_ON]->(:BuildJob { build_id: 'raw-b' }) RETURN count(r) AS c".to_string(),
        ))
        .await
        .unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>("c").unwrap(), 1);
}

#[tokio::test]
async fn raw_merge_write_rejects_disallowed_keywords_before_touching_the_graph() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;

    // DELETE, DETACH, DROP, REMOVE, CALL are all in the master
    // vocabulary but none are in RawMergeWrite's own allowlist -- this
    // is the real thing this whole redesign exists to guarantee: not
    // just "the specific dangerous words we thought of," but "anything
    // outside what this variant explicitly declares it needs."
    for dangerous in [
        "MATCH (n:BuildJob {build_id: 'raw-reject'}) DETACH DELETE n",
        "MATCH (n:BuildJob {build_id: 'raw-reject'}) REMOVE n.build_id",
        "CALL db.labels()",
        "DROP INDEX ON :BuildJob(build_id)",
    ] {
        let result = handle_op(
            graph,
            &GraphOp::RawMergeWrite {
                cypher: dangerous.to_string(),
                params: vec![],
            },
        )
        .await;
        assert!(result.is_err(), "expected rejection for: {dangerous}");
    }

    assert_eq!(
        count_nodes(graph, "BuildJob", "n.build_id = 'raw-reject'").await,
        0
    );
}

#[tokio::test]
async fn query_returns_real_rows_from_a_real_actor() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;

    handle_op(
        graph,
        &GraphOp::UpsertNode {
            key: BuildJobKey {
                build_id: "query-test-001".into(),
            }
            .into_key(),
            props: vec![Property(
                "repo_url".into(),
                GraphValue::String("https://example.com/query-test".into()),
            )],
        },
    )
    .await
    .unwrap();

    let query = neo4rs::Query::new(
        "MATCH (n:BuildJob { build_id: 'query-test-001' }) RETURN n.repo_url AS url".to_string(),
    );

    let rows = handle_query(graph, query).await.unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<String>("url").unwrap(),
        "https://example.com/query-test"
    );
}

#[tokio::test]
async fn query_returns_empty_vec_for_no_matching_rows() {
    let graph = neo4j();
    let _guard = polar_test_graph::enter();
    clear_graph(graph).await;

    let query = neo4rs::Query::new(
        "MATCH (n:BuildJob { build_id: 'definitely-does-not-exist' }) RETURN n.repo_url AS url"
            .to_string(),
    );

    let rows = handle_query(graph, query).await.unwrap();

    assert!(rows.is_empty());
}
