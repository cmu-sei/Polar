#!/usr/bin/env nu
# smoke-test.nu — local integration test for the unified provenance event pipeline
#
# Exercises the full emission path end to end:
#   cassini daemon → emit-provenance-event → polar.provenance.events → build processor → neo4j
#
# Run from the pipeline directory:
#   nu smoke-test.nu
#
# Prerequisites:
#   - cassini-client in PATH and daemon reachable
#   - build processor running and subscribed to polar.provenance.events
#   - Neo4j reachable by the build processor
#
# After running, verify in Neo4j with the queries printed at the end.

use ./core/logging.nu *
use core/cassini.nu *
use core/events.nu *

const COMPONENT = "smoke-test"

def main [] {
    # ── Synthetic identity ─────────────────────────────────────────────────────
    let build_id          = (random uuid)
    let commit_sha        = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    let stage_id          = (random uuid)
    let sbom_hash         = $"sha256:(random chars --length 64)"
    let binary_hash       = $"sha256:(random chars --length 64)"
    let artifact_digest   = $"sha256:(random chars --length 64)"
    let config_digest     = $"sha256:(random chars --length 64)"
    let tarball_hash      = $"sha256:(random chars --length 64)"
    let layer_diff_id     = $"sha256:(random chars --length 64)"

    $env.CI_PIPELINE_ID = "smoke-pipeline-1"
    $env.CI_JOB_ID      = "smoke-job-1"
    $env.CI_JOB_URL     = "http://localhost/smoke/job/1"

    log-info $"smoke test build_id: ($build_id)" --component $COMPONENT
    log-info "cassini daemon assumed running — start manually if needed" --component $COMPONENT

    # ── ExecutionStarted ───────────────────────────────────────────────────────
    log-info "emitting ExecutionStarted" --component $COMPONENT
    emit-execution-started $build_id $commit_sha "main" "https://gitlab.example.com/polar/polar.git" --triggered_by "smoke-test"
    sleep 200ms

    # ── StageStarted ──────────────────────────────────────────────────────────
    log-info "emitting StageStarted" --component $COMPONENT
    emit-stage-started $build_id "build-agents" $stage_id
    sleep 200ms

    # ── ArtifactProduced (SBOM file) ───────────────────────────────────────────
    # Announces the raw SBOM file as a pipeline artifact, keyed on content hash.
    log-info "emitting ArtifactProduced (sbom)" --component $COMPONENT
    emit-artifact-produced $sbom_hash "sbom" --name "polar.cdx.json" --content_type "application/vnd.cyclonedx+json"
    sleep 200ms

    # ── SbomAnalyzed ──────────────────────────────────────────────────────────
    # Carries the full dependency graph fragment extracted from the SBOM.
    # The build processor writes Package nodes and DEPENDS_ON edges from this.
    log-info "emitting SbomAnalyzed" --component $COMPONENT
    emit-sbom-analyzed {
        artifact_content_hash: $sbom_hash
        root: {purl: "pkg:cargo/polar@0.1.0", name: "polar", version: "0.1.0", component_type: "application"}
        components: [
            {purl: "pkg:cargo/serde@1.0.0",  name: "serde",  version: "1.0.0", component_type: "library"}
            {purl: "pkg:cargo/tokio@1.0.0",  name: "tokio",  version: "1.0.0", component_type: "library"}
        ]
        edges: [
            {from_ref: "pkg:cargo/polar@0.1.0", to_refs: ["pkg:cargo/serde@1.0.0", "pkg:cargo/tokio@1.0.0"]}
        ]
    } "polar.cdx.json"
    sleep 200ms

    # ── ArtifactProduced (ELF binary) ──────────────────────────────────────────
    log-info "emitting ArtifactProduced (elf-binary)" --component $COMPONENT
    emit-artifact-produced $binary_hash "elf-binary" --name "polar-build-processor"
    sleep 200ms

    # ── BinaryLinked ──────────────────────────────────────────────────────────
    # Ties the compiled binary to its source package and SBOM.
    # Enables (Binary)-[:BUILT_FROM]->(Package)<-[:DESCRIBES]-(Sbom) traversal.
    log-info "emitting BinaryLinked" --component $COMPONENT
    emit-binary-linked $binary_hash "polar-build-processor" "pkg:cargo/polar@0.1.0" $sbom_hash --binding_digest $"sha256:(random chars --length 64)"
    sleep 200ms

    # ── ContainerImageCreated ─────────────────────────────────────────────────
    # Announces the OCI image before registry push.
    # The build processor creates ContainerImage and OCILayer nodes from this.
    log-info "emitting ContainerImageCreated" --component $COMPONENT
     ( emit-container-image-created
        "polar-build-processor"
        $tarball_hash
        $config_digest
        [{order: 0, diff_id: $layer_diff_id, tar_path: "layer.tar"}]
        --os "linux"
        --arch "amd64"
        --created "2026-06-07T00:00:00Z"
        --entrypoint "/bin/polar-build-processor"
     )
    sleep 200ms

    # ── ArtifactProduced (OCI image post-push digest) ─────────────────────────
    # Emitted after skopeo upload — carries the registry manifest digest.
    # Distinct from ContainerImageCreated which uses the pre-push config_digest.
    log-info "emitting ArtifactProduced (oci-image post-push)" --component $COMPONENT
    emit-artifact-produced $artifact_digest "oci-image" --name "polar-build-processor"
    sleep 200ms

    # ── VulnerabilityFound ─────────────────────────────────────────────────────
    # Links a CVE to the build and optionally to the artifact that contains it.
    log-info "emitting VulnerabilityFound" --component $COMPONENT
    emit-vulnerability-found $build_id "high" "CVE-2024-12345" --in_artifact $artifact_digest
    sleep 200ms

    # ── StageCompleted ────────────────────────────────────────────────────────
    log-info "emitting StageCompleted" --component $COMPONENT
    emit-stage-completed $build_id "build-agents" $stage_id 42 "succeeded"
    sleep 200ms

    # ── ExecutionCompleted ────────────────────────────────────────────────────
    log-info "emitting ExecutionCompleted" --component $COMPONENT
    emit-execution-completed $build_id 120
    sleep 500ms

    # ── Verify queries ────────────────────────────────────────────────────────
    log-info "" --component $COMPONENT
    log-info "smoke test complete — verify in Neo4j:" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob \{build_id: '($build_id)'\}\) RETURN j" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:BUILT_BY]-\(c:GitCommit\) RETURN j, c" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:HAS_STAGE]->\(s:BuildStage\) RETURN j, s" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:FOUND_VULNERABILITY]->\(v:Vulnerability\) RETURN j, v" --component $COMPONENT
    log-info $"  MATCH \(b:Binary\)-[:BUILT_FROM]->\(p:Package\) RETURN b, p" --component $COMPONENT
    log-info $"  MATCH \(s:Sbom\)-[:DESCRIBES]->\(p:Package\) RETURN s, p" --component $COMPONENT
    log-info $"  MATCH \(p:Package\)-[:DEPENDS_ON]->\(d:Package\) RETURN p, d" --component $COMPONENT
    log-info $"  MATCH \(i:ContainerImage\)-[:HAS_LAYER]->\(l:OCILayer\) RETURN i, l" --component $COMPONENT
    log-info $"  MATCH \(v:Vulnerability\)-[:FOUND_IN]->\(a:BuildArtifact\) RETURN v, a" --component $COMPONENT
}
