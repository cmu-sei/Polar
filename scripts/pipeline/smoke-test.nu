#!/usr/bin/env nu
# smoke-test.nu — local integration test for the canonical build event pipeline
#
# Exercises the full emission path end to end:
#   cassini daemon → emit-build-event → polar.builds.events topic → build processor → neo4j
#
# Run this from the pipeline directory:
#   nu smoke-test.nu
#
# Prerequisites:
#   - cassini-client in PATH
#   - build processor running and subscribed to polar.builds.events
#   - Neo4j reachable by the build processor
#
# After running, verify in Neo4j:
#   MATCH (j:BuildJob {build_id: "<the UUID printed below"}) RETURN j
#   MATCH (j:BuildJob)-[:BUILT_BY]-(c:GitCommit) RETURN j, c
#   MATCH (j:BuildJob)-[:HAS_STAGE]->(s:BuildStage) RETURN j, s
#   MATCH (j:BuildJob)-[:PRODUCED]->(a:BuildArtifact) RETURN j, a
#   MATCH (j:BuildJob)-[:FOUND_VULNERABILITY]->(v:Vulnerability) RETURN j, v

use ./core/logging.nu *
use ./core/cassini.nu *
use ./core/events.nu *
const COMPONENT = "smoke-test"

def main [] {

    # ── Synthetic identity ─────────────────────────────────────────────────────
    # Use a fixed UUID so you can query Neo4j for it by name after the run.
    # Change this if you want a fresh node each time.
    let build_id = (random uuid)
    let commit_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    let stage_id = (random uuid)
    let artifact_digest = $"sha256:(random chars --length 64)"

    $env.POLAR_BUILD_ID = $build_id
    $env.CI_PIPELINE_ID = "smoke-pipeline-1"
    $env.CI_JOB_ID = "smoke-job-1"
    $env.CI_JOB_URL = "http://localhost/smoke/job/1"

    log-info $"smoke test build_id: ($build_id)" --component $COMPONENT

    # ── Start cassini daemon ───────────────────────────────────────────────────
    # TODO: Use cassini-client in daemon mode when bug is resolved
    # SEE: https://github.com/cmu-sei/Polar/issues/212
    #
    # let cassini_pid = (start-cassini-daemon)
    # log-info "cassini daemon ready" --component $COMPONENT

    # ── ExecutionStarted ───────────────────────────────────────────────────────
    log-info "emitting ExecutionStarted" --component $COMPONENT
    emit-execution-started $commit_sha "main" "https://gitlab.example.com/polar/polar.git" --triggered_by "smoke-test"
    sleep 200ms

    # ── StageStarted ──────────────────────────────────────────────────────────
    log-info "emitting StageStarted" --component $COMPONENT
    emit-stage-started "build-agents" $stage_id
    sleep 200ms

    # ── ArtifactProduced (SBOM — raw file artifact) ────────────────────────────
    log-info "emitting ArtifactProduced (sbom)" --component $COMPONENT
    emit-artifact-produced $"sha256:(random chars --length 64)" "sbom" --name "polar.cdx.json" --content_type "application/vnd.cyclonedx+json"
    sleep 200ms

    # ── VulnerabilityFound ─────────────────────────────────────────────────────
    log-info "emitting VulnerabilityFound" --component $COMPONENT
    emit-vulnerability-found "high" "CVE-2024-12345" --in_artifact $artifact_digest
    sleep 200ms

    # ── StageCompleted ────────────────────────────────────────────────────────
    log-info "emitting StageCompleted" --component $COMPONENT
    emit-stage-completed "build-agents" $stage_id 42 "succeeded"
    sleep 200ms

    # ── ArtifactProduced (OCI image — post-push digest) ───────────────────────
    log-info "emitting ArtifactProduced (oci-image)" --component $COMPONENT
    emit-artifact-produced $artifact_digest "oci-image" --name "polar-cassini" --content_type "application/vnd.oci.image.manifest.v1+json"
    sleep 200ms

    # ── ExecutionCompleted ────────────────────────────────────────────────────
    log-info "emitting ExecutionCompleted" --component $COMPONENT
    emit-execution-completed 120
    sleep 500ms

    # ── Stop daemon ───────────────────────────────────────────────────────────
    # stop-cassini-daemon $cassini_pid

    log-info "smoke test complete — verify in Neo4j:" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob {build_id: '($build_id)'}\) RETURN j" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:BUILT_BY]-\(c:GitCommit\) RETURN j, c" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:HAS_STAGE]->\(s:BuildStage\) RETURN j, s" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:PRODUCED]->\(a:BuildArtifact\) RETURN j, a" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:FOUND_VULNERABILITY]->\(v:Vulnerability\) RETURN j, v" --component $COMPONENT
}
