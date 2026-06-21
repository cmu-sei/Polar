
use cassini.nu [emit, emit-provenance-event]
use state.nu [get-build-id]

export const SUBJECT_PREFIX = "polar.provenance"
export const BUILD_EVENTS_TOPIC = $"($SUBJECT_PREFIX).events"

# ---------------------------------------------------------------------------
# Canonical ProvenanceEvent emission
#
# These functions emit ProvenanceEvent variants to the unified provenance
# events topic consumed by the build processor. The payload shape must match
# the corresponding ProvenanceEvent variant in polar::events exactly —
# serde's internally-tagged enum dispatches on the `type` field.
#
# build_id is appended unconditionally by the private `emit-provenance-event` wrapper below,
# not by individual emit-* functions. Every event in this pipeline comes from
# a build, so build_id is not optional here — call sites never need to think
# about it. init-pipeline-state must be called once at the top of the
# pipeline (see state.nu) before any function in this file is used.
#
# cassini.nu's emit-provenance-event remains the lower-level transport
# primitive — it knows how to publish a record, nothing more. This module
# owns the build_id contract on top of it.
# ---------------------------------------------------------------------------

# Emit a canonical ProvenanceEvent to the unified provenance events topic.
# The payload record must include a `type` field matching a ProvenanceEvent
# variant name in snake_case, plus all fields for that variant.
# build_id is carried on the payload itself for variants that have one —
# it is not on this envelope.
def emit-provenance-event [payload: record]: nothing -> nothing {
    let json = $payload | to json --raw
    log-debug $json
    cassini-client publish $BUILD_EVENTS_TOPIC $json
}

# ── Execution lifecycle ────────────────────────────────────────────────────────
#
# Emit ExecutionStarted — first event in the build lifecycle.
# Creates the BuildJob anchor node in the graph.
# TODO: Adding this ensures we get a git commit node, but we also have the whole db present during the build.
# Perhaps we could get more data than the hash (e.g. remote refs, author, timestamp, etc. )
export def emit-execution-started [
    commit_sha: string
    ref_name: string
    repo_url: string
    --triggered_by: string = "" # TODO: Not sure we care for this field, could be useful, could just be noise
]: nothing -> nothing {
    mut payload = {
        type: "execution_started"
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

export def emit-execution-completed [duration_secs: int]: nothing -> nothing {
    emit-provenance-event {type: "execution_completed", duration_secs: $duration_secs}
}

export def emit-execution-failed [
    reason: string
    --stage: string = ""
]: nothing -> nothing {
    mut payload = {type: "execution_failed", reason: $reason, stage: null}
    if ($stage | is-not-empty) {
        $payload = ($payload | upsert stage $stage)
    }
    emit-provenance-event $payload
}

export def emit-execution-cancelled [--reason: string = ""]: nothing -> nothing {
    mut payload = {type: "execution_cancelled", reason: null}
    if ($reason | is-not-empty) {
        $payload = ($payload | upsert reason $reason)
    }
    emit-provenance-event $payload
}

export def emit-vulnerability-found [
    severity: string
    identifier: string
    --in_artifact: string = ""
]: nothing -> nothing {
    mut payload = {
        type: "vulnerability_found"
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
#
# Emit ArtifactProduced — a raw pipeline artifact was produced.
# Covers SBOMs, ELF binaries, test reports, scan results.
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
    }
    if ($name | is-not-empty)         { $payload = ($payload | insert name $name) }
    if ($content_type | is-not-empty) { $payload = ($payload | insert content_type $content_type) }
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
#
# Called twice per image across the pipeline lifecycle:
#   1. Pre-push, right after nix build — carries config_digest, layers, os/arch/etc.
#      digest and uri are absent. Creates the ContainerImage node.
#   2. Post-push, right after a successful registry upload — carries the same
#      config_digest plus the registry manifest digest and uri. The linker
#      upserts the OCIArtifact node and writes
#      ContainerImage -[:INSTANCE_OF]-> OCIArtifact, keyed on config_digest.
#      No resolver involvement — ci.nu already has every fact needed.
#
# config_digest is the stable join key across both calls.
export def emit-container-image-created [
    image_name: string
    tarball_hash: string
    config_digest: string
    layers: list<any>
    --os: string = ""
    --arch: string = ""
    --created: string = ""
    --entrypoint: string = ""
    --cmd: string = ""
    --repo_tags: list<any> = []
    --digest: string = ""   # post-push registry manifest digest
    --uri: string = ""      # post-push remote ref e.g. registry/repo@sha256:...
]: nothing -> nothing {
    mut payload = {
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
    if ($digest | is-not-empty) { $payload = ($payload | insert digest $digest) }
    if ($uri | is-not-empty)    { $payload = ($payload | insert uri $uri) }
    emit-provenance-event $payload
}

# Emit BinaryLinked — a compiled binary linked to its source package and SBOM.
#
# The binding_digest is sha256(binary:cargo_toml:cargo_lock:source_tree) —
# a cryptographic attestation of the build inputs. Recorded for audit;
# not used for graph structure.
#
# Graph edges written by the build processor:
#   (Binary)-[:BUILT_FROM]->(Package {purl: root_purl})
#   (Sbom {hash: sbom_content_hash})-[:ATTESTS]->(Binary)
export def emit-binary-linked [
    binary_content_hash: string
    binary_name: string
    root_purl: string
    sbom_content_hash: string
    --binding_digest: string = ""
]: nothing -> nothing {
    mut payload = {
        type: "binary_linked"
        binary_content_hash: $binary_content_hash
        binary_name: $binary_name
        root_purl: $root_purl
        sbom_content_hash: $sbom_content_hash
        binding_digest: null
    }
    if ($binding_digest | is-not-empty) {
        $payload = ($payload | upsert binding_digest $binding_digest)
    }
    emit-provenance-event $payload
}
