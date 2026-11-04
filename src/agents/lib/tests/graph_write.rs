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

//! The graph's write path against a real Neo4j (docs/architecture/graph-write-path.md). A test binary of its own, so
//! its Neo4j is its own (one container a process), and its tests take [`exclusive`] in turn: they count over the
//! whole graph, make constraints and remove nodes, which the crate's other graph tests must not see.

use neo4rs::Graph;
use polar::graph::controller::{GraphOp, GraphValue, IntoGraphKey, Property, handle_op};
use polar::graph::nodes::kube::KubeNodeKey;
use polar_test_graph::{clear_graph, neo4j};

static EXCLUSIVE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The shared graph, emptied of nodes and of every constraint and index but Neo4j's own, held by this test alone
/// until the guard drops.
async fn exclusive() -> (&'static Graph, tokio::sync::MutexGuard<'static, ()>) {
    let held = EXCLUSIVE.lock().await;
    let graph = neo4j();
    clear_graph(graph).await;
    for name in names(graph, "SHOW CONSTRAINTS YIELD name RETURN name").await {
        graph
            .run(neo4rs::Query::new(format!("DROP CONSTRAINT {name}")))
            .await
            .unwrap();
    }
    for name in names(
        graph,
        "SHOW INDEXES YIELD name, type WHERE type <> 'LOOKUP' RETURN name",
    )
    .await
    {
        graph
            .run(neo4rs::Query::new(format!("DROP INDEX {name}")))
            .await
            .unwrap();
    }
    (graph, held)
}

async fn names(graph: &Graph, cypher: &str) -> Vec<String> {
    let mut rows = graph
        .execute(neo4rs::Query::new(cypher.to_string()))
        .await
        .unwrap();
    let mut names = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        names.push(row.get::<String>("name").unwrap());
    }
    names.sort();
    names
}

async fn count(graph: &Graph, cypher: &str) -> i64 {
    let mut result = graph
        .execute(neo4rs::Query::new(cypher.to_string()))
        .await
        .unwrap();
    result
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>("c")
        .unwrap()
}

fn pod_transition(pod: &str, at: i64, phase: &str) -> GraphOp {
    GraphOp::UpdateState {
        resource_key: KubeNodeKey::Pod {
            uid: pod.into(),
            cluster_uid: "c".into(),
        }
        .into_key(),
        state_type_key: KubeNodeKey::State.into_key(),
        state_instance_key: KubeNodeKey::PodState {
            pod_uid: pod.into(),
            valid_from: at,
            cluster_uid: "c".into(),
        }
        .into_key(),
        state_instance_props: vec![
            Property("phase".into(), GraphValue::String(phase.into())),
            Property("valid_from".into(), GraphValue::I64(at)),
        ],
    }
}

/// The u7c graph held 35,185 nodes with no label and no property, each the target of an OF_TYPE from a state
/// instance: the state type's pattern `(:State)` left `stype` unbound. A transition writes no such node, and every
/// transition of every resource points at the one `:State` node.
#[tokio::test]
async fn a_state_transition_writes_no_unlabeled_node_and_reuses_the_one_state_node() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();

    for (pod, at, phase) in [
        ("pod-a", 1, "Pending"),
        ("pod-a", 2, "Running"),
        ("pod-a", 3, "Succeeded"),
        ("pod-b", 1, "Running"),
    ] {
        handle_op(graph, &pod_transition(pod, at, phase))
            .await
            .unwrap();
    }

    assert_eq!(
        count(
            graph,
            "MATCH (n) WHERE size(labels(n)) = 0 RETURN count(n) AS c"
        )
        .await,
        0,
        "no node without a label"
    );
    assert_eq!(
        count(graph, "MATCH (n:State) RETURN count(n) AS c").await,
        1
    );
    assert_eq!(
        count(
            graph,
            "MATCH (:PodState)-[:OF_TYPE]->(:State) RETURN count(*) AS c"
        )
        .await,
        4,
        "every instance is OF_TYPE the one State"
    );
    assert_eq!(
        count(graph, "MATCH ()-[r:OF_TYPE]->() RETURN count(r) AS c").await,
        4
    );
    assert_eq!(
        count(
            graph,
            "MATCH (p:Pod)-[:HAS_STATE]->(:State) RETURN count(DISTINCT p) AS c"
        )
        .await,
        2
    );
    assert_eq!(
        count(graph, "MATCH ()-[r:HAS_STATE]->() RETURN count(r) AS c").await,
        2,
        "one current-state pointer a pod"
    );
    assert_eq!(
        count(
            graph,
            "MATCH (s:PodState {pod_uid: 'pod-a'}) RETURN count(s) AS c"
        )
        .await,
        3
    );
}

