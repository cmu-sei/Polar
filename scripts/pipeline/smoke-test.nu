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
#   - cassini-client in PATH
#   - build processor running and subscribed to polar.provenance.events
#   - Neo4j reachable by the build processor
#
# After running, verify in Neo4j:
#   MATCH (j:BuildJob {build_id: "<uuid printed below>"}) RETURN j
#   MATCH (j:BuildJob)-[:BUILT_BY]-(c:GitCommit) RETURN j, c
#   MATCH (j:BuildJob)-[:HAS_STAGE]->(s:BuildStage) RETURN j, s
#   MATCH (j:BuildJob)-[:FOUND_VULNERABILITY]->(v:Vulnerability) RETURN j, v

use ./core/logging.nu *
use ./core/events.nu *
use ./core/cassini.nu *


const COMPONENT = "smoke-test"

def main [] {
    # ── Synthetic identity ─────────────────────────────────────────────────────
    let build_id       = (random uuid)
    let commit_sha     = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    let stage_id       = (random uuid)
    let artifact_hash  = $"sha256:(random chars --length 64)"

    # Populate env vars the emit functions read
    $env.CI_PIPELINE_ID = "smoke-pipeline-1"
    $env.CI_JOB_ID      = "smoke-job-1"
    $env.CI_JOB_URL     = "http://localhost/smoke/job/1"

    log-info $"smoke test build_id: ($build_id)" --component $COMPONENT

    # ── Start cassini daemon ───────────────────────────────────────────────────
    # let cassini_pid = (start-cassini-daemon)

    # ── ExecutionStarted ───────────────────────────────────────────────────────
    log-info "emitting ExecutionStarted" --component $COMPONENT
    emit-execution-started $build_id $commit_sha "main" "https://gitlab.example.com/polar/polar.git" --triggered_by "smoke-test"
    sleep 200ms

    # ── StageStarted ──────────────────────────────────────────────────────────
    log-info "emitting StageStarted" --component $COMPONENT
    emit-stage-started $build_id "build-agents" $stage_id
    sleep 200ms

    # ── ArtifactProduced (SBOM) ────────────────────────────────────────────────
    log-info "emitting ArtifactProduced (sbom)" --component $COMPONENT
    emit-artifact-produced $"sha256:(random chars --length 64)" "sbom" --name "polar.cdx.json" --content_type "application/vnd.cyclonedx+json"
    sleep 200ms

    # ── VulnerabilityFound ─────────────────────────────────────────────────────
    log-info "emitting VulnerabilityFound" --component $COMPONENT
    emit-vulnerability-found $build_id "high" "CVE-2024-12345" --in_artifact $artifact_hash
    sleep 200ms

    # ── StageCompleted ────────────────────────────────────────────────────────
    log-info "emitting StageCompleted" --component $COMPONENT
    emit-stage-completed $build_id "build-agents" $stage_id 42 "succeeded"
    sleep 200ms

    # ── ExecutionCompleted ────────────────────────────────────────────────────
    log-info "emitting ExecutionCompleted" --component $COMPONENT
    emit-execution-completed $build_id 120
    sleep 500ms

    # ── Stop daemon ───────────────────────────────────────────────────────────
    # stop-cassini-daemon $cassini_pid

    log-info "smoke test complete — verify in Neo4j:" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob {build_id: '($build_id)'\}\) RETURN j" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:BUILT_BY]-\(c:GitCommit\) RETURN j, c" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:HAS_STAGE]->\(s:BuildStage\) RETURN j, s" --component $COMPONENT
    log-info $"  MATCH \(j:BuildJob\)-[:FOUND_VULNERABILITY]->\(v:Vulnerability\) RETURN j, v" --component $COMPONENT
}
