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

//! The state agent's read against a real Neo4j (a testcontainer, `polar-test-graph`): the query it builds, through a
//! graph controller stand-in that runs it on the container, the way the agent answers a request. Needs a container
//! runtime (DOCKER_HOST at podman's socket, AGENTS.md).

use polar::graph::controller::{GraphControllerMsg, handle_query};
use polar_agent_builder::PolicyLookup;
use polar_test_graph::{clear_graph, enter, neo4j};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use soft_state_common::{StateReply, StateRequest};
use soft_state_observer::floor::{Entry, Floor};
use soft_state_observer::query::{max_cypher, reply_of};
use soft_state_observer::supervisor::answer;

/// Runs each query on the test container, as the graph controller runs it on the cluster's graph.
struct ContainerGraph;

#[async_trait]
impl Actor for ContainerGraph {
    type Msg = GraphControllerMsg;
    type State = ();
    type Arguments = ();
    async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut (),
    ) -> Result<(), ActorProcessingErr> {
        if let GraphControllerMsg::Query(query, reply) = message {
            let _ = reply.send(handle_query(neo4j(), query).await);
        }
        Ok(())
    }
}

fn id(workload: &str) -> String {
    format!("{workload}.polar.serviceaccount.cluster.local")
}

fn max(label: &str, property: &str) -> StateRequest {
    StateRequest::Max {
        label: label.into(),
        property: property.into(),
    }
}

async fn run(cypher: &str) {
    neo4j()
        .run(neo4rs::Query::new(cypher.to_string()))
        .await
        .expect("the fixture's write");
}

/// One test, so the shared container's graph is not cleared under another.
#[tokio::test]
async fn the_largest_value_is_read_from_the_graph_and_only_for_an_admitted_asker() {
    let graph_db = neo4j();
    let _guard = enter();
    clear_graph(graph_db).await;

    let rows = |label: &str, property: &str| {
        let cypher = max_cypher(label, property).unwrap();
        async move {
            handle_query(neo4j(), neo4rs::Query::new(cypher))
                .await
                .unwrap()
        }
    };
    // No node: no value.
    assert_eq!(
        reply_of(&rows("JiraIssue", "updated_ms").await),
        StateReply::Value(None)
    );

    run(
        "CREATE (:JiraIssue {key: 'A-1', updated_ms: 1704164645000}),
                (:JiraIssue {key: 'A-2', updated_ms: 1704178800000}),
                (:JiraIssue {key: 'A-3'}),
                (:JiraUser {key: 'u', updated_ms: 9999999999999}),
                (:JiraIssue {key: 'A-4', Updated: '2024-01-02T03:04:05.000+0000'})",
    )
    .await;
    // The largest, over the label only, ignoring nodes without it.
    assert_eq!(
        reply_of(&rows("JiraIssue", "updated_ms").await),
        StateReply::Value(Some(1_704_178_800_000))
    );
    // A property no node has.
    assert_eq!(
        reply_of(&rows("JiraIssue", "absent").await),
        StateReply::Value(None)
    );
    // A string property is not an integer.
    assert!(matches!(
        reply_of(&rows("JiraIssue", "Updated").await),
        StateReply::Refused(_)
    ));

    // Through the agent's own answer, with the floor deciding (no policy actor runs here).
    let (graph, _) = Actor::spawn(None, ContainerGraph, ()).await.unwrap();
    let floor = Floor {
        format: 1,
        workloads: vec![Entry {
            workload: "jira-observer".into(),
            identity: id("jira-observer"),
            owns: vec![],
            reads: vec!["JiraIssue".into()],
        }],
    };
    let policy = PolicyLookup::named("no-policy-in-this-test");
    let asker = id("jira-observer");
    assert_eq!(
        answer(
            &floor,
            &policy,
            &graph,
            Some(&asker),
            &max("JiraIssue", "updated_ms")
        )
        .await,
        StateReply::Value(Some(1_704_178_800_000))
    );
    // Not declared: refused, though the graph has the nodes.
    assert!(matches!(
        answer(
            &floor,
            &policy,
            &graph,
            Some(&asker),
            &max("JiraUser", "updated_ms")
        )
        .await,
        StateReply::Refused(_)
    ));
    // A name that would change the query never reaches the graph, which keeps its nodes.
    assert!(matches!(
        answer(
            &floor,
            &policy,
            &graph,
            Some(&asker),
            &max("JiraIssue", "updated_ms`) DETACH DELETE n //")
        )
        .await,
        StateReply::Refused(_)
    ));
    assert_eq!(
        reply_of(&rows("JiraIssue", "updated_ms").await),
        StateReply::Value(Some(1_704_178_800_000))
    );
    graph.stop(None);
}