fn pods(n: usize) -> GraphOp {
    GraphOp::UpsertNodes {
        items: (0..n)
            .map(|i| {
                (
                    KubeNodeKey::Pod {
                        uid: format!("pod-{i}"),
                        cluster_uid: "c".into(),
                    }
                    .into_key(),
                    vec![
                        Property("name".into(), GraphValue::String(format!("p{i}"))),
                        Property("position".into(), GraphValue::I64(i as i64)),
                    ],
                )
            })
            .collect(),
    }
}

/// Apache's 674 projects, one UpsertNodes of one MERGE each in one statement, timed out after 30 s on u7c (and in
/// write_bench on main). A thousand nodes are one UNWIND statement, written well within that, even in a debug build.
#[tokio::test]
async fn a_thousand_node_batch_is_written_well_within_the_timeout() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();

    let started = std::time::Instant::now();
    let statements = handle_op(graph, &pods(1000)).await.unwrap();
    let took = started.elapsed();

    assert_eq!(statements, 1);
    assert!(took < std::time::Duration::from_secs(10), "took {took:?}");
    assert_eq!(
        count(
            graph,
            "MATCH (n:Pod) WHERE n.name STARTS WITH 'p' RETURN count(n) AS c"
        )
        .await,
        1000
    );
    assert_eq!(
        count(
            graph,
            "MATCH (n:Pod {uid: 'pod-999'}) RETURN n.position AS c"
        )
        .await,
        999
    );
    // Again: the same thousand nodes, merged, not made again.
    handle_op(graph, &pods(1000)).await.unwrap();
    assert_eq!(
        count(graph, "MATCH (n:Pod) RETURN count(n) AS c").await,
        1000
    );
}

/// An empty batch (the groups a site read anonymously lists: none) was the empty statement "", which Neo4j refuses.
/// It sends nothing: not even to a graph that is not there.
#[tokio::test]
async fn an_empty_batch_sends_nothing() {
    let nowhere = Graph::connect(
        neo4rs::ConfigBuilder::default()
            .uri("bolt://127.0.0.1:1")
            .user("neo4j")
            .password("none")
            .build()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        handle_op(&nowhere, &GraphOp::UpsertNodes { items: vec![] })
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        handle_op(&nowhere, &GraphOp::Transaction { ops: vec![] })
            .await
            .unwrap(),
        0
    );
}

/// A mixed batch: several labels and key shapes, a node given twice (its second properties win), a node with no
/// key, a key known only at run time, and a property set to null. The graph after it is the graph the per-item
/// statements the controller used to write leave (`MERGE (n:L {k: $k}) SET n.p = $p`, written here by hand).
#[tokio::test]
async fn a_mixed_batch_writes_the_same_nodes_as_one_statement_an_item() {
    use polar::graph::controller::{DynamicNodeKey, GraphNodeKey};

    let items: Vec<(Box<dyn GraphNodeKey>, Vec<Property>)> = vec![
        (
            KubeNodeKey::Pod {
                uid: "a".into(),
                cluster_uid: "c".into(),
            }
            .into_key(),
            vec![Property("phase".into(), GraphValue::String("x".into()))],
        ),
        (
            KubeNodeKey::PodContainer {
                pod_uid: "a".into(),
                name: "c".into(),
                cluster_uid: "c".into(),
            }
            .into_key(),
            vec![Property("image".into(), GraphValue::String("i".into()))],
        ),
        (
            KubeNodeKey::Pod {
                uid: "a".into(),
                cluster_uid: "c".into(),
            }
            .into_key(),
            vec![
                Property("phase".into(), GraphValue::String("y".into())),
                Property("ready".into(), GraphValue::Bool(true)),
            ],
        ),
        (KubeNodeKey::State.into_key(), vec![]),
        (
            DynamicNodeKey::new("JiraIssue_labels", "key", "ops")
                .unwrap()
                .into_key(),
            vec![Property(
                "seen".into(),
                GraphValue::List(vec![GraphValue::I64(1), GraphValue::I64(2)]),
            )],
        ),
        (
            KubeNodeKey::Namespace {
                name: "polar".into(),
                cluster_uid: "k".into(),
            }
            .into_key(),
            vec![
                Property("gone".into(), GraphValue::Null),
                Property("n".into(), GraphValue::F64(1.5)),
            ],
        ),
        (
            KubeNodeKey::Pod {
                uid: "b".into(),
                cluster_uid: "c".into(),
            }
            .into_key(),
            vec![],
        ),
    ];

    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();

    // The old shape: one MERGE and one SET an item, run one after another.
    for (key, props) in &items {
        let (pattern, mut params) = key.cypher_match("n");
        let mut cypher = format!("MERGE {pattern}");
        if !props.is_empty() {
            let sets: Vec<String> = props
                .iter()
                .map(|Property(k, _)| format!("n.{k} = $v_{k}"))
                .collect();
            cypher.push_str(&format!(" SET {}", sets.join(", ")));
        }
        for Property(k, v) in props {
            params.push((format!("v_{k}"), v.clone().into()));
        }
        let mut q = neo4rs::Query::new(cypher);
        for (k, v) in params {
            q = q.param(&k, v);
        }
        graph.run(q).await.unwrap();
    }
    let one_by_one = polar_test_graph::dump(graph).await;
    clear_graph(graph).await;

    let statements = handle_op(graph, &GraphOp::UpsertNodes { items })
        .await
        .unwrap();
    let batched = polar_test_graph::dump(graph).await;

    assert_eq!(batched, one_by_one);
    assert_eq!(
        statements, 5,
        "one a shape: Pod, PodContainer, State, the field, Namespace"
    );
    assert_eq!(batched.len(), 6, "{batched:#?}");
}

