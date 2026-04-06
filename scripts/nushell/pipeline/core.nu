
const CASSINI_SOCK_ENV = "CASSINI_DAEMON_SOCK"
const CASSINI_SESSION_ENV = "POLAR_CASSINI_SESSION_ID"

export const MANIFEST_PATH = "/etc/pipeline/pipeline.json"
export const SUBJECT_PREFIX = "polar.builds"
export const ELF_BINARY_ARTIFACT = "elf-binary-set"


# ---------------------------------------------------------------------------
# Build the graph fragment from a parsed CycloneDX document.
#
# This is a pure data transform: CycloneDX JSON → graph projection.
# No side effects, no emissions. Separating this from emit makes it
# testable in isolation.
# ---------------------------------------------------------------------------
export def extract-graph-fragment [doc: record, artifact_content_hash: string]: nothing -> record {
    let root_component = ($doc.metadata?.component? | default null)

    # The root package identity. This is the crate/binary the SBOM describes.
    # We key on purl because it's the only stable cross-build identifier.
    # Content hashes change when the SBOM is regenerated; purls don't.
    let root = if $root_component != null {
        {
            purl: ($root_component.purl? | default "")
            name: ($root_component.name? | default "")
            version: ($root_component.version? | default "")
            component_type: ($root_component.type? | default "application")
        }
    } else {
        null
    }

    # Flat node list. We only extract identity fields — no license text,
    # no external references, no tool metadata. Those belong in the SBOM
    # document (which you can always re-fetch from object storage by
    # content hash), not in the graph merge payload.
    let components = ($doc.components? | default [] | each {|c|
        {
            purl: ($c.purl? | default "")
            name: ($c.name? | default "")
            version: ($c.version? | default "")
            component_type: ($c.type? | default "library")
        }
    } | where { ($in.purl | is-not-empty) or ($in.name | is-not-empty) })

    # Edge list from the `dependencies` key in the CycloneDX spec.
    # This is where the actual tree lives. `components` is just an
    # inventory; `dependencies` encodes "A depends on [B, C, D]".
    #
    # If this key is missing (some generators omit it), we degrade
    # gracefully — the agent gets nodes but no edges. That's still
    # useful for "what packages exist in this build" queries, just
    # not for "what depends on what" traversals.
    let edges = ($doc.dependencies? | default [] | each {|dep|

        let edge = {
            from_ref: ($dep.ref? | default "")
            to_refs: ($dep.dependsOn? | default [])
        }

    } | where { ($in.from_ref | is-not-empty) and ($in.to_refs | length) > 0 })

    {
        artifact_content_hash: $artifact_content_hash
        root: $root
        components: $components
        edges: $edges
    }
}

export def workspace-root []: nothing -> string {
    cargo metadata --format-version 1 --no-deps
    | from json
    | get workspace_root
}

export def process-sboms [files: list<record>, packages: list<record>, artifact_dir: path = "pipeline-out", --component: string = ""]: nothing -> list<record> {
    # Now read whatever landed in artifact_dir.
    let sbom_files = (
        ls $artifact_dir
        | where type == file
        | where { ($in.name | path basename | str ends-with ".cdx.json") }
    )

    $sbom_files | each {|f|
            let doc = try { open $f.name } catch {|e|
                log-warn $"could not parse ($f.name): ($e.msg)" --component $component
                null
            }
            if $doc == null { return null }
            if ($doc.bomFormat? | default "") != "CycloneDX" { return null }

            let filename = ($f.name | path basename)
            let stem = ($filename | str replace ".cdx.json" "")
            let content_hash = (content-hash-file $f.name)

            # Provenance: record that this build stage produced an SBOM artifact.
            emit-artifact-produced $content_hash "sbom" --name $filename --content_type "application/vnd.cyclonedx+json"

            # Graph projection: extract nodes + edges, emit as a single event.
            let fragment = (extract-graph-fragment $doc $content_hash)

            if $fragment.root != null {
                emit-sbom-analyzed $fragment $filename
            } else {
                log-warn $"($filename) has no metadata.component — emitting artifact.produced only, no graph fragment" --component $component
            }

            # Return summary for upstream pipeline orchestration.
            let matched = ($packages | where name == $stem | first)
            {
                name: ($matched.name? | default $stem)
                path: $f.name
                artifact_content_hash: $content_hash
                component_count: ($fragment.components | length)
                edge_count: ($fragment.edges | length)
            }
        }
        | where { $in != null }
}

# ---------------------------------------------------------------------------
# Timing
# ---------------------------------------------------------------------------

def elapsed-ms [start: datetime]: nothing -> int {
    (((date now) - $start) / 1_000_000) | into int
}

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

export def log [level: string, msg: string, --component: string = ""] {
    let ts = (date now | format date "%Y-%m-%dT%H:%M:%S%.3fZ")
    print $"($ts) [($level)] ($component) — ($msg)"
}

export def log-info  [msg: string, --component: string = ""] { log "INFO"  $msg --component $component }
export def log-warn  [msg: string, --component: string = ""] { log "WARN"  $msg --component $component }
export def log-error [msg: string, --component: string = ""] { log "ERROR" $msg --component $component }
export def log-debug [msg: string, --component: string = ""] { log "DEBUG" $msg --component $component }

# ---------------------------------------------------------------------------
# Cassini
# ---------------------------------------------------------------------------

