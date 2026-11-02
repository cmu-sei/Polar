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