// ── Constraints and indexes ────────────────────────────────────────────────

use polar::graph::controller::{compile_graph_op, shapes_of};
use polar::graph::schema::{Ensured, KeyIndex, KeyIndexKind, ensure_key_indexes, key_indexes};

/// What kube-consumer owns, some of it: a single key, two composite keys, and a label merged on two keys.
fn kube_indexes() -> Vec<KeyIndex> {
    key_indexes(
        &[
            "Pod",
            "PodState",
            "PodContainer",
            "FluxOCIRepository",
            "State",
        ],
        &shapes_of::<KubeNodeKey>(),
    )
}

async fn constraints_and_indexes(graph: &Graph) -> Vec<String> {
    let mut all = names(graph, "SHOW CONSTRAINTS YIELD name RETURN name").await;
    all.extend(names(graph, "SHOW INDEXES YIELD name RETURN name").await);
    all.sort();
    all
}

/// Each owned label's key gets its constraint or index, composite keys included on Neo4j 5 Community; a second start
/// makes nothing and changes nothing.
#[tokio::test]
async fn owned_labels_get_their_constraints_and_a_second_start_is_a_no_op() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    let wanted = kube_indexes();
    assert_eq!(
        wanted.iter().map(KeyIndex::name).collect::<Vec<_>>(),
        [
            "polar_index_FluxOCIRepository_name_namespace",
            "polar_index_FluxOCIRepository_uid",
            "polar_unique_Pod_uid",
            "polar_unique_PodContainer_pod_uid_name",
            "polar_unique_PodState_pod_uid_valid_from",
        ]
    );

    let first = ensure_key_indexes(graph, &wanted).await.unwrap();
    assert!(
        first.iter().all(|e| matches!(e, Ensured::Created(_))),
        "{first:#?}"
    );
    let after_first = constraints_and_indexes(graph).await;
    for index in &wanted {
        assert!(after_first.contains(&index.name()), "{after_first:?}");
    }
    assert_eq!(
        names(graph, "SHOW CONSTRAINTS YIELD name RETURN name").await,
        [
            "polar_unique_PodContainer_pod_uid_name",
            "polar_unique_PodState_pod_uid_valid_from",
            "polar_unique_Pod_uid",
        ]
    );

    let second = ensure_key_indexes(graph, &wanted).await.unwrap();
    assert!(
        second.iter().all(|e| matches!(e, Ensured::Present(_))),
        "{second:#?}"
    );
    assert_eq!(constraints_and_indexes(graph).await, after_first);
}

/// The operators Neo4j plans for the batch statement that writes `op`'s one node.
async fn plan_of(op: GraphOp, key: serde_json::Value) -> Vec<String> {
    let queries = compile_graph_op(&op).unwrap();
    assert_eq!(queries.len(), 1);
    polar_test_graph::explain(
        queries[0].query(),
        serde_json::json!({ "rows": [{ "k": key, "p": {} }] }),
    )
    .await
}

