
const CASSINI_SOCK_ENV = "CASSINI_DAEMON_SOCK"
const CASSINI_SESSION_ENV = "POLAR_CASSINI_SESSION_ID"

export const MANIFEST_PATH = "/etc/pipeline/pipeline.json"
export const SUBJECT_PREFIX = "polar.builds"
export const ELF_BINARY_ARTIFACT = "elf-binary-set"


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
# export def start-cassini-daemon [
#     --socket: string = "/tmp/cassini-pipeline.sock"
#     --timeout: int = 30
# ]: nothing -> int {
#     # If a daemon is already reachable at the socket, don't start another.
#     let status = (try { cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
#     if $status.exit_code == 0 {
#         log-info "cassini daemon already running, reusing" --component "cassini"
#         $env.CASSINI_DAEMON_SOCK = $socket
#         return -1  # sentinel: we didn't start it, don't stop it
#     }

#     log-info $"starting cassini daemon at ($socket)" --component "cassini"

#     let job_id = (job spawn {
#         cassini-client --daemon --foreground --socket $socket
#     })

#     # Poll until the socket is accepting commands or we time out.
#     let ready = (
#         0..($timeout * 2)  # 500ms steps
#         | each {|_|
#             let probe = (try { cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
#             if $probe.exit_code == 0 { true } else { sleep 500ms; false }
#         }
#         | any { $in == true }
#     )

#     if not $ready {
#         log-error $"cassini daemon did not become ready within ($timeout)s" --component "cassini"
#         job kill $job_id
#         error make { msg: "cassini daemon startup timeout" }
#     }

#     $env.CASSINI_DAEMON_SOCK = $socket
#     log-info "cassini daemon ready" --component "cassini"
#     $job_id
# }

# # Register a session for this pipeline run. The session ID correlates
# # all events emitted during the pipeline in the knowledge graph.
# # Sets POLAR_CASSINI_SESSION_ID in the environment.
# export def register-pipeline-session [pipeline_name: string, build_id: string]: nothing -> string {
#     let session_id = (
#         cassini-client --format json get-session
#         | from json
#         | get registration_id?
#         | default ""
#     )

#     if ($session_id | is-not-empty) {
#         log-info $"reusing cassini session ($session_id)" --component "cassini"
#         $env.POLAR_CASSINI_SESSION_ID = $session_id
#         return $session_id
#     }

#     # The daemon registers automatically on first connect; retrieve the ID it was assigned.
#     let status = (cassini-client --format json status | from json)
#     let sid = ($status.registration_id? | default "")
#     if ($sid | is-empty) {
#         error make { msg: "cassini daemon has no registration_id after startup" }
#     }

#     $env.POLAR_CASSINI_SESSION_ID = $sid
#     log-info $"cassini session registered: ($sid)" --component "cassini"
#     $sid
# }

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

# Internal — not exported. All public emit-* functions funnel through here.
def emit [subject_suffix: string, payload: record] {
    let envelope = {
        build_id: ($env.POLAR_BUILD_ID? | default "00000000-0000-0000-0000-000000000000")
        stage_exec_id: ($env.POLAR_STAGE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        pipeline_exec_id: ($env.POLAR_PIPELINE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        timestamp: (date now | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        subject: $"($SUBJECT_PREFIX).($subject_suffix)"
        payload: ($payload | merge { type: $subject_suffix })
    }
    # cassini-client publish goes here
    log-debug ($envelope | to json --raw)

    # tODO: invoke cassini client here
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

export def emit-artifact-produced [
    artifact_id: string
    artifact_type: string
    --name: string = ""
    --content_type: string = ""
] {
    mut payload = { artifact_id: $artifact_id, artifact_type: $artifact_type }
    if ($name | is-not-empty)         { $payload = ($payload | insert name $name) }
    if ($content_type | is-not-empty) { $payload = ($payload | insert content_type $content_type) }
    emit "artifact.produced" $payload
}

export def emit-dependency-resolved [
    artifact_id: string
    --name: string = ""
    --version: string = ""
    --role: string = ""
] {
    mut payload = { artifact_id: $artifact_id, artifact_type: "dependency" }
    if ($name | is-not-empty)    { $payload = ($payload | insert name $name) }
    if ($version | is-not-empty) { $payload = ($payload | insert version $version) }
    if ($role | is-not-empty)    { $payload = ($payload | insert role $role) }
    emit "dependency.resolved" $payload
}

export def emit-source-identified [artifact_id: string, git_commit: string] {
    emit "source.identified" { artifact_id: $artifact_id, git_commit: $git_commit }
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
