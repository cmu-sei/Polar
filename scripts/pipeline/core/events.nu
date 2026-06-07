use cassini.nu [emit, emit-provenance-event]

# ---------------------------------------------------------------------------
# Canonical ProvenanceEvent emission
#
# These functions emit ProvenanceEvent variants to the unified provenance
# events topic consumed by the build processor. The payload shape must match
# the corresponding ProvenanceEvent variant in polar::events exactly —
# serde's internally-tagged enum dispatches on the `type` field.
#
# build_id is a required parameter on lifecycle events because it lives on
# the variant directly, not on an envelope. Only CI pipeline events have one.
#
# Artifact domain events (emit-sbom-analyzed, emit-artifact-produced, etc.)
# also go through emit-provenance-event — the old separate topic path via
# emit "sbom.resolved" is retired.
# ---------------------------------------------------------------------------

# ── Execution lifecycle ────────────────────────────────────────────────────────

# Emit ExecutionStarted — first event in the build lifecycle.
# Creates the BuildJob anchor node in the graph.
export def emit-execution-started [
    build_id: string
    commit_sha: string
    ref_name: string
    repo_url: string
    --triggered_by: string = ""
]: nothing -> nothing {
    mut payload = {
        type: "execution_started"
        build_id: $build_id
        commit_sha: $commit_sha
        ref_name: $ref_name
        repo_url: $repo_url
        backend: null
        triggered_by: null
    }
    if ($triggered_by | is-not-empty) {
        $payload = ($payload | upsert triggered_by $triggered_by)
    }
    emit-provenance-event $payload
}

export def emit-execution-completed [build_id: string, duration_secs: int]: nothing -> nothing {
    emit-provenance-event {
        type: "execution_completed"
        build_id: $build_id
        duration_secs: $duration_secs
    }
}

export def emit-execution-failed [
    build_id: string
    reason: string
    --stage: string = ""
]: nothing -> nothing {
    mut payload = {type: "execution_failed", build_id: $build_id, reason: $reason, stage: null}
    if ($stage | is-not-empty) {
        $payload = ($payload | upsert stage $stage)
    }
    emit-provenance-event $payload
}

export def emit-execution-cancelled [
    build_id: string
    --reason: string = ""
]: nothing -> nothing {
    mut payload = {type: "execution_cancelled", build_id: $build_id, reason: null}
    if ($reason | is-not-empty) {
        $payload = ($payload | upsert reason $reason)
    }
    emit-provenance-event $payload
}

export def emit-stage-started [
    build_id: string
    stage_name: string
    stage_id: string
]: nothing -> nothing {
    emit-provenance-event {
        type: "stage_started"
        build_id: $build_id
        stage_name: $stage_name
        stage_id: $stage_id
    }
}

export def emit-stage-completed [
    build_id: string
    stage_name: string
    stage_id: string
    duration_secs: int
    outcome: string  # "succeeded" | "failed" | "skipped" | "cancelled"
]: nothing -> nothing {
    emit-provenance-event {
        type: "stage_completed"
        build_id: $build_id
        stage_name: $stage_name
        stage_id: $stage_id
        duration_secs: $duration_secs
        outcome: $outcome
    }
}

export def emit-vulnerability-found [
    build_id: string
    severity: string
    identifier: string
    --in_artifact: string = ""
]: nothing -> nothing {
    mut payload = {
        type: "vulnerability_found"
        build_id: $build_id
        severity: $severity
        identifier: $identifier
        in_artifact: null
    }
    if ($in_artifact | is-not-empty) {
        $payload = ($payload | upsert in_artifact $in_artifact)
    }
    emit-provenance-event $payload
}

# ── Artifact domain ────────────────────────────────────────────────────────────

# Emit ArtifactProduced — a raw pipeline artifact was produced.
# Covers SBOMs, ELF binaries, test reports, scan results, OCI manifest bundles.
# Not for OCI container images — use emit-container-image-created instead.
export def emit-artifact-produced [
    content_hash: string
    artifact_type: string
    --name: string = ""
    --content_type: string = ""
]: nothing -> nothing {
    mut payload = {
        type: "artifact_produced"
        artifact_content_hash: $content_hash
        artifact_type: $artifact_type
        name: null
        content_type: null
    }
    if ($name | is-not-empty)         { $payload = ($payload | upsert name $name) }
    if ($content_type | is-not-empty) { $payload = ($payload | upsert content_type $content_type) }
    emit-provenance-event $payload
}

# Emit SbomAnalyzed — SBOM was parsed and its dependency graph extracted.
# Carries the full graph fragment so the build processor can write Package
# nodes and DEPENDS_ON edges in one pass.
export def emit-sbom-analyzed [fragment: record, filename: string]: nothing -> nothing {
    emit-provenance-event {
        type: "sbom_analyzed"
        filename: $filename
        artifact_content_hash: $fragment.artifact_content_hash
        root: $fragment.root
        components: $fragment.components
        edges: $fragment.edges
    }
}

# Emit ContainerImageCreated — OCI container image built and available as a tarball.
# Emitted before registry push. config_digest is the stable content identity.
# Non-image OCI artifacts use emit-artifact-produced instead.
export def emit-container-image-created [
    image_name: string
    tarball_hash: string
    config_digest: string
    layers: list
    --os: string = ""
    --arch: string = ""
    --created: string = ""
    --entrypoint: string = ""
    --cmd: string = ""
    --repo_tags: list = []
]: nothing -> nothing {
    emit-provenance-event {
        type: "container_image_created"
        image_name: $image_name
        tarball_hash: $tarball_hash
        config_digest: $config_digest
        layers: $layers
        os: $os
        arch: $arch
        created: $created
        entrypoint: $entrypoint
        cmd: $cmd
        repo_tags: $repo_tags
    }
}

# ── Legacy ─────────────────────────────────────────────────────────────────────
# Retained for callers not yet migrated to the canonical path.

export def emit-build-observed [command: string, working_dir: string, --parent_id: string = ""] {
    let base = {command: $command, working_dir: $working_dir}
    emit "build.observed" (
        if ($parent_id | is-not-empty) { $base | merge {parent_execution_id: $parent_id} } else { $base }
    )
}

export def emit-build-completed [exit_code: int, duration_ms: int] {
    emit "build.completed" {exit_code: $exit_code, duration_ms: $duration_ms}
}
