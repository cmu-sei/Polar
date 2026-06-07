# ---------------------------------------------------------------------------
# Cassini
# ---------------------------------------------------------------------------
export const SUBJECT_PREFIX = "polar.provenance"
export const BUILD_EVENTS_TOPIC = $"($SUBJECT_PREFIX).events"
export const CASSINI_SOCK_ENV = "CASSINI_DAEMON_SOCK"
export const CASSINI_SESSION_ENV = "POLAR_CASSINI_SESSION_ID"

# ---------------------------------------------------------------------------
# Cassini daemon lifecycle
#
# The cassini-client daemon re-execs itself as an orphan process when called
# with --daemon (no --foreground). The parent invocation returns immediately;
# the child is adopted by PID 1 and runs independently of this script.
#
# Consequences for lifetime management:
#   - Nu's job system cannot track or kill the daemon — it never owned it.
#   - The daemon writes its own PID file at <socket>.pid before accepting
#     connections (resolved by Rust's resolve_pid_path: socket.with_extension("pid")).
#   - The daemon's SIGTERM handler removes both the socket and PID file on exit.
#   - Stopping = sending SIGTERM to the PID in the PID file.
#
# The "already running" path is the normal case in a warm CI environment.
# start-cassini-daemon is idempotent: if the socket is reachable it returns
# a sentinel (-1) and stop-cassini-daemon treats -1 as a no-op.
# ---------------------------------------------------------------------------

export def start-cassini-daemon [
    --socket: string = "/tmp/cassini-pipeline.sock"
    --timeout: int   = 30
]: nothing -> int {
    # Probe first — if reachable, reuse the running daemon.
    # status exits 0 when the daemon responds; non-zero or exception means absent/stale.
    let probe = (try { ^cassini-client --socket $socket status | complete } catch {{exit_code: 1}})
    if $probe.exit_code == 0 {
        log-info "cassini daemon already running, reusing" --component cassini
        $env.CASSINI_DAEMON_SOCK = $socket
        return (-1)   # sentinel: caller must NOT signal this daemon on exit
    }

    # Remove a stale socket if one exists — the daemon won't start if it finds
    # a socket file it didn't create.
    if ($socket | path exists) {
        log-warn $"removing stale cassini socket at ($socket)" --component cassini
        try { rm --force $socket }
    }

    log-info $"starting cassini daemon at ($socket)" --component cassini

    # --daemon causes the Rust process to re-exec itself with --foreground and
    # then exit. The child is detached (stdin/stdout/stderr → /dev/null) and
    # adopted by PID 1. This call returns almost immediately.
    ^cassini-client --daemon --socket $socket

    # Poll the socket until the daemon is accepting connections or we time out.
    # Each iteration is 500ms; total attempts = timeout * 2.
    let ready = (
        0..($timeout * 2)
        | each {|_|
            let check = (try { ^cassini-client --socket $socket status | complete } catch {{exit_code: 1}})
            if $check.exit_code == 0 {
                true
            } else {
                sleep 500ms
                false
            }
        }
        | any { $in }
    )

    if not $ready {
        log-error $"cassini daemon did not become ready within ($timeout)s" --component cassini
        # Best-effort cleanup — the process may not have written its PID yet.
        let pid_file = ($socket | path parse | update extension pid | path join)
        if ($pid_file | path exists) {
            let pid = (try { open --raw $pid_file | str trim | into int } catch { 0 })
            if $pid > 0 { try { ^kill $pid } catch {} }
        }
        error make {msg: "cassini daemon startup timeout"}
    }

    $env.CASSINI_DAEMON_SOCK = $socket
    log-info "cassini daemon ready" --component cassini

    # Return the PID so stop-cassini-daemon can signal the orphan process.
    # We read it from the PID file the daemon wrote rather than tracking a
    # Nu job ID, which would be meaningless for a detached process.
    let pid_file = ($socket | path parse | update extension pid | path join)
    let pid = (try { open --raw $pid_file | str trim | into int } catch { 0 })
    if $pid == 0 {
        log-warn "could not read cassini daemon PID — stop-cassini-daemon will be a no-op" --component cassini
    }
    $pid
}

# Stop the cassini daemon started by start-cassini-daemon.
#
# Pass -1 (the "reused existing daemon" sentinel) to make this a no-op —
# we must not kill a daemon we didn't start.
#
# For a daemon we did start, sends SIGTERM. The daemon's signal handler
# disconnects from the broker cleanly and removes the socket + PID file.
# We wait up to 3s for the socket to disappear as confirmation.
export def stop-cassini-daemon [
    pid: int
    --socket: string = "/tmp/cassini-pipeline.sock"
]: nothing -> nothing {
    if $pid == -1 { return }
    if $pid == 0 {
        log-warn "stop-cassini-daemon: no valid PID, skipping" --component cassini
        return
    }

    log-info $"stopping cassini daemon (pid ($pid))" --component cassini

    try { ^kill $pid } catch {|e|
        log-warn $"could not send SIGTERM to cassini daemon: ($e.msg)" --component cassini
        return
    }

    # Wait for the socket to disappear — the daemon's signal handler removes
    # it as part of clean shutdown. If it's still present after 3s something
    # went wrong, but there's nothing more we can do.
    let gone = (
        1..6
        | each {|_| sleep 500ms; not ($socket | path exists) }
        | any { $in }
    )
    if not $gone {
        log-warn "cassini socket still present after SIGTERM — daemon may not have exited cleanly" --component cassini
    }
}

# ---------------------------------------------------------------------------
# Cassini provenance emission
#
# emit-provenance-event: canonical ProvenanceEvent envelope for the unified
# provenance events topic. All typed emit-* functions in events.nu call this.
# build_id lives on the variant payload, not on the envelope — only CI pipeline
# events have a build_id, and not all ProvenanceEvent variants are CI events.
#
# emit: legacy envelope retained for agent-specific topics still in use by
# k8s and GitLab agents. Do not use for new pipeline events.
# ---------------------------------------------------------------------------

# Emit a canonical ProvenanceEvent to the unified provenance events topic.
# The payload record must include a `type` field matching a ProvenanceEvent
# variant name in snake_case, plus all fields for that variant.
# build_id is carried on the payload itself for variants that have one —
# it is not on this envelope.
export def emit-provenance-event [payload: record]: nothing -> nothing {
    let json = ($payload | to json --raw)
    cassini-client publish $BUILD_EVENTS_TOPIC $json
}

# Legacy envelope — retained for agent-specific topics still consumed by
# k8s and GitLab agents. Do not use for new pipeline events.
export def emit [subject_suffix: string, payload: record]: nothing -> nothing {
    let envelope = {
        build_id: ($env.POLAR_BUILD_ID? | default "00000000-0000-0000-0000-000000000000")
        stage_exec_id: ($env.POLAR_STAGE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        pipeline_exec_id: ($env.POLAR_PIPELINE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        observed_at: (date now | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        payload: ($payload | merge {type: $subject_suffix})
    }
    let json = ($envelope | to json --raw)
    cassini-client publish $"($SUBJECT_PREFIX).($subject_suffix)" $json
}

# Construct the source identity envelope from GitLab CI environment variables.
# Outside a GitLab CI context these default to null — that's honest.
# TODO: make system configurable for non-GitLab CI environments.
def build-origin []: nothing -> record {
    {
        system: "gitlab"
        native_id: ($env.CI_PIPELINE_ID? | default null)
        native_job_id: ($env.CI_JOB_ID? | default null)
        native_url: ($env.CI_JOB_URL? | default null)
    }
}
