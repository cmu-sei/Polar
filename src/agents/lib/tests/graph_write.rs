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

/// The shared graph, emptied, held by this test alone until the guard drops.
async fn exclusive() -> (&'static Graph, tokio::sync::MutexGuard<'static, ()>) {
    let held = EXCLUSIVE.lock().await;
    let graph = neo4j();
    clear_graph(graph).await;
    (graph, held)
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
        resource_key: KubeNodeKey::Pod { uid: pod.into() }.into_key(),
        state_type_key: KubeNodeKey::State.into_key(),
        state_instance_key: KubeNodeKey::PodState {
            pod_uid: pod.into(),
            valid_from: at.to_string(),
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
            KubeNodeKey::Pod { uid: "a".into() }.into_key(),
            vec![Property("phase".into(), GraphValue::String("x".into()))],
        ),
        (
            KubeNodeKey::PodContainer {
                pod_uid: "a".into(),
                name: "c".into(),
            }
            .into_key(),
            vec![Property("image".into(), GraphValue::String("i".into()))],
        ),
        (
            KubeNodeKey::Pod { uid: "a".into() }.into_key(),
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
        (KubeNodeKey::Pod { uid: "b".into() }.into_key(), vec![]),
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
