//! Projects real events against a real Neo4j (the deployed 5.26.2) through the
//! same path the agent uses: `project()` → `compile_graph_op` → execute.
//!
//! Requires a container runtime for testcontainers, so it is ignored by default:
//!   cargo test -p build-processor --test projection_neo4j -- --ignored
//! With Podman: DOCKER_HOST=unix://<podman socket> TESTCONTAINERS_RYUK_DISABLED=true

use build_processor::project::project;
use neo4rs::{Graph, Query};
use polar::graph::controller::compile_graph_op;
use polar::{
    AnalysisFindingPayload, ContainerImageCreatedPayload, DiscoverySourceRef, OciLayerEntry,
    PackageAnalysisStartedPayload, ProvenanceEvent, SecurityAdvisoryFoundPayload,
};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::neo4j::Neo4j;

/// Project and execute one event, stamped `observed_at`.
async fn apply(graph: &Graph, event: ProvenanceEvent, observed_at: &str) {
    for op in project(event, observed_at).unwrap() {
        graph.run(compile_graph_op(&op)).await.unwrap();
    }
}

async fn count(graph: &Graph, cypher: &str) -> i64 {
    let mut rows = graph.execute(Query::new(cypher.to_string())).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>("n").unwrap()
}

fn analysis_started() -> ProvenanceEvent {
    ProvenanceEvent::PackageAnalysisStarted(PackageAnalysisStartedPayload {
        build_id: "b1".into(),
        purl: "pkg:cargo/openssl-sys@0.9.117".into(),
        archive_sha256: "a".repeat(64),
        analyzer_image: "reg/analyzer@sha256:abc".into(),
    })
}

fn finding(path: Option<&str>) -> ProvenanceEvent {
    ProvenanceEvent::AnalysisFindingReported(AnalysisFindingPayload {
        build_id: "b1".into(),
        rule_id: "build-script".into(),
        message: "package declares a build script".into(),
        severity: Some("info".into()),
        path: path.map(Into::into),
        line: None,
    })
}

fn advisory() -> ProvenanceEvent {
    ProvenanceEvent::SecurityAdvisoryFound(SecurityAdvisoryFoundPayload {
        build_id: "b1".into(),
        severity: "high".into(),
        identifier: "RUSTSEC-2024-0001".into(),
        in_artifact: None,
        kind: Some("vulnerability".into()),
        scanner: Some("cargo-audit".into()),
        affected_package: Some("pkg:cargo/openssl-sys@0.9.117".into()),
        cve_id: None,
        ghsa_id: None,
        fix_version: None,
        unaffected_constraint: None,
        advisory_url: None,
    })
}

const IMAGE_DIGEST: &str = "sha256:manifest";

fn image_built() -> ProvenanceEvent {
    ProvenanceEvent::ContainerImageCreated(ContainerImageCreatedPayload {
        image_name: "polar/app".into(),
        tarball_hash: "sha256:tarball".into(),
        config_digest: "sha256:config".into(),
        layers: ["sha256:diff0", "sha256:diff1"]
            .into_iter()
            .enumerate()
            .map(|(order, diff_id)| OciLayerEntry {
                order: order as u32,
                diff_id: diff_id.into(),
                tar_path: String::new(),
            })
            .collect(),
        os: "linux".into(),
        arch: "amd64".into(),
        created: String::new(),
        entrypoint: String::new(),
        cmd: String::new(),
        repo_tags: vec![],
        digest: IMAGE_DIGEST.into(),
        uri: format!("reg.example/polar/app@{IMAGE_DIGEST}"),
    })
}

fn image_resolved() -> ProvenanceEvent {
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": "sha256:config",
            "size": 100
        },
        "layers": [
            { "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": "sha256:blob0", "size": 10 },
            { "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": "sha256:blob1", "size": 20 }
        ]
    });
    ProvenanceEvent::OCIArtifactResolved {
        uri: format!("reg.example/polar/app@{IMAGE_DIGEST}"),
        digest: IMAGE_DIGEST.into(),
        manifest_data: serde_json::to_vec(&manifest).unwrap(),
        registry: "reg.example".into(),
        source_ref: DiscoverySourceRef::OCIRepository {
            repo_uri: "reg.example/polar/app".into(),
        },
    }
}