/// A state seen again (a container's status, unchanged, in the next pod event) is the same state instance: its key's
/// time is the integer its UpdateState SETs, so the MERGE finds it. With a text key it made a second node each time,
/// which the uniqueness constraint would now refuse, failing the write.
#[tokio::test]
async fn a_state_seen_again_is_the_same_instance_under_the_constraint() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    ensure_key_indexes(graph, &kube_indexes()).await.unwrap();

    for _ in 0..3 {
        handle_op(graph, &pod_transition("pod-a", 5, "Running"))
            .await
            .unwrap();
    }

    assert_eq!(
        count(
            graph,
            "MATCH (s:PodState {pod_uid: 'pod-a'}) RETURN count(s) AS c"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            graph,
            "MATCH (s:PodState {pod_uid: 'pod-a', valid_from: 5}) RETURN count(s) AS c"
        )
        .await,
        1
    );
}

/// With them, a batch's MERGE seeks its node by the constraint or the index; without them it scans the label.
#[tokio::test]
async fn merge_seeks_by_them_instead_of_scanning_the_label() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    let pod = || GraphOp::UpsertNode {
        key: KubeNodeKey::Pod {
            uid: "x".into(),
            cluster_uid: "c".into(),
        }
        .into_key(),
        props: vec![],
    };
    let pod_state = || GraphOp::UpsertNode {
        key: KubeNodeKey::PodState {
            pod_uid: "x".into(),
            valid_from: 1,
            cluster_uid: "c".into(),
        }
        .into_key(),
        props: vec![],
    };
    let flux = || GraphOp::UpsertNode {
        key: KubeNodeKey::FluxOciRepository {
            uid: "x".into(),
            cluster_uid: "c".into(),
        }
        .into_key(),
        props: vec![],
    };
    let uid = serde_json::json!({ "uid": "x" });
    let state_key = serde_json::json!({ "pod_uid": "x", "valid_from": 1 });

    let before = plan_of(pod(), uid.clone()).await;
    assert!(
        before.iter().any(|o| o.starts_with("NodeByLabelScan")),
        "{before:?}"
    );

    ensure_key_indexes(graph, &kube_indexes()).await.unwrap();
    graph
        .run(neo4rs::Query::new("CALL db.awaitIndexes(60)".to_string()))
        .await
        .unwrap();

    for (what, plan) in [
        ("Pod", plan_of(pod(), uid.clone()).await),
        ("PodState", plan_of(pod_state(), state_key).await),
        ("FluxOCIRepository", plan_of(flux(), uid).await),
    ] {
        assert!(
            !plan.iter().any(|o| o.starts_with("NodeByLabelScan")),
            "{what}: {plan:?}"
        );
        let seek = if what == "FluxOCIRepository" {
            "NodeIndexSeek"
        } else {
            "NodeUniqueIndexSeek"
        };
        assert!(plan.iter().any(|o| o.starts_with(seek)), "{what}: {plan:?}");
    }
}

/// Nodes that already share a key keep a uniqueness constraint from being made: they are counted and reported, never
/// deleted, and the range index on the same key is made instead, so MERGE still seeks.
#[tokio::test]
async fn nodes_sharing_a_key_are_reported_and_left_and_an_index_is_made_instead() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    graph
        .run(neo4rs::Query::new(
            "CREATE (:Pod {uid: 'd'}), (:Pod {uid: 'd'}), (:Pod {uid: 'd'}), (:Pod {uid: 'e'}), (:Pod {uid: 'e'}), \
             (:Pod {uid: 'f'})"
                .to_string(),
        ))
        .await
        .unwrap();
    let wanted = key_indexes(&["Pod"], &shapes_of::<KubeNodeKey>());
    assert_eq!(wanted.len(), 1);
    assert_eq!(wanted[0].kind, KeyIndexKind::Unique);

    let outcome = ensure_key_indexes(graph, &wanted).await.unwrap();

    let [
        Ensured::Refused {
            duplicates,
            instead: Some(instead),
            ..
        },
    ] = &outcome[..]
    else {
        panic!("{outcome:#?}")
    };
    assert_eq!(*duplicates, Some((2, 5)), "two keys held by five nodes");
    assert!(
        matches!(instead.as_ref(), Ensured::Created(i) if i.name() == "polar_index_Pod_uid"),
        "{instead:?}"
    );
    assert_eq!(count(graph, "MATCH (n:Pod) RETURN count(n) AS c").await, 6);
    assert!(
        names(graph, "SHOW CONSTRAINTS YIELD name RETURN name")
            .await
            .is_empty()
    );
}

