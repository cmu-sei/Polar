//! Projection: turns one [`ProvenanceEvent`] into the graph operations that
//! record it.
//!
//! Every projection is a pure function from an event to `Vec<GraphOp>`. Nothing
//! here touches the graph or the broker. The supervisor sends the returned
//! operations, in order, to the `GraphControllerActor`, which serializes all
//! writes.
//!
//! Each operation is an idempotent MERGE, so projecting the same event twice
//! (a broker replay) leaves the graph unchanged, and events about the same
//! nodes may arrive in any order.
//!
//! The modules are cut by what the graph is *about*, which also tracks whose
//! word each fact is:
//!
//! | Module        | Subject                                         | Reported by          |
//! |---------------|-------------------------------------------------|----------------------|
//! | [`execution`] | `BuildJob`: what ran, where, and how it ended   | orchestrator, CI     |
//! | [`outputs`]   | what a build produced: artifacts, SBOMs, images | the producing build  |
//! | [`findings`]  | facts about packages: advisories, analyses      | scanners, analyzers  |
//! | [`registry`]  | what a registry actually serves                 | the resolver         |
//!
//! [`project`] is the only dispatch, and it has no wildcard arm: adding a
//! [`ProvenanceEvent`] variant fails to compile until it is routed here.

pub mod execution;
pub mod findings;
pub mod outputs;
pub mod registry;

use polar::ProvenanceEvent;
use polar::graph::controller::{GraphOp, IntoGraphKey, Property};
use ractor::ActorProcessingErr;
use tracing::{debug, warn};

/// Project one event. `observed_at` (RFC 3339) is stamped on every node and
/// edge the event writes, so all writes for one event share one timestamp.
///
/// Errors only for payloads that can't be interpreted (e.g. a manifest that
/// isn't valid OCI JSON); nothing is written for that event.
pub fn project(
    event: ProvenanceEvent,
    observed_at: &str,
) -> Result<Vec<GraphOp>, ActorProcessingErr> {
    let ops = match event {
        // ── Execution ─────────────────────────────────────────────────────────
        ProvenanceEvent::ExecutionStarted {
            build_id,
            commit_sha,
            ref_name,
            repo_url,
            triggered_by,
            backend,
        } => execution::started(
            build_id,
            commit_sha,
            ref_name,
            repo_url,
            triggered_by,
            backend,
            observed_at,
        ),
        ProvenanceEvent::StageStarted {
            build_id,
            stage_name,
            stage_id,
        } => execution::stage_started(build_id, stage_name, stage_id, observed_at),
        ProvenanceEvent::StageCompleted {
            build_id,
            stage_name,
            stage_id,
            duration_secs,
            outcome,
        } => execution::stage_completed(
            build_id,
            stage_name,
            stage_id,
            duration_secs,
            outcome,
            observed_at,
        ),
        ProvenanceEvent::ExecutionCompleted {
            build_id,
            duration_secs,
        } => execution::completed(build_id, duration_secs, observed_at),
        ProvenanceEvent::ExecutionFailed {
            build_id,
            reason,
            stage,
        } => execution::failed(build_id, reason, stage, observed_at),
        ProvenanceEvent::ExecutionCancelled { build_id, reason } => {
            execution::cancelled(build_id, reason, observed_at)
        }
        ProvenanceEvent::PackageAnalysisStarted(p) => execution::analysis_started(p, observed_at),

        // ── Build outputs ─────────────────────────────────────────────────────
        ProvenanceEvent::ArtifactProduced(p) => outputs::artifact_produced(p),
        ProvenanceEvent::SbomAnalyzed(fragment) => outputs::sbom_analyzed(fragment),
        ProvenanceEvent::BinaryLinked(p) => outputs::binary_linked(p, observed_at),
        ProvenanceEvent::ContainerImageCreated(p) => outputs::container_image_created(p),

        // ── Findings ──────────────────────────────────────────────────────────
        ProvenanceEvent::SecurityAdvisoryFound(p) => findings::security_advisory(p, observed_at),
        ProvenanceEvent::PackageStatusFound(p) => findings::package_status(p, observed_at),
        ProvenanceEvent::AnalysisFindingReported(p) => findings::analysis_finding(p, observed_at),

        // ── Registry ──────────────────────────────────────────────────────────
        ProvenanceEvent::OCIArtifactResolved {
            uri,
            digest,
            manifest_data,
            registry,
            ..
        } => registry::artifact_resolved(uri, digest, &manifest_data, registry, observed_at)?,
        ProvenanceEvent::OCIRegistryDiscovered { hostname } => {
            registry::registry_discovered(hostname)
        }

        // ── Not projected ─────────────────────────────────────────────────────
        // Discovery events are requests for the resolver to fetch and verify
        // something. The resolver reports the result (OCIArtifactResolved);
        // the unverified request itself is not recorded.
        ProvenanceEvent::OCIArtifactDiscovered { uri, .. } => {
            debug!("OCI artifact discovered: {uri} — the resolver records the resolved form");
            vec![]
        }
        ProvenanceEvent::ArtifactDiscovered { name, .. } => {
            debug!("artifact discovered: {name} — nothing to record");
            vec![]
        }
        // Defined in the vocabulary but emitted by no agent yet. Warn rather
        // than drop silently, so the first producer is noticed and gets a
        // projection instead of disappearing into a log.
        ProvenanceEvent::OCIArtifactCreated { digest, .. } => {
            warn!(%digest, "OCIArtifactCreated has no projection yet — not recorded");
            vec![]
        }
        ProvenanceEvent::ImageRefResolved { uri, .. } => {
            warn!(%uri, "ImageRefResolved has no projection yet — not recorded");
            vec![]
        }
        ProvenanceEvent::Ignored => vec![],
    };
    Ok(ops)
}

