//! Execution: the `BuildJob` — what ran, where, and how it ended.
//!
//! Reported by whatever ran the job: the build orchestrator (Cyclops), or an
//! instrumented CI pipeline reporting on itself.
//!
//! ```text
//! (GitCommit)-[:BUILT_BY {at, ref_name}]->(BuildJob)     git sources only
//! (BuildJob)-[:EXECUTED_IN {at}]->(<backend job>)         e.g. KubernetesJob {uid}
//! (BuildJob)-[:IS]->(BuildExecution)
//! (BuildJob)-[:HAS_STAGE]->(BuildStage)
//! (BuildJob)-[:TRANSITIONED_TO]->(BuildJobState {phase})-[:OF_TYPE]->(State)
//! (BuildJob)-[:ANALYZED {analyzer_image, archive_sha256, at}]->(Package)
//! ```
//!
//! `completed_at` is set on success, failure *and* cancellation: it means
//! "finished". The outcome is the `phase` of the latest `BuildJobState`.
//!
//! `observed_at` records when this processor saw the event, which is useful for
//! measuring observer lag. It is distinct from when the reporting system says
//! the event happened.
//!
//! Node ownership: `BuildJob`, `BuildJobState` and `BuildStage` are owned here
//! and freely upserted. `GitCommit` and backend job nodes belong to their
//! observers and are only referenced through edges.

use super::{edge, upsert};
use neo4rs::BoltType;
use polar::graph::controller::{
    GraphOp, GraphValue, IntoGraphKey, Property,
    rel::{BUILT_BY, IS},
};
use polar::graph::nodes::builds::BuildNodeKey;
use polar::graph::nodes::git::GitNodeKey;
use polar::{BackendIdentity, PackageAnalysisStartedPayload, StageOutcome};

fn job(build_id: &str) -> BuildNodeKey {
    BuildNodeKey::BuildJob {
        build_id: build_id.to_string(),
    }
}

/// Append a `BuildJobState` instance. The state history is append-only; the
/// current state is the instance with the latest `valid_from`.
fn transition(build_id: &str, observed_at: &str, mut props: Vec<Property>) -> GraphOp {
    props.push(Property(
        "valid_from".into(),
        GraphValue::String(observed_at.to_string()),
    ));
    GraphOp::UpdateState {
        resource_key: job(build_id).into_key(),
        state_type_key: BuildNodeKey::State.into_key(),
        state_instance_key: BuildNodeKey::BuildJobState {
            build_id: build_id.to_string(),
            valid_from: observed_at.to_string(),
        }
        .into_key(),
        state_instance_props: props,
    }
}

fn string(s: impl Into<String>) -> GraphValue {
    GraphValue::String(s.into())
}

pub fn started(
    build_id: String,
    commit_sha: String,
    ref_name: String,
    repo_url: String,
    triggered_by: Option<String>,
    backend: Option<BackendIdentity>,
    observed_at: &str,
) -> Vec<GraphOp> {
    let mut ops = vec![
        // The BuildJob anchor. Later events add properties and states to it;
        // they don't re-create it.
        upsert(
            job(&build_id),
            vec![
                Property("build_id".into(), string(&build_id)),
                Property("repo_url".into(), string(repo_url)),
                Property("commit_sha".into(), string(&commit_sha)),
                Property("ref_name".into(), string(&ref_name)),
                Property(
                    "triggered_by".into(),
                    string(triggered_by.unwrap_or_else(|| "unknown".into())),
                ),
                Property("observed_at".into(), string(observed_at)),
            ],
        ),
        transition(
            &build_id,
            observed_at,
            vec![Property("phase".into(), string("started"))],
        ),
    ];

    // A build of a package archive has no commit (commit_sha is empty).
    // Merging on an empty oid would tie every such build to one bogus
    // GitCommit node, so the edge is skipped. EnsureEdge MERGEs both ends, so
    // this is safe whether or not the VCS agent has written the commit yet.
    if !commit_sha.is_empty() {
        ops.push(edge(
            GitNodeKey::Commit { oid: commit_sha },
            BUILT_BY,
            job(&build_id),
            vec![
                Property("at".into(), string(observed_at)),
                Property("ref_name".into(), string(ref_name)),
            ],
        ));
    }

    // Optional: GitLab-sourced events may not know their runner at start time.
    if let Some(backend) = backend {
        ops.push(edge(
            job(&build_id),
            "EXECUTED_IN",
            BuildNodeKey::BackendJob {
                node_label: backend.node_label,
                identity_props: backend.identity_props,
            },
            vec![Property("at".into(), string(observed_at))],
        ));
    }

    // Type hierarchy, as with (OCIArtifact)-[:IS]->(Artifact) for artifacts.
    ops.push(edge(
        job(&build_id),
        IS,
        BuildNodeKey::BuildExecution,
        vec![],
    ));
    ops
}

