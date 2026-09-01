//! Belongs at `consume/tests/pod_running_on_scenario.rs` -- a genuine Rust
//! integration test in the consume crate specifically, not kube_common.
//! It imports consume's own GraphOperable/ProjectionCache directly, so it
//! can't live anywhere kube_common (a dependency of consume) can see --
//! that would be circular. kube_common::testing supplies the reusable
//! harness (Scenario, bootstrap_container, bootstrap_connections,
//! fixture loading); this file is what actually uses it for one agent.
//!
//! First real Tier 1 scenario (issue #221): proves the whole chain --
//! fixture loading, cluster_uid scoping, the Node name->UID cache
//! resolution, the Sync barrier, and a real read assertion -- actually
//! works end to end, not just that each piece compiles in isolation.
//!
//! Two resources, one shared ProjectionCache, in order: Node first (so its
//! name->uid mapping is in the cache before Pod needs it), then Pod, whose
//! spec.nodeName matches the Node's name. If RUNNING_ON shows up in the
//! graph afterward, every layer between "here's a YAML fixture" and "here
//! is an edge in Neo4j" actually did its job.
//!
//! CRATE NAME PLACEHOLDER: `kube_consumer` below is a guess at the consume
//! crate's actual Cargo.toml package name -- the directory is `consume`,
//! but the declared name could be anything. Everything else in this file
//! is grounded in code already verified across this conversation; this
//! one import is the thing to fix if it doesn't resolve.

use k8s_openapi::api::core::v1::{Node, Pod};
use kube_consumer::GraphOperable;
use kube_consumer::supervisor::ProjectionCache;
use tracing::info;

use kube_common::testing::scenario::{Scenario, bootstrap_connections, bootstrap_container};

// tracing's global subscriber can only be installed once per process --
// as this file grows more scenarios, each one calling polar::init_logging
// directly and unconditionally would break every test after the first.
// Same problem tests.rs solves by nesting the call inside its OnceCell
// fixture; this mirrors that with a standalone guard since this file has
// no single shared fixture struct of its own.
static INIT_TRACING: std::sync::Once = std::sync::Once::new();

fn init_tracing() {
    INIT_TRACING.call_once(|| {
        polar::init_logging("kube_consumer.tests.pod_running_on_scenario".to_string());
    });
}

// Realistic manifests, not trimmed-down test shapes -- per fixtures.rs's
// own design, these should be indistinguishable from real kubectl output.

const NODE_FIXTURE: &str = r#"
apiVersion: v1
kind: Node
metadata:
  name: test-node-1
  uid: 22222222-2222-2222-2222-222222222222
status:
  conditions:
    - type: Ready
      status: "True"
      lastTransitionTime: "2026-08-31T00:00:00Z"
"#;

const POD_FIXTURE: &str = r#"
apiVersion: v1
kind: Pod
metadata:
  name: test-pod
  namespace: default
  uid: 11111111-1111-1111-1111-111111111111
spec:
  nodeName: test-node-1
  containers:
    - name: main
      image: example.com/app:1.0.0
status:
  phase: Running
  containerStatuses:
    - name: main
      ready: true
      imageID: example.com/app@sha256:deadbeef
      image: example.com/app:1.0.0
      state:
        running:
          startedAt: "2026-08-31T00:00:00Z"
"#;

#[tokio::test]
async fn pod_resolves_running_on_after_node_is_observed() {
    init_tracing();

    bootstrap_container()
        .await
        .expect("Neo4j test container must start");
    info!("Neo4j test container is up");

    let (graph, read_graph) = bootstrap_connections()
        .await
        .expect("GraphController and read connection must both come up");
    info!("GraphController spawned, read connection open");

    let scenario = Scenario::new(graph, read_graph);
    // Logged, not just held silently -- if this test ever fails, this is
    // the exact value to go query Neo4j with by hand, and it's not
    // derivable from anything else in the output.
    info!(cluster_uid = %scenario.cluster_uid, "scenario started");

    // One cache for the whole scenario, matching production -- the
    // ClusterConsumerSupervisor holds exactly one ProjectionCache for its
    // entire lifetime, shared across every event it ever processes. A
    // fresh cache per project_into_graph call would be wrong: it's
    // precisely the Node's entry surviving into the Pod's call that makes
    // RUNNING_ON resolvable at all.
    let mut cache = ProjectionCache::default();

    let node: Node = scenario
        .load(NODE_FIXTURE)
        .expect("Node fixture must parse");
    let node_name = node.metadata.name.clone().unwrap_or_default();
    let node_uid = node.metadata.uid.clone().unwrap_or_default();
    node.project_into_graph(
        &scenario.graph,
        &scenario.cassini,
        &scenario.cluster_uid,
        &mut cache,
    )
    .expect("Node must project cleanly");
    info!(node_name, node_uid, "Node projected");

    let pod: Pod = scenario.load(POD_FIXTURE).expect("Pod fixture must parse");
    let pod_uid = pod.metadata.uid.clone().unwrap_or_default();
    let pod_node_name = pod
        .spec
        .as_ref()
        .and_then(|s| s.node_name.clone())
        .unwrap_or_default();
    pod.project_into_graph(
        &scenario.graph,
        &scenario.cassini,
        &scenario.cluster_uid,
        &mut cache,
    )
    .expect("Pod must project cleanly");
    info!(
        pod_uid,
        scheduled_on = pod_node_name,
        "Pod projected -- RUNNING_ON should now resolve since its node was already observed"
    );

    scenario
        .sync()
        .await
        .expect("both writes must be confirmed landed before asserting");
    info!("Sync barrier returned -- both writes are confirmed durable");

    // KubeNodeKey::Node's label ("KubernetesNode") and KubeNodeKey::Pod's
    // ("Pod") -- both confirmed directly from kube.rs's cypher_match arms,
    // not guessed. Scoped by cluster_uid on both sides of the edge, same
    // as every write this scenario made.
    let cypher = "
        MATCH (p:Pod { uid: $pod_uid, cluster_uid: $cluster_uid })
              -[:RUNNING_ON]->(n:KubernetesNode { cluster_uid: $cluster_uid })
        RETURN n.uid AS node_uid
    ";

    let mut result = scenario
        .read_graph
        .execute(
            neo4rs::Query::new(cypher.to_string())
                .param("pod_uid", "11111111-1111-1111-1111-111111111111")
                .param("cluster_uid", scenario.cluster_uid.clone()),
        )
        .await
        .expect("assertion query must execute");

    let row = result
        .next()
        .await
        .expect("row fetch must succeed")
        .expect("RUNNING_ON edge must exist -- the Node name->UID resolution didn't happen");

    let node_uid: String = row
        .get("node_uid")
        .expect("query must return a node_uid column");
    info!(
        resolved_node_uid = node_uid,
        expected_node_uid = "22222222-2222-2222-2222-222222222222",
        "assertion query returned"
    );

    assert_eq!(
        node_uid, "22222222-2222-2222-2222-222222222222",
        "RUNNING_ON must point at the specific Node the Pod was scheduled onto, not just any node"
    );
}
