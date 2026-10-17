//! Runs PENDING_PACKAGES_QUERY against a real Neo4j (the deployed 5.26.2).
//!
//! Requires a container runtime for testcontainers, so it is ignored by default:
//!   cargo test -p analysis-scheduler --test pending_query -- --ignored
//! With Podman: DOCKER_HOST=unix://<podman socket> TESTCONTAINERS_RYUK_DISABLED=true

use analysis_scheduler::{IN_FLIGHT_QUERY, PENDING_PACKAGES_QUERY};
use neo4rs::{Graph, query};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::neo4j::Neo4j;

const IMAGE: &str = "reg/polar-analyzer-cargo-manifest@sha256:abc";
const OTHER_IMAGE: &str = "reg/polar-analyzer-cargo-manifest@sha256:old";
const SHA: &str = "8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682";

/// One analysis run, in the shape the build processor projects:
/// `(j:BuildJob)-[:ANALYZED { analyzer_image }]->(p)`, `completed_at` and
/// `failure_reason` on the job, terminal phase as a TRANSITIONED_TO state.
async fn run(graph: &Graph, purl: &str, image: &str, build_id: &str, outcome: Option<&str>) {
    let cypher = "
        MATCH (p:Package { purl: $purl })
        CREATE (j:BuildJob { build_id: $build_id })-[:ANALYZED { analyzer_image: $image }]->(p)
        WITH j
        FOREACH (_ IN CASE WHEN $phase IS NULL THEN [] ELSE [1] END |
            SET j.completed_at = '2026-10-15T00:00:00Z'
            CREATE (j)-[:TRANSITIONED_TO]->(:BuildJobState { build_id: $build_id, phase: $phase }))
        FOREACH (_ IN CASE WHEN $phase = 'failed' THEN [1] ELSE [] END |
            SET j.failure_reason = 'boom')";
    graph
        .run(
            query(cypher)
                .param("purl", purl)
                .param("image", image)
                .param("build_id", build_id)
                .param("phase", outcome.map(str::to_string)),
        )
        .await
        .unwrap();
}

async fn package(graph: &Graph, purl: &str, sha: Option<&str>) {
    graph
        .run(
            query("CREATE (:Package { purl: $purl, archive_sha256: $sha })")
                .param("purl", purl)
                .param("sha", sha.map(str::to_string)),
        )
        .await
        .unwrap();
}

async fn pending(graph: &Graph, limit: i64) -> Vec<String> {
    let mut result = graph
        .execute(
            query(PENDING_PACKAGES_QUERY)
                .param("analyzer_image", IMAGE)
                .param("max_failed_attempts", 3_i64)
                .param("limit", limit),
        )
        .await
        .unwrap();
    let mut purls = Vec::new();
    while let Some(row) = result.next().await.unwrap() {
        purls.push(row.get::<String>("purl").unwrap());
    }
    purls
}

#[tokio::test]
#[ignore = "requires a container runtime for a Neo4j testcontainer"]
async fn pending_query_selects_exactly_the_packages_due_for_analysis() {
    let container = Neo4j::default()
        .with_version("5.26.2")
        .start()
        .await
        .unwrap();
    let config = neo4rs::ConfigBuilder::new()
        .uri(format!(
            "bolt://{}:{}",
            container.get_host().await.unwrap(),
            container.image().bolt_port_ipv4().unwrap()
        ))
        .user(container.image().user().unwrap())
        .password(container.image().password().unwrap())
        .build()
        .unwrap();
    let graph = Graph::connect(config).unwrap();

    // Due: never analyzed.
    package(&graph, "pkg:cargo/itoa@1.0.18", Some(SHA)).await;
    // Not due: succeeded with this image.
    package(&graph, "pkg:cargo/serde@1.0.229", Some(SHA)).await;
    run(
        &graph,
        "pkg:cargo/serde@1.0.229",
        IMAGE,
        "b-serde",
        Some("succeeded"),
    )
    .await;
    // Due: succeeded only with a different analyzer image.
    package(&graph, "pkg:cargo/serde_derive@1.0.229", Some(SHA)).await;
    run(
        &graph,
        "pkg:cargo/serde_derive@1.0.229",
        OTHER_IMAGE,
        "b-sd",
        Some("succeeded"),
    )
    .await;
    // Not due: in flight (no terminal event yet).
    package(&graph, "pkg:cargo/openssl-sys@0.9.117", Some(SHA)).await;
    run(
        &graph,
        "pkg:cargo/openssl-sys@0.9.117",
        IMAGE,
        "b-ossl",
        None,
    )
    .await;
    // Due: failed once, under the attempt limit.
    package(&graph, "pkg:cargo/libc@0.2.180", Some(SHA)).await;
    run(
        &graph,
        "pkg:cargo/libc@0.2.180",
        IMAGE,
        "b-libc-1",
        Some("failed"),
    )
    .await;
    // Not due: failed three times.
    package(&graph, "pkg:cargo/ring@0.17.14", Some(SHA)).await;
    for n in 1..=3 {
        run(
            &graph,
            "pkg:cargo/ring@0.17.14",
            IMAGE,
            &format!("b-ring-{n}"),
            Some("failed"),
        )
        .await;
    }
    // Due: cancelled — completed_at is set, but it did not succeed.
    package(&graph, "pkg:cargo/bytes@1.10.1", Some(SHA)).await;
    run(
        &graph,
        "pkg:cargo/bytes@1.10.1",
        IMAGE,
        "b-bytes",
        Some("cancelled"),
    )
    .await;
    // Not due: workspace (path) crate — qualified purl.
    package(
        &graph,
        "pkg:cargo/sbomprobe@0.1.0?download_url=file://.",
        Some(SHA),
    )
    .await;
    // Not due: no published-archive digest.
    package(&graph, "pkg:cargo/git-dep@0.1.0", None).await;
    // Not due: not a cargo package.
    package(&graph, "pkg:npm/left-pad@1.3.0", Some(SHA)).await;

    assert_eq!(
        pending(&graph, 100).await,
        vec![
            "pkg:cargo/bytes@1.10.1",
            "pkg:cargo/itoa@1.0.18",
            "pkg:cargo/libc@0.2.180",
            "pkg:cargo/serde_derive@1.0.229",
        ]
    );
    assert_eq!(pending(&graph, 2).await.len(), 2, "batch size is honored");

    // Only openssl-sys is unfinished with IMAGE (serde_derive's run used OTHER_IMAGE).
    let mut r = graph
        .execute(query(IN_FLIGHT_QUERY).param("analyzer_image", IMAGE))
        .await
        .unwrap();
    assert_eq!(r.next().await.unwrap().unwrap().get::<i64>("n").unwrap(), 1);
}