# Start the cassini-client daemon as a background job and wait for it
# # to be ready. Sets CASSINI_DAEMON_SOCK in the environment so all
# # subsequent cassini-client invocations find the socket automatically.
# # Returns the job id so the caller can stop it cleanly on exit.
export def start-cassini-daemon [
    --socket: string = "/tmp/cassini-pipeline.sock"
    --timeout: int = 30
]: nothing -> int {
    # If a daemon is already reachable at the socket, don't start another.
    let status = (try { cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
    if $status.exit_code == 0 {
        log-info "cassini daemon already running, reusing" --component "cassini"
        $env.CASSINI_DAEMON_SOCK = $socket
        return 1  # sentinel: we didn't start it, don't stop it
    }

    log-info $"starting cassini daemon at ($socket)" --component "cassini"

    let job_id = (job spawn {
        cassini-client --daemon --foreground --socket $socket
    })

    # Poll until the socket is accepting commands or we time out.
    let ready = (
        0..($timeout * 2)  # 500ms steps
        | each {|_|
            let probe = (try { cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
            if $probe.exit_code == 0 { true } else { sleep 500ms; false }
        }
        | any { $in == true }
    )

    if not $ready {
        log-error $"cassini daemon did not become ready within ($timeout)s" --component "cassini"
        job kill $job_id
        error make { msg: "cassini daemon startup timeout" }
    }

    $env.CASSINI_DAEMON_SOCK = $socket
    log-info "cassini daemon ready" --component "cassini"
    $job_id
}

# Stop the daemon job started by start-cassini-daemon.
# Pass -1 if you didn't start it (reused an existing daemon) and this is a no-op.
export def stop-cassini-daemon [job_id: int] {
    if $job_id == -1 { return }
    log-info "stopping cassini daemon" --component "cassini"
    try { cassini-client status | complete } catch {}  # flush in-flight
    job kill $job_id
}

# ---------------------------------------------------------------------------
# Cassini provenance emission
# ---------------------------------------------------------------------------

export def emit [subject_suffix: string, payload: record] {
    let envelope = {
        build_id: ($env.POLAR_BUILD_ID? | default "00000000-0000-0000-0000-000000000000")
        stage_exec_id: ($env.POLAR_STAGE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        pipeline_exec_id: ($env.POLAR_PIPELINE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        observed_at: (date now | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        payload: ($payload | merge { type: $subject_suffix })
    }
    # cassini-client publish goes here
    let payload = ($envelope | to json --raw)

    $payload | save --force last_emitted.json

    cassini-client publish $"($SUBJECT_PREFIX).($subject_suffix)" $payload
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
    let base = { command: $command, working_dir: $working_dir }
    emit "build.observed" (
        if ($parent_id | is-not-empty) { $base | merge { parent_execution_id: $parent_id } } else { $base }
    )
}

export def emit-build-completed [exit_code: int, duration_ms: int] {
    emit "build.completed" { exit_code: $exit_code, duration_ms: $duration_ms }
}

# ---------------------------------------------------------------------------
# Lifecycle event: an artifact was produced by this build stage.
# This is purely about provenance — "stage X produced file Y at time T."
# It does NOT carry dependency semantics. The graph fragment does that.
#
# We still emit this so the knowledge graph can link:
#   (BuildStage)-[:PRODUCED]->(Artifact {content_hash})
# which is a different concern from the dependency subgraph.
# ---------------------------------------------------------------------------
export def emit-artifact-produced [content_hash: string, artifact_type: string, --name: string = "", --content_type: string = ""] {
    mut payload = { artifact_content_hash: $content_hash, artifact_type: $artifact_type }
    if ($name | is-not-empty)         { $payload = ($payload | insert name $name) }
    if ($content_type | is-not-empty) { $payload = ($payload | insert content_type $content_type) }
    emit "artifact.produced" $payload
}

# ---------------------------------------------------------------------------
# Content hashing
# ---------------------------------------------------------------------------

export def content-hash-file [path: string]: nothing -> string {
    open $path --raw | hash sha256 | $"sha256:($in)"
}

export def content-hash-dir [dir: string]: nothing -> string {
    let git_check = try { git -C $dir rev-parse --git-dir | complete } catch { { exit_code: 1 } }
    if $git_check.exit_code == 0 {
        let h = (git -C $dir rev-parse "HEAD^{tree}" | str trim)
        $"sha256:($h)"
    } else {
        glob $"($dir)/**/*"
        | where { ($in | path type) == "file" }
        | sort
        | each { open $in --raw | hash sha256 }
        | str join "\n"
        | hash sha256
        | $"sha256:($in)"
    }
}

export def content-hash [path: string]: nothing -> string {
    if ($path | path type) == "dir" {
        content-hash-dir $path
    } else {
        content-hash-file $path
    }
}

export def git-commit-sha [dir: string]: nothing -> record {
    let result = try { git -C $dir rev-parse HEAD | complete } catch { { exit_code: 1 } }
    if $result.exit_code == 0 {
        { available: true, sha: ($result.stdout | str trim) }
    } else {
        { available: false, sha: "" }
    }
}

# Hash a directory tree via git tree hash if available, else sorted file hashes.
export def tree-hash [dir: string]: nothing -> string {
    let git_check = try { git -C $dir rev-parse --git-dir | complete } catch { { exit_code: 1 } }
    if $git_check.exit_code == 0 {
        let h = (git -C $dir rev-parse "HEAD^{tree}" | str trim)
        $"sha256:($h)"
    } else {
        glob $"($dir)/**/*"
        | where { $in | path type | $in == "file" }
        | sort
        | each { open $in --raw | hash sha256 }
        | str join "\n"
        | hash sha256
        | $"sha256:($in)"
    }
}
