use cassini.nu emit
use cassini.nu emit-build-event
use cassini.nu SUBJECT_PREFIX

# ---------------------------------------------------------------------------
# Canonical build event emission
#
# These functions emit canonical BuildEvent instances to the build events
# topic consumed by the build processor. The envelope shape is defined by
# orchestrator_core::events::BuildEvent — any change to that struct must
# be reflected here.
#
# All other emit-* functions in this file publish to separate agent-specific
# topics and are unrelated to this contract.
# ---------------------------------------------------------------------------

export def build-origin [native_job_id: string = ""] {
    let base = {
        system: "gitlab"
        native_id: ($env.CI_PIPELINE_ID? | default "")
        native_url: ($env.CI_JOB_URL? | default null)
    }
    if ($native_job_id | is-not-empty) {
        $base | insert native_job_id $native_job_id
    } else {
        $base | insert native_job_id ($env.CI_JOB_ID? | default null)
    }
}

export def emit-execution-started [
    commit_sha: string
    ref_name: string
    repo_url: string
    --triggered_by: string = ""
] {
    mut payload = {
        type: "execution_started"
        commit_sha: $commit_sha
        ref_name: $ref_name
        repo_url: $repo_url
        backend: null
    }
    if ($triggered_by | is-not-empty) {
        $payload = ($payload | insert triggered_by $triggered_by)
    } else {
        $payload = ($payload | insert triggered_by null)
    }
    emit-build-event $payload
}

export def emit-execution-completed [duration_secs: int] {
    emit-build-event {type: "execution_completed", duration_secs: $duration_secs}
}

export def emit-execution-failed [reason: string, --stage: string = ""] {
    mut payload = {type: "execution_failed", reason: $reason, stage: null}
    if ($stage | is-not-empty) {
        $payload = ($payload | upsert stage $stage)
    }
    emit-build-event $payload
}

export def emit-execution-cancelled [--reason: string = ""] {
    mut payload = {type: "execution_cancelled", reason: null}
    if ($reason | is-not-empty) {
        $payload = ($payload | upsert reason $reason)
    }
    emit-build-event $payload
}

export def emit-stage-started [stage_name: string, stage_id: string] {
    emit-build-event {type: "stage_started", stage_name: $stage_name, stage_id: $stage_id}
}

export def emit-stage-completed [
    stage_name: string
    stage_id: string
    duration_secs: int
    outcome: string  # "succeeded" | "failed" | "skipped" | "cancelled"
] {
    emit-build-event {
        type: "stage_completed"
        stage_name: $stage_name
        stage_id: $stage_id
        duration_secs: $duration_secs
        outcome: $outcome
    }
}

export def emit-vulnerability-found [severity: string, identifier: string, --in_artifact: string = ""] {
    mut payload = {
        type: "vulnerability_found"
        severity: $severity
        identifier: $identifier
        in_artifact: null
    }
    if ($in_artifact | is-not-empty) {
        $payload = ($payload | upsert in_artifact $in_artifact)
    }
    emit-build-event $payload
}

export def emit-artifact-produced [
    content_hash: string
    artifact_type: string
    --name: string = ""
    --content_type: string = ""
] {
    mut payload = {
        type: "artifact_produced"
        content_hash: $content_hash
        artifact_type: $artifact_type
        name: (if ($name | is-not-empty) { $name } else { null })
        content_type: (if ($content_type | is-not-empty) { $content_type } else { null })
    }
    emit-build-event $payload
}

# ---------------------------------------------------------------------------
# Emit the graph fragment as a single `sbom.analyzed` event.
#
# One SBOM → one event → one batch Cypher merge on the consumer side.
# The envelope carries pipeline/build provenance; the payload carries
# the graph projection. These are separate concerns: provenance tells
# you *when* and *why* this graph was observed, the fragment tells you
# *what* the graph looks like.
# ---------------------------------------------------------------------------
export def emit-sbom-analyzed [fragment: record, filename: string] {
    let payload = {
        filename: $filename
        artifact_content_hash: $fragment.artifact_content_hash
        root: $fragment.root
        components: $fragment.components
        edges: $fragment.edges
    }
    emit "sbom.resolved" $payload
}

export def emit-build-observed [command: string, working_dir: string, --parent_id: string = ""] {
    let base = {command: $command, working_dir: $working_dir}
    emit "build.observed" (
        if ($parent_id | is-not-empty) { $base | merge { parent_execution_id: $parent_id } } else { $base }
    )
}

export def emit-build-completed [exit_code: int, duration_ms: int] {
    emit "build.completed" {exit_code: $exit_code, duration_ms: $duration_ms}
}