#[tokio::test]
#[ignore = "requires a container runtime for a Neo4j testcontainer"]
async fn projections_are_idempotent_and_keep_layer_identities_apart() {
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

    // Every event twice, at different times, as a broker replay would.
    for observed_at in ["2026-10-16T00:00:00Z", "2026-10-16T01:00:00Z"] {
        apply(&graph, analysis_started(), observed_at).await;
        apply(&graph, finding(Some("build/main.rs")), observed_at).await;
        apply(&graph, finding(None), observed_at).await;
        apply(&graph, advisory(), observed_at).await;
        apply(&graph, image_built(), observed_at).await;
        apply(&graph, image_resolved(), observed_at).await;
    }

    // ── Analysis and findings: replay adds nothing ───────────────────────────
    assert_eq!(
        count(
            &graph,
            "MATCH (:BuildJob)-[a:ANALYZED]->(:Package) RETURN count(a) AS n"
        )
        .await,
        1
    );
    assert_eq!(
        count(&graph, "MATCH (f:AnalysisFinding) RETURN count(f) AS n").await,
        2
    );
    assert_eq!(
        count(
            &graph,
            "MATCH (:BuildJob)-[r:REPORTED]->(:AnalysisFinding) RETURN count(r) AS n"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &graph,
            "MATCH (:BuildJob)-[r:REPORTED]->(:SecurityAdvisory) RETURN count(r) AS n"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &graph,
            "MATCH (:SecurityAdvisory)-[r:AFFECTS]->(:Package) RETURN count(r) AS n"
        )
        .await,
        1
    );
    assert_eq!(
        count(&graph, "MATCH (t:Finding) RETURN count(t) AS n").await,
        1
    );
    assert_eq!(
        count(&graph, "MATCH (j:BuildJob) RETURN count(j) AS n").await,
        1
    );

    // ── Images: build and registry facts meet on one OCIArtifact ─────────────
    assert_eq!(
        count(&graph, "MATCH (a:OCIArtifact) RETURN count(a) AS n").await,
        1
    );
    assert_eq!(
        count(
            &graph,
            "MATCH (:OCIArtifact)-[r:HAS_DIFF_ID]->(:OCILayerDiffId) RETURN count(r) AS n"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &graph,
            "MATCH (:OCIArtifact)-[r:HAS_LAYER]->(:OCILayer) RETURN count(r) AS n"
        )
        .await,
        2
    );
    // No diff ID landed in the registry-layer space.
    assert_eq!(
        count(
            &graph,
            "MATCH (l:OCILayer) WHERE l.digest STARTS WITH 'sha256:diff' RETURN count(l) AS n"
        )
        .await,
        0
    );
    // Layer i and diff ID i are the same layer.
    assert_eq!(
        count(
            &graph,
            "MATCH (l:OCILayer)<-[hl:HAS_LAYER]-(a:OCIArtifact)-[hd:HAS_DIFF_ID]->(d:OCILayerDiffId)
             WHERE hl.order = hd.order
               AND ((l.digest = 'sha256:blob0' AND d.diff_id = 'sha256:diff0')
                 OR (l.digest = 'sha256:blob1' AND d.diff_id = 'sha256:diff1'))
             RETURN count(*) AS n"
        )
        .await,
        2
    );

    // ── An archive build has no commit to be BUILT_BY ────────────────────────
    apply(
        &graph,
        ProvenanceEvent::ExecutionStarted {
            build_id: "b1".into(),
            commit_sha: String::new(),
            ref_name: String::new(),
            repo_url: "https://static.crates.io/crates/openssl-sys/openssl-sys-0.9.117.crate"
                .into(),
            triggered_by: Some("polar.analysis.scheduler".into()),
            backend: None,
        },
        "2026-10-16T02:00:00Z",
    )
    .await;
    apply(
        &graph,
        ProvenanceEvent::ExecutionCompleted {
            build_id: "b1".into(),
            duration_secs: 3,
        },
        "2026-10-16T02:00:05Z",
    )
    .await;
    assert_eq!(
        count(&graph, "MATCH (c:GitCommit) RETURN count(c) AS n").await,
        0
    );
    // The success marker the analysis scheduler relies on.
    assert_eq!(
        count(
            &graph,
            "MATCH (:BuildJob {build_id: 'b1'})-[:TRANSITIONED_TO]->(:BuildJobState {phase: 'succeeded'}) RETURN count(*) AS n"
        )
        .await,
        1
    );
}
