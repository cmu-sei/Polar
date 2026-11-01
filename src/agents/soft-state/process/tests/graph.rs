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

//! The writer's rounds against a real Neo4j (a testcontainer, `polar-test-graph`): what a round writes, and that the
//! histories it reads back at start are the ones it wrote, bounded and in order. Needs a container runtime
//! (DOCKER_HOST at podman's socket, AGENTS.md).

use polar::graph::controller::{handle_op, handle_query};
use polar_health::{
    HealthAnswer, HealthCounters, HealthPhase, HealthReason, HealthReport, ReportStatus,
};
use polar_test_graph::{clear_graph, enter, neo4j};
use soft_state_processor::expected::{Entry, Expected};
use soft_state_processor::graph::{READ_BACK, histories_of, node_of, round_op};
use soft_state_processor::history::{History, MAX_TRANSITIONS};
use soft_state_processor::supervisor::{Round, close};
use std::collections::BTreeMap;

fn id(workload: &str) -> String {
    format!("{workload}.polar.serviceaccount.cluster.local")
}

fn report(status: ReportStatus, reason: Option<HealthReason>) -> HealthReport {
    HealthReport {
        status,
        phase: HealthPhase::Normal,
        reason,
        message: reason.map(|_| "JIRA_URL is still the example placeholder".to_string()),
        cassini_connected: true,
        graph_connected: false,
        expects_graph: false,
        counters: HealthCounters::default(),
        uptime_secs: 61,
    }
}

/// Runs one round on the container, as the writer would.
async fn write_round(
    expected: &Expected,
    histories: &mut BTreeMap<String, History>,
    number: u64,
    answers: &[(&str, HealthReport)],
    at_ms: i64,
) {
    let mut round = Round::new(number, format!("r-{number}"));
    for (workload, report) in answers {
        round.accept(
            expected,
            HealthAnswer {
                correlation_id: format!("r-{number}"),
                report: report.clone(),
            },
            Some(&id(workload)),
        );
    }
    let reachable = |w: &str| w != "drifted";
    let found = close(&round, expected, reachable, histories, at_ms);
    let items = found
        .iter()
        .map(|(entry, observation)| {
            node_of(entry, observation, &histories[&entry.workload], at_ms).unwrap()
        })
        .collect();
    handle_op(neo4j(), &round_op(items))
        .await
        .expect("the round's write");
}

async fn row(workload: &str) -> neo4rs::Row {
    let mut rows = handle_query(
        neo4j(),
        neo4rs::query(
            "MATCH (h:AgentHealth {workload: $w}) RETURN h.state AS state, h.status AS status, \
             h.reason AS reason, h.message AS message, h.answered_at_ms AS answered",
        )
        .param("w", workload),
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "one node for {workload}");
    rows.remove(0)
}

/// One test, so the shared container's graph is not cleared under another.
#[tokio::test]
async fn a_round_writes_each_workload_s_latest_report_and_its_bounded_history_survives_a_restart() {
    let graph_db = neo4j();
    let _guard = enter();
    clear_graph(graph_db).await;

    let expected = Expected {
        workloads: ["jira-observer", "kube-observer", "drifted"]
            .iter()
            .map(|w| Entry {
                workload: w.to_string(),
                identity: id(w),
            })
            .collect(),
    };
    let mut histories = BTreeMap::new();
    write_round(
        &expected,
        &mut histories,
        1,
        &[(
            "jira-observer",
            report(ReportStatus::Unhealthy, Some(HealthReason::Configuration)),
        )],
        1_000,
    )
    .await;

    let jira = row("jira-observer").await;
    assert_eq!(jira.get::<String>("state").unwrap(), "unhealthy");
    assert_eq!(jira.get::<String>("reason").unwrap(), "configuration");
    assert_eq!(
        jira.get::<String>("message").unwrap(),
        "JIRA_URL is still the example placeholder"
    );
    let kube = row("kube-observer").await;
    assert_eq!(kube.get::<String>("state").unwrap(), "unresponsive");
    assert!(kube.get::<Option<String>>("status").unwrap().is_none());
    assert_eq!(
        row("drifted").await.get::<String>("state").unwrap(),
        "unreachable"
    );

    // Thirty rounds of jira-observer flapping: the graph keeps the last twenty transitions, in order, and the latest
    // report; an unresponsive round leaves the report as the last answer said.
    for n in 0..30u64 {
        let status = if n % 2 == 0 {
            report(ReportStatus::Healthy, None)
        } else {
            report(ReportStatus::Degraded, Some(HealthReason::Graph))
        };
        write_round(
            &expected,
            &mut histories,
            2 + n,
            &[("jira-observer", status)],
            2_000 + i64::try_from(n).unwrap(),
        )
        .await;
    }
    write_round(&expected, &mut histories, 40, &[], 9_000).await;
    let jira = row("jira-observer").await;
    assert_eq!(jira.get::<String>("state").unwrap(), "unresponsive");
    assert_eq!(jira.get::<String>("status").unwrap(), "degraded");
    assert_eq!(jira.get::<i64>("answered").unwrap(), 2_029);
    assert_eq!(jira.get::<String>("reason").unwrap(), "graph");

    // What a restarted writer reads back is what it held.
    let back = histories_of(
        &handle_query(neo4j(), neo4rs::Query::new(READ_BACK.to_string()))
            .await
            .unwrap(),
    );
    assert_eq!(back, histories);
    let jira = &back["jira-observer"];
    assert_eq!(jira.len(), MAX_TRANSITIONS);
    let times: Vec<i64> = jira.transitions().map(|t| t.at_ms).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
    assert_eq!(jira.transitions().last().unwrap().state, "unresponsive");
    // Its own label only.
    let others = handle_query(
        neo4j(),
        neo4rs::Query::new("MATCH (n) WHERE NOT n:AgentHealth RETURN count(n) AS n".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(others[0].get::<i64>("n").unwrap(), 0);
}