pub fn stage_started(
    build_id: String,
    stage_name: String,
    stage_id: String,
    observed_at: &str,
) -> Vec<GraphOp> {
    let stage = BuildNodeKey::BuildStage {
        build_id: build_id.clone(),
        stage_id: stage_id.clone(),
    };
    vec![
        upsert(
            stage.clone(),
            vec![
                Property("stage_id".into(), string(stage_id)),
                Property("stage_name".into(), string(stage_name)),
                Property("started_at".into(), string(observed_at)),
                Property("observed_at".into(), string(observed_at)),
            ],
        ),
        edge(job(&build_id), "HAS_STAGE", stage, vec![]),
    ]
}

pub fn stage_completed(
    build_id: String,
    stage_name: String,
    stage_id: String,
    duration_secs: u64,
    outcome: StageOutcome,
    observed_at: &str,
) -> Vec<GraphOp> {
    // Upsert, not create: StageStarted may arrive after this, e.g. from a
    // polling observer.
    vec![upsert(
        BuildNodeKey::BuildStage {
            build_id,
            stage_id: stage_id.clone(),
        },
        vec![
            Property("stage_id".into(), string(stage_id)),
            Property("stage_name".into(), string(stage_name)),
            Property("outcome".into(), string(format!("{outcome:?}"))),
            Property(
                "duration_secs".into(),
                GraphValue::I64(duration_secs as i64),
            ),
            Property("completed_at".into(), string(observed_at)),
            Property("observed_at".into(), string(observed_at)),
        ],
    )]
}

pub fn completed(build_id: String, duration_secs: u64, observed_at: &str) -> Vec<GraphOp> {
    vec![
        upsert(
            job(&build_id),
            vec![
                Property("completed_at".into(), string(observed_at)),
                Property(
                    "duration_secs".into(),
                    GraphValue::I64(duration_secs as i64),
                ),
                Property("observed_at".into(), string(observed_at)),
            ],
        ),
        transition(
            &build_id,
            observed_at,
            vec![
                Property("phase".into(), string("succeeded")),
                Property(
                    "duration_secs".into(),
                    GraphValue::I64(duration_secs as i64),
                ),
            ],
        ),
    ]
}

pub fn failed(
    build_id: String,
    reason: String,
    stage: Option<String>,
    observed_at: &str,
) -> Vec<GraphOp> {
    let stage = stage.unwrap_or_else(|| "unattributed".into());
    vec![
        upsert(
            job(&build_id),
            vec![
                Property("completed_at".into(), string(observed_at)),
                Property("failure_reason".into(), string(&reason)),
                Property("failure_stage".into(), string(&stage)),
                Property("observed_at".into(), string(observed_at)),
            ],
        ),
        transition(
            &build_id,
            observed_at,
            vec![
                Property("phase".into(), string("failed")),
                Property("reason".into(), string(reason)),
                Property("stage".into(), string(stage)),
            ],
        ),
    ]
}

pub fn cancelled(build_id: String, reason: Option<String>, observed_at: &str) -> Vec<GraphOp> {
    vec![
        upsert(
            job(&build_id),
            vec![
                Property("completed_at".into(), string(observed_at)),
                Property("observed_at".into(), string(observed_at)),
            ],
        ),
        transition(
            &build_id,
            observed_at,
            vec![
                Property("phase".into(), string("cancelled")),
                Property(
                    "reason".into(),
                    string(reason.unwrap_or_else(|| "no reason given".into())),
                ),
            ],
        ),
    ]
}

/// `(BuildJob)-[:ANALYZED]->(Package)`: which analyzer image ran against which
/// package archive, as the orchestrator reports it.
///
/// This is what makes the graph the record of past analysis runs: "has
/// analyzer X successfully analyzed package P" is
///
/// ```cypher
/// MATCH (p:Package { purl: $purl })<-[:ANALYZED { analyzer_image: $img }]-(j:BuildJob)
/// WHERE EXISTS { (j)-[:TRANSITIONED_TO]->(:BuildJobState { phase: 'succeeded' }) }
/// ```
///
/// and the analysis scheduler (build-orchestrator/scheduler) builds its work
/// queue on that. MERGE on the pair keeps a replay from adding a second edge.
pub fn analysis_started(p: PackageAnalysisStartedPayload, observed_at: &str) -> Vec<GraphOp> {
    let cypher = "MERGE (j:BuildJob { build_id: $build_id }) \
         MERGE (p:Package { purl: $purl }) \
         MERGE (j)-[a:ANALYZED]->(p) \
         ON CREATE SET a.at = $observed_at \
         SET a.analyzer_image = $analyzer_image, a.archive_sha256 = $archive_sha256"
        .to_string();
    let params: Vec<(String, BoltType)> = vec![
        ("build_id".into(), p.build_id.into()),
        ("purl".into(), p.purl.into()),
        ("analyzer_image".into(), p.analyzer_image.into()),
        ("archive_sha256".into(), p.archive_sha256.into()),
        ("observed_at".into(), observed_at.into()),
    ];
    vec![GraphOp::RawQuery { cypher, params }]
}