// ── Shared op builders ─────────────────────────────────────────────────────────

pub(crate) fn upsert(key: impl IntoGraphKey, props: Vec<Property>) -> GraphOp {
    GraphOp::UpsertNode {
        key: key.into_key(),
        props,
    }
}

pub(crate) fn edge(
    from: impl IntoGraphKey,
    rel_type: &str,
    to: impl IntoGraphKey,
    props: Vec<Property>,
) -> GraphOp {
    GraphOp::EnsureEdge {
        from: from.into_key(),
        rel_type: rel_type.to_string(),
        to: to.into_key(),
        props,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neo4rs::BoltType;
    use polar::{
        AnalysisFindingPayload, ContainerImageCreatedPayload, OciLayerEntry,
        PackageAnalysisStartedPayload, SecurityAdvisoryFoundPayload,
    };

    const T0: &str = "2026-10-16T00:00:00Z";

    /// The single RawQuery an event projects to.
    fn raw(ops: &[GraphOp]) -> (&str, &[(String, BoltType)]) {
        match ops {
            [GraphOp::RawQuery { cypher, params }] => (cypher, params),
            _ => panic!("expected exactly one RawQuery, got {} ops", ops.len()),
        }
    }

    fn param<'a>(params: &'a [(String, BoltType)], key: &str) -> &'a BoltType {
        &params.iter().find(|(k, _)| k == key).unwrap().1
    }

    fn finding(path: Option<&str>, line: Option<u32>) -> AnalysisFindingPayload {
        AnalysisFindingPayload {
            build_id: "b1".into(),
            rule_id: "build-script".into(),
            message: "crate has a build script".into(),
            severity: None,
            path: path.map(Into::into),
            line,
        }
    }

    #[test]
    fn analysis_started_binds_package_and_image() {
        let ops = project(
            ProvenanceEvent::PackageAnalysisStarted(PackageAnalysisStartedPayload {
                build_id: "b1".into(),
                purl: "pkg:cargo/itoa@1.0.18".into(),
                archive_sha256: "a".repeat(64),
                analyzer_image: "reg/analyzer@sha256:abc".into(),
            }),
            T0,
        )
        .unwrap();
        let (cypher, params) = raw(&ops);
        assert!(cypher.contains("(j)-[a:ANALYZED]->(p)"));
        assert_eq!(
            param(params, "purl"),
            &BoltType::from("pkg:cargo/itoa@1.0.18")
        );
        assert_eq!(
            param(params, "analyzer_image"),
            &BoltType::from("reg/analyzer@sha256:abc")
        );
    }

    #[test]
    fn finding_location_combines_path_and_line() {
        for (f, expected) in [
            (finding(Some("build.rs"), Some(3)), "build.rs:3"),
            (finding(Some("build.rs"), None), "build.rs"),
            (finding(None, None), ""),
        ] {
            let ops = project(ProvenanceEvent::AnalysisFindingReported(f), T0).unwrap();
            assert_eq!(param(raw(&ops).1, "location"), &BoltType::from(expected));
        }
    }

    #[test]
    fn finding_omits_absent_optionals_from_props() {
        let ops = project(
            ProvenanceEvent::AnalysisFindingReported(finding(None, None)),
            T0,
        )
        .unwrap();
        let BoltType::Map(props) = param(raw(&ops).1, "props") else {
            panic!("props is not a map");
        };
        assert!(props.get::<String>("severity").is_err());
        assert!(props.get::<String>("path").is_err());
    }

    /// Replays must not add edges: every timestamped edge is MERGEd on its
    /// endpoints alone, with `at` set only on creation.
    #[test]
    fn finding_edges_are_merged_without_timestamps() {
        let advisory = project(
            ProvenanceEvent::SecurityAdvisoryFound(SecurityAdvisoryFoundPayload {
                build_id: "b1".into(),
                severity: "high".into(),
                identifier: "RUSTSEC-2024-0001".into(),
                in_artifact: None,
                kind: None,
                scanner: None,
                affected_package: Some("pkg:cargo/itoa@1.0.18".into()),
                cve_id: None,
                ghsa_id: None,
                fix_version: None,
                unaffected_constraint: None,
                advisory_url: None,
            }),
            T0,
        )
        .unwrap();
        let (cypher, _) = raw(&advisory);
        assert!(
            !cypher.contains("{ at:"),
            "timestamp inside a MERGE pattern: {cypher}"
        );
        assert!(cypher.contains("ON CREATE SET r.at") && cypher.contains("ON CREATE SET af.at"));
    }

    #[test]
    fn container_image_layers_are_diff_ids_not_registry_layers() {
        let ops = project(
            ProvenanceEvent::ContainerImageCreated(ContainerImageCreatedPayload {
                image_name: "polar/app".into(),
                tarball_hash: "sha256:t".into(),
                config_digest: "sha256:c".into(),
                layers: vec![OciLayerEntry {
                    order: 0,
                    diff_id: "sha256:d0".into(),
                    tar_path: String::new(),
                }],
                os: String::new(),
                arch: String::new(),
                created: String::new(),
                entrypoint: String::new(),
                cmd: String::new(),
                repo_tags: vec![],
                digest: "sha256:m".into(),
                uri: "reg/polar/app@sha256:m".into(),
            }),
            T0,
        )
        .unwrap();
        let rendered = format!("{ops:?}");
        assert!(rendered.contains("OCILayerDiffId") && rendered.contains("HAS_DIFF_ID"));
        assert!(
            !rendered.contains("OCILayer {"),
            "pipeline wrote a registry layer node"
        );
    }

    #[test]
    fn container_image_without_digest_writes_nothing() {
        let ops = project(
            ProvenanceEvent::ContainerImageCreated(ContainerImageCreatedPayload {
                image_name: "polar/app".into(),
                tarball_hash: "sha256:t".into(),
                config_digest: "sha256:c".into(),
                layers: vec![],
                os: String::new(),
                arch: String::new(),
                created: String::new(),
                entrypoint: String::new(),
                cmd: String::new(),
                repo_tags: vec![],
                digest: String::new(),
                uri: String::new(),
            }),
            T0,
        )
        .unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn invalid_manifest_is_an_error_not_a_partial_write() {
        let result = project(
            ProvenanceEvent::OCIArtifactResolved {
                uri: "reg/x@sha256:m".into(),
                digest: "sha256:m".into(),
                manifest_data: b"not json".to_vec(),
                registry: "reg".into(),
                source_ref: polar::DiscoverySourceRef::OCIRepository {
                    repo_uri: "reg/x".into(),
                },
            },
            T0,
        );
        assert!(result.is_err());
    }

    #[test]
    fn archive_build_has_no_commit_edge() {
        let ops = project(
            ProvenanceEvent::ExecutionStarted {
                build_id: "b1".into(),
                commit_sha: String::new(),
                ref_name: String::new(),
                repo_url: "https://static.crates.io/crates/itoa/itoa-1.0.18.crate".into(),
                triggered_by: None,
                backend: None,
            },
            T0,
        )
        .unwrap();
        assert!(!format!("{ops:?}").contains("BUILT_BY"));
    }

    /// `emit-sbom-analyzed` output (scripts/pipeline/core/events.nu) for a
    /// cargo-cyclonedx SBOM of a crate depending on itoa 1.0.18, captured from
    /// a run of the pipeline library. The digest is itoa's Cargo.lock
    /// checksum; the path-sourced root crate has none.
    #[test]
    fn sbom_analyzed_event_carries_archive_sha256() {
        let json = r#"{"type":"sbom_analyzed","filename":"sbomprobe.cdx.json","artifact_content_hash":"sha256:test","root":{"purl":"pkg:cargo/sbomprobe@0.1.0?download_url=file://.","name":"sbomprobe","version":"0.1.0","component_type":"application","license":null,"archive_sha256":null,"source":"registry"},"components":[{"purl":"pkg:cargo/itoa@1.0.18","name":"itoa","version":"1.0.18","component_type":"library","license":"MIT OR Apache-2.0","archive_sha256":"8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682","source":"registry"}],"edges":[{"from_ref":"path+file:///private/tmp/claude-502/-Users-vacoates-projects-Polar/b22f1fe6-065b-4e79-87f4-8023440c2bf5/scratchpad/sbomprobe#0.1.0","to_refs":["registry+https://github.com/rust-lang/crates.io-index#itoa@1.0.18"]}]}"#;
        let ProvenanceEvent::SbomAnalyzed(fragment) = serde_json::from_str(json).unwrap() else {
            panic!("not an SbomAnalyzed event");
        };
        assert_eq!(
            fragment.components[0].archive_sha256.as_deref(),
            Some("8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682")
        );
        assert_eq!(fragment.root.unwrap().archive_sha256, None);
    }

    /// Exact payloads produced by `emit-analysis-finding` in
    /// scripts/pipeline/core/events.nu (captured from a run of that function).
    #[test]
    fn nushell_finding_payloads_deserialize() {
        for json in [
            r#"{"type":"analysis_finding_reported","build_id":"b1","rule_id":"build-script","message":"crate has a build script","path":"build.rs","line":1}"#,
            r#"{"type":"analysis_finding_reported","build_id":"b1","rule_id":"proc-macro","message":"crate is a proc-macro","severity":"info"}"#,
        ] {
            let event: ProvenanceEvent = serde_json::from_str(json).unwrap();
            assert!(matches!(event, ProvenanceEvent::AnalysisFindingReported(_)));
        }
    }
}
