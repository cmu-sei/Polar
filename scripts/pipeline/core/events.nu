

use state.nu [get-build-id]
use logging.nu [log-debug]
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
# build_id is appended unconditionally by the private `emit` wrapper below,
# not by individual emit-* functions. Every event in this pipeline comes from
# a build, so build_id is not optional here — call sites never need to think
# about it. init-pipeline-state must be called once at the top of the
# pipeline (see state.nu) before any function in this file is used.
#
# cassini.nu's emit remains the lower-level transport
# primitive — it knows how to publish a record, nothing more. This module
# owns the build_id contract on top of it.
# ---------------------------------------------------------------------------

# Emit an event to the unified provenance events topic.
# The payload record must include a `type` field matching a ProvenanceEvent
# variant name in snake_case, plus all fields for that variant.
# build_id is carried on the payload itself for variants that have one —
# it is not on this envelope.
# TODO: To support more audtiable event logs, append events to a log
def emit [payload: record]: nothing -> nothing {
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

    let build_id = get-build-id

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
    emit $payload
}

export def emit-execution-completed [duration_secs: int]: nothing -> nothing {
    emit {type: "execution_completed", duration_secs: $duration_secs}
}

export def emit-execution-failed [
    reason: string
    --stage: string = ""
]: nothing -> nothing {
    mut payload = {type: "execution_failed", reason: $reason, stage: null}
    if ($stage | is-not-empty) {
        $payload = ($payload | upsert stage $stage)
    }
    emit $payload
}

export def emit-execution-cancelled [--reason: string = ""]: nothing -> nothing {
    mut payload = {type: "execution_cancelled", reason: null}
    if ($reason | is-not-empty) {
        $payload = ($payload | upsert reason $reason)
    }
    emit $payload
}

# Emit VulnerabilityFound — a SecurityAdvisory-shaped finding from a scanner:
# cargo-audit's `vulnerability`, `unsound`, and `notice` kinds. Each of these
# has a real identified issue with an advisory ID and description, as
# opposed to a package-state fact like `unmaintained` (see
# emit-package-status-found below, PackageStatus in the graph — deliberately
# not this).
#
# Graph edges written by the build processor (once implemented — see the
# Finding-model spike):
#   (SecurityAdvisory {identifier})-[:IS]->(Finding)
#   (SecurityAdvisory {identifier})-[:AFFECTS]->(Package {purl: affected_package})
#
# `kind` defaults to omitted rather than "vulnerability" here, matching every
# other optional field's omit-when-empty convention — the Rust side decides
# how to interpret an absent kind (almost certainly "vulnerability", for
# backward compatibility with anything emitted before this field existed).
export def emit-security-advisory [
    severity: string
    identifier: string
    --in_artifact: string = ""
    --scanner: string = ""
    --kind: string = ""                # vulnerability | unsound | notice
    --affected_package: string = ""    # canonical purl, sourced from the SBOM
    --cve_id: string = ""
    --ghsa_id: string = ""
    --fix_version: string = ""         # NB: often a semver constraint (">=X"), not a concrete version
    --unaffected_constraint: string = ""
    --advisory_url: string = ""
]: nothing -> nothing {
    mut payload = { type: "security_advisory_found", build_id: (get-build-id), severity: $severity, identifier: $identifier }
    for kv in [
        [in_artifact $in_artifact] [scanner $scanner] [kind $kind]
        [affected_package $affected_package] [cve_id $cve_id] [ghsa_id $ghsa_id]
        [fix_version $fix_version] [unaffected_constraint $unaffected_constraint]
        [advisory_url $advisory_url]
    ] {
        if ($kv.1 | is-not-empty) { $payload = ($payload | insert $kv.0 $kv.1) }
    }
    emit $payload
}

# Emit PackageStatusFound — a state-of-being fact about a package, not a
# discrete flaw: cargo-audit's `unmaintained` kind today. `yanked` will need
# its own call shape when it's built (no advisory id exists for it — see the
# Finding-model spike's Non-Goals), so this signature is intentionally NOT
# generalized to cover it yet.
#
# Graph edges written by the build processor (once implemented):
#   (PackageStatus {identifier})-[:IS]->(Finding)
#   (PackageStatus {identifier})-[:CONCERNS]->(Package {purl: affected_package})
# CONCERNS, not AFFECTS — "affects" implies an active exploitable flaw, which
# a maintenance-status signal is not.
export def emit-package-status-found [
    kind: string             # "unmaintained" today
    identifier: string       # RUSTSEC advisory id backing this status claim
    package_name: string
    package_version: string
    --in_artifact: string = ""
    --affected_package: string = ""    # canonical purl, sourced from the SBOM
    --advisory_url: string = ""
    --scanner: string = ""
]: nothing -> nothing {
    mut payload = {
        type: "package_status_found"
        build_id: (get-build-id)
        kind: $kind
        identifier: $identifier
        package_name: $package_name
        package_version: $package_version
    }
    for kv in [
        [in_artifact $in_artifact] [affected_package $affected_package]
        [advisory_url $advisory_url] [scanner $scanner]
    ] {
        if ($kv.1 | is-not-empty) { $payload = ($payload | insert $kv.0 $kv.1) }
    }
    emit $payload
}

# ── Artifact domain ────────────────────────────────────────────────────────────
#
# Emit ArtifactProduced — a raw pipeline artifact was produced.
# Covers SBOMs, ELF binaries, test reports, scan results.
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
        build_id: (get-build-id)
    }
    if ($name | is-not-empty)         { $payload = ($payload | insert name $name) }
    if ($content_type | is-not-empty) { $payload = ($payload | insert content_type $content_type) }
    emit $payload
}

# Emit SbomAnalyzed — SBOM was parsed and its dependency graph extracted.
# Carries the full graph fragment so the build processor can write Package
# nodes and DEPENDS_ON edges in one pass.
export def emit-sbom-analyzed [fragment: record, filename: string]: nothing -> nothing {
    emit {
        type: "sbom_analyzed"
        filename: $filename
        artifact_content_hash: $fragment.artifact_content_hash
        root: $fragment.root
        components: $fragment.components
        edges: $fragment.edges
    }
}

# Emit ContainerImageCreated — OCI container image built and available as a tarball.
# TODO: make digest and uri non-optional fields
#
# Whatever system we're choosing to observe (e.g. Kubernetes, Podman, etc.) should be able to find it later.
# So even if we don't emit on build, the resolver and build processor will get the connection later and make the
# graph consistent eventually.
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
    emit $payload
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
    emit $payload
}