/// The graph controller makes them itself, at the first contact with the graph.
#[tokio::test]
async fn the_graph_controller_makes_them_at_start() {
    use polar::graph::controller::{GraphControllerActor, GraphControllerArgs};
    use polar::graph::environment::{GraphSettings, GraphTls};

    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    let c = polar_test_graph::connection();
    let settings = GraphSettings {
        database: "neo4j".to_string(),
        user: c.user,
        password: c.password,
        endpoint: c.bolt,
        tls: GraphTls::None,
    };
    let (controller, handle) = ractor::Actor::spawn(
        None,
        GraphControllerActor,
        GraphControllerArgs::new(std::sync::Arc::new(ractor::OutputPort::default()))
            .with_settings(settings)
            .with_key_indexes(kube_indexes()),
    )
    .await
    .unwrap();

    // Made in post_start, after the canary: before the controller takes its first message.
    let mut made = Vec::new();
    for _ in 0..100 {
        made = constraints_and_indexes(graph).await;
        if kube_indexes().iter().all(|i| made.contains(&i.name())) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    controller.stop(None);
    handle.await.unwrap();
    for index in kube_indexes() {
        assert!(made.contains(&index.name()), "{made:?}");
    }
}

// ── Cleaning up what was written ───────────────────────────────────────────

use polar::graph::repair::{BATCH, EMPTY_STATE_TYPE_NODES};

/// The u7c shape, by hand: pods whose transitions point OF_TYPE and HAS_STATE at empty nodes, one a transition, as
/// the unbound `(:State)` wrote them; and beside them what must be left: an unlabeled node with a property, one with
/// an outgoing relationship, one with another incoming relationship, a labeled node with no property, and a pod
/// transitioned since the fix (its HAS_STATE to `:State`), and one pointing at both.
const U7C: &str = "
    CREATE (a:Pod {uid: 'a'}), (b:Pod {uid: 'b'}), (fixed:Pod {uid: 'fixed'}), (state:State)
    CREATE (a)-[:TRANSITIONED_TO]->(a1:PodState {pod_uid: 'a', valid_from: 1})-[:OF_TYPE]->(e1)
    CREATE (a)-[:TRANSITIONED_TO]->(a2:PodState {pod_uid: 'a', valid_from: 2})-[:OF_TYPE]->(e2)
    CREATE (a)-[:HAS_STATE]->(e2)
    CREATE (b)-[:TRANSITIONED_TO]->(b1:PodState {pod_uid: 'b', valid_from: 1})-[:OF_TYPE]->(e3)
    CREATE (b)-[:HAS_STATE]->(e3)
    CREATE (fixed)-[:TRANSITIONED_TO]->(f1:PodState {pod_uid: 'fixed', valid_from: 1})-[:OF_TYPE]->(e4)
    CREATE (fixed)-[:TRANSITIONED_TO]->(f2:PodState {pod_uid: 'fixed', valid_from: 2})-[:OF_TYPE]->(state)
    CREATE (fixed)-[:HAS_STATE]->(state)
    CREATE (both:Pod {uid: 'both'})-[:HAS_STATE]->(e5), (both)-[:HAS_STATE]->(state)
    CREATE (a1)-[:OF_TYPE]->({note: 'has a property'})
    CREATE (a1)-[:OF_TYPE]->(out)-[:POINTS_TO]->(b)
    CREATE (a1)-[:OF_TYPE]->(other)<-[:DESCRIBES]-(b)
    CREATE (a1)-[:OF_TYPE]->(:Labelled)
";

async fn empties(graph: &Graph) -> i64 {
    count(
        graph,
        "MATCH (n) WHERE size(labels(n)) = 0 AND size(keys(n)) = 0 RETURN count(n) AS c",
    )
    .await
}

/// The repair removes exactly the empty nodes the transitions made, points their OF_TYPE and HAS_STATE at the one
/// `:State` (a pod transitioned since keeps its one HAS_STATE), leaves everything else, and a second run removes
/// nothing and changes nothing.
#[tokio::test]
async fn the_empty_state_type_nodes_are_removed_and_their_edges_pointed_at_state() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    graph
        .run(neo4rs::Query::new(U7C.to_string()))
        .await
        .unwrap();
    assert_eq!(
        empties(graph).await,
        7,
        "e1..e5, and the two decoys with edges"
    );

    assert_eq!(EMPTY_STATE_TYPE_NODES.run(graph).await.unwrap(), 5);

    assert_eq!(
        empties(graph).await,
        2,
        "the decoy with an outgoing edge, and the one with another edge"
    );
    assert_eq!(
        count(graph, "MATCH (n:State) RETURN count(n) AS c").await,
        1
    );
    assert_eq!(
        count(
            graph,
            "MATCH (:PodState)-[:OF_TYPE]->(:State) RETURN count(*) AS c"
        )
        .await,
        5,
        "every instance of the shape"
    );
    for pod in ["a", "b", "fixed", "both"] {
        assert_eq!(
            count(
                graph,
                &format!("MATCH (:Pod {{uid: '{pod}'}})-[r:HAS_STATE]->() RETURN count(r) AS c")
            )
            .await,
            1,
            "{pod}"
        );
        assert_eq!(
            count(
                graph,
                &format!(
                    "MATCH (:Pod {{uid: '{pod}'}})-[:HAS_STATE]->(:State) RETURN count(*) AS c"
                )
            )
            .await,
            1,
            "{pod}"
        );
    }
    assert_eq!(
        count(
            graph,
            "MATCH (n {note: 'has a property'}) RETURN count(n) AS c"
        )
        .await,
        1
    );
    assert_eq!(
        count(graph, "MATCH (n:Labelled) RETURN count(n) AS c").await,
        1
    );
    assert_eq!(
        count(
            graph,
            "MATCH ()-[r:POINTS_TO|DESCRIBES]->() RETURN count(r) AS c"
        )
        .await,
        2
    );

    let after = polar_test_graph::dump(graph).await;
    assert_eq!(EMPTY_STATE_TYPE_NODES.run(graph).await.unwrap(), 0);
    assert_eq!(polar_test_graph::dump(graph).await, after);
}

