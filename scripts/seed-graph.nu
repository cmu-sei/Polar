#!/usr/bin/env nu

# seed-graph.nu
# Generates fixture chains in the provenance graph for CLI development.
# Requires: cypher-shell on PATH, NEO4J_URI / NEO4J_USER / NEO4J_PASS in env.

def main [
    scenario: string = "happy_path"
    # happy_path | missing_reconciliation | deployment_pending | digest_mismatch
    --base-time: string = ""
] {
    let anchor = if $base_time == "" {
        (date now) - 3hr
    } else {
        $base_time | into datetime
    }

    let commit_sha = (random chars -l 8)
    let build_id = (random uuid)
    let digest = $"sha256:(random chars -l 64)"
    let stale_digest = $"sha256:(random chars -l 64)"

    let authored_time = $anchor
    let observed_at = $anchor + 4sec
    let pipeline_start = $anchor + 3min + 25sec
    let artifact_created = $pipeline_start + 16min + 16sec
    let flux_fetch = $artifact_created + 1hr + 37min + 41sec
    let reconciled = $flux_fetch + 17min + 11sec

    print $"Seeding scenario: ($scenario)"
    print $"  commit_sha:    ($commit_sha)"
    print $"  build_id:      ($build_id)"
    print $"  digest:        ($digest)"
    print $"  authored_time: ($authored_time)"
    print $"  reconciled:    ($reconciled)"

    let cypher = (build_cypher $scenario $commit_sha $build_id $digest $stale_digest
        $authored_time $observed_at $pipeline_start $artifact_created $flux_fetch $reconciled)

    let outfile = $"./fixtures/($scenario).cypher"
    mkdir fixtures
    $cypher | save -f $outfile
    print $"Wrote ($outfile)"

    print "Applying to graph via cypher-shell..."
    open $outfile | cypher-shell -a $env.GRAPH_ENDPOINT -u $env.GRAPH_USER -p $env.GRAPH_PASSWORD
}

def build_cypher [
    scenario: string
    commit_sha: string
    build_id: string
    digest: string
    stale_digest: string
    authored_time: datetime
    observed_at: datetime
    pipeline_start: datetime
    artifact_created: datetime
    flux_fetch: datetime
    reconciled: datetime
] {
    let authored_epoch = ($authored_time | into int) / 1000000000
    let observed_iso = $observed_at | format date "%Y-%m-%dT%H:%M:%SZ"
    let pipeline_start_iso = $pipeline_start | format date "%Y-%m-%dT%H:%M:%SZ"
    let artifact_created_iso = $artifact_created | format date "%Y-%m-%dT%H:%M:%SZ"
    let flux_fetch_iso = $flux_fetch | format date "%Y-%m-%dT%H:%M:%SZ"
    let reconciled_iso = $reconciled | format date "%Y-%m-%dT%H:%M:%SZ"

    mut q = $"
// ── GitCommit ────────────────────────────────────────────────────────────
MERGE \(c:GitCommit { commit_sha: '($commit_sha)' }\)
SET c.authored_time = ($authored_epoch),
    c.observed_at = '($observed_iso)'

// ── BuildJob ─────────────────────────────────────────────────────────────
MERGE \(j:BuildJob { build_id: '($build_id)' }\)
SET j.started_at = '($pipeline_start_iso)',
    j.completed_at = '($artifact_created_iso)',
    j.repo_url = 'fixtures/sandbox-repo'

MERGE \(c\)-[:BUILT_BY]->\(j\)

// ── BuildArtifact ────────────────────────────────────────────────────────
MERGE \(a:BuildArtifact { digest: '($digest)' }\)
MERGE \(j\)-[:PRODUCED]->\(a\)

// ── OCIArtifact ──────────────────────────────────────────────────────────
MERGE \(oci:OCIArtifact { digest: '($digest)' }\)
MERGE \(a\)-[:INSTANCE_OF]->\(oci\)
"

    if $scenario == "missing_reconciliation" {

        # No Flux nodes at all. Exercises exit code 6 —
        # RECONCILED edge missing, artifact unresolved.
        return $q
    }

    $q = $q + $"
// ── Flux side ────────────────────────────────────────────────────────────
MERGE \(repo:FluxOCIRepository { name: 'sandbox-repo' }\)
MERGE \(repo\)-[:RECONCILED]->\(oci\)

MERGE \(ks:FluxKustomization { name: 'sandbox-app' }\)
MERGE \(ks\)-[:RECONCILES_FROM]->\(repo\)

MERGE \(repo_state:FluxOCIRepositoryState {
    name: 'sandbox-repo',
    valid_from: '($flux_fetch_iso)'
}\)
MERGE \(repo\)-[:TRANSITIONED_TO]->\(repo_state\)
"

    if $scenario == "deployment_pending" {

        # Flux nodes and RECONCILES_FROM exist, repo has fetched,
        # but no FluxKustomizationState yet — exercises exit code 8.
        return $q
    }

    let applied_digest = if $scenario == "digest_mismatch" { $stale_digest } else { $digest }

    $q = $q + $"
MERGE \(ks_state:FluxKustomizationState {
    name: 'sandbox-app',
    valid_from: '($reconciled_iso)'
}\)
SET ks_state.last_applied_revision = 'main@($applied_digest)'

MERGE \(ks\)-[:TRANSITIONED_TO]->\(ks_state\)
"

    $q
}
