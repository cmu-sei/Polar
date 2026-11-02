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

//! The graph write path measured against a real Neo4j (docs/architecture/graph-write-path.md, "Measured"): Apache's
//! issues written as jira-processor writes them, each as its own transaction, and a listing of projects as one batch.
//! Measurements, so `#[ignore]`d and run in a release build only:
//!
//! ```text
//! cargo test --release -p jira-processor write_bench -- --ignored --nocapture --test-threads 1
//! ```

use crate::issues::build_ops_for_issue;
use jira_common::types::{JiraIssue, JiraProject};
use polar::graph::controller::{GraphOp, handle_op};
use std::time::{Duration, Instant};

#[expect(
    dead_code,
    reason = "the processor reads only what the observer publishes; the rest is the observer's input"
)]
mod apache {
    include!("../../common/fixtures/apache.rs");
}

/// How many times each of Apache's issues is written: each copy is a distinct issue (its key and its history's ids
/// suffixed), sharing its users, type and project with the others, as a project's issues do.
const COPIES: usize = 100;

fn copies() -> Vec<JiraIssue> {
    let mut out = Vec::new();
    for i in 0..COPIES {
        for published in apache::PUBLISHED {
            let mut issue: JiraIssue = serde_json::from_str(published).unwrap();
            issue.key = format!("{}-{i}", issue.key);
            for history in &mut issue.changelog.histories {
                history.id = format!("{}-{i}", history.id);
            }
            out.push(issue);
        }
    }
    out
}

fn projects(n: usize) -> Vec<JiraProject> {
    (0..n)
        .map(|i| JiraProject {
            id: format!("{}", 10_000 + i),
            key: format!("P{i}"),
            name: format!("Project {i}"),
            project_type_key: "software".to_string(),
        })
        .collect()
}

async fn clear(graph: &neo4rs::Graph) {
    polar_test_graph::clear_graph(graph).await;
}

#[tokio::test]
#[ignore = "a measurement: run in a release build with --ignored --nocapture"]
async fn write_bench_issues() {
    let graph = polar_test_graph::neo4j();
    let _guard = polar_test_graph::enter();
    clear(graph).await;

    let issues = copies();
    let ops: Vec<Vec<GraphOp>> = issues.iter().map(build_ops_for_issue).collect();
    let per_issue: Vec<usize> = ops.iter().map(Vec::len).collect();
    let started = Instant::now();
    for issue_ops in ops {
        handle_op(graph, &GraphOp::Transaction { ops: issue_ops })
            .await
            .unwrap();
    }
    let took = started.elapsed();
    println!(
        "write_bench_issues: {} issues ({} ops each, {} in all) in {:.3} s: {:.2} ms an issue",
        issues.len(),
        per_issue[..apache::PUBLISHED.len()]
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join("/"),
        per_issue.iter().sum::<usize>(),
        took.as_secs_f64(),
        took.as_secs_f64() * 1000.0 / issues.len() as f64
    );
}

#[tokio::test]
#[ignore = "a measurement: run in a release build with --ignored --nocapture"]
async fn write_bench_projects() {
    let graph = polar_test_graph::neo4j();
    let _guard = polar_test_graph::enter();
    for n in [10, 100, 674, 1000] {
        clear(graph).await;
        let op = crate::projects::build_ops_for_projects(&projects(n));
        let started = Instant::now();
        let outcome = tokio::time::timeout(Duration::from_secs(120), handle_op(graph, &op)).await;
        let took = started.elapsed();
        let what = match outcome {
            Ok(Ok(())) => "written".to_string(),
            Ok(Err(e)) => format!("failed: {e}"),
            Err(_) => "not done after 120 s".to_string(),
        };
        println!(
            "write_bench_projects: {n} projects as one batch: {what} in {:.3} s",
            took.as_secs_f64()
        );
    }
}