/// More empty nodes than one batch: removed in several transactions, every one.
#[tokio::test]
async fn more_than_a_batch_of_them_are_all_removed() {
    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    let n = BATCH * 2 + 7;
    graph
        .run(
            neo4rs::Query::new(
                "MERGE (p:Pod {uid: 'many'}) WITH p UNWIND range(1, $n) AS i \
                 CREATE (p)-[:TRANSITIONED_TO]->(:PodState {pod_uid: 'many', valid_from: i})-[:OF_TYPE]->()"
                    .to_string(),
            )
            .param("n", n),
        )
        .await
        .unwrap();
    assert_eq!(empties(graph).await, n);

    assert_eq!(EMPTY_STATE_TYPE_NODES.run(graph).await.unwrap(), n);

    assert_eq!(empties(graph).await, 0);
    assert_eq!(
        count(
            graph,
            "MATCH (:PodState)-[:OF_TYPE]->(:State) RETURN count(*) AS c"
        )
        .await,
        n
    );
}

/// kube-consumer's graph controller runs it at start, after its constraints.
#[tokio::test]
async fn the_graph_controller_runs_its_repairs_at_start() {
    use polar::graph::controller::{GraphControllerActor, GraphControllerArgs};
    use polar::graph::environment::{GraphSettings, GraphTls};

    let (graph, _held) = exclusive().await;
    let _runtime = polar_test_graph::enter();
    graph
        .run(neo4rs::Query::new(U7C.to_string()))
        .await
        .unwrap();
    let c = polar_test_graph::connection();
    let (controller, handle) = ractor::Actor::spawn(
        None,
        GraphControllerActor,
        GraphControllerArgs::new(std::sync::Arc::new(ractor::OutputPort::default()))
            .with_settings(GraphSettings {
                database: "neo4j".to_string(),
                user: c.user,
                password: c.password,
                endpoint: c.bolt,
                tls: GraphTls::None,
            })
            .with_key_indexes(kube_indexes())
            .with_repairs(vec![EMPTY_STATE_TYPE_NODES]),
    )
    .await
    .unwrap();
    let mut left = empties(graph).await;
    for _ in 0..100 {
        if left == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        left = empties(graph).await;
    }
    controller.stop(None);
    handle.await.unwrap();
    assert_eq!(left, 2);
}
