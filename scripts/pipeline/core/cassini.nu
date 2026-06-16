# ---------------------------------------------------------------------------
# Cassini
# ---------------------------------------------------------------------------
export const SUBJECT_PREFIX = "polar.provenance"
export const BUILD_EVENTS_TOPIC = $"($SUBJECT_PREFIX).events"
export const CASSINI_SOCK_ENV = "CASSINI_DAEMON_SOCK"
export const CASSINI_SESSION_ENV = "POLAR_CASSINI_SESSION_ID"

# ---------------------------------------------------------------------------
# Cassini daemon lifecycle (per-CI-job)
#
# Each call to start-cassini-daemon launches a cassini-client daemon scoped
# to, and torn down with, the calling job. That's deliberate, not a
# placeholder for "real" supervision — see the two notes below before
# changing it.
#
# 1. Why per-job, not a shared/long-lived daemon
#
#    cassini client certs are short-lived and issued per job by an external
#    service. cassini-client's TcpClientActor builds its rustls ClientConfig
#    once at startup from those cert files and never reloads it — not even on
#    reconnect. A daemon that outlived the job whose cert it was started with
#    would either keep using a credential that's since been rotated out from
#    under it, or start failing every reconnect once the broker stops
#    accepting that cert. Per-job lifetime keeps the daemon's lifespan inside
#    the cert's validity window by construction. Don't "fix" this by trying
#    to keep a daemon warm across jobs without first giving cassini-client a
#    way to reload its TLS config.
#
# 2. Why --foreground, not --daemon
#
#    `cassini-client --daemon` (without --foreground) re-execs itself and the
#    re-exec'd process gets adopted by PID 1 — it is, by design, NOT a child
#    of this shell. Nothing in this process, Nu's job system included, can
#    track or signal that process via normal parent/child mechanisms; the
#    only handle left behind is the pidfile it writes at <socket>.pid.
#
#    Running --foreground avoids all of that: cassini-client never re-execs,
#    this shell is its real parent, and `job spawn` gives us Nu's own
#    job-table bookkeeping as a *secondary* cleanup path layered on top of
#    the pidfile (which remains the primary, version-independent mechanism —
#    see stop-cassini-daemon).
#
# ---------------------------------------------------------------------------
# 3. Why every job gets its own socket AND queue path — read this before
#    "simplifying" back to a fixed /tmp path
#
#    An earlier version of this module used a single fixed socket path
#    (/tmp/cassini-pipeline.sock) plus a "probe first, reuse if reachable"
#    pattern: if a daemon was already listening on that path, the job adopted
#    it and skipped killing it on exit.
#
#    That pattern is only correct if at most one job per host can ever be
#    using cassini-client at a time. On docker-executor runners that's
#    roughly true — each job gets its own container, its own /tmp, its own
#    PID namespace, nothing to collide with. On shell-executor runners, where
#    multiple jobs commonly run concurrently on the same host and share /tmp,
#    it is NOT true, and the failure mode is nasty specifically because it's
#    intermittent and gives no hint that another job is involved:
#
#      1. Job A starts, finds nothing at /tmp/cassini-pipeline.sock, starts a
#         daemon there, and records "I own this — stop it when I'm done."
#      2. Job B starts on the SAME HOST while A is still running, probes the
#         same path, finds A's daemon reachable, and adopts it — recording
#         "someone else owns this — leave it alone when I'm done."
#      3. Job A finishes first and, correctly per its own bookkeeping,
#         SIGTERMs "its" daemon — which is the daemon Job B is actively
#         using.
#      4. Every subsequent cassini-client call from Job B fails with
#         something like:
#
#           Error: Failed to send message: Messaging failed to enqueue the
#           message to the specified actor, the actor is likely terminated
#
#         which reads like a broker connectivity problem or a cassini
#         session bug, and is neither — it's two unrelated CI jobs that
#         happened to land on the same runner at the same time.
#
#    The same hazard applies to cassini-client's offline message queue file
#    (default /tmp/cassini-queue.jsonl): two daemons sharing that path while
#    the broker is unreachable can interleave writes, and a `drain`/`replay`
#    from either job can pick up and publish messages that were queued by the
#    OTHER job's topic/payload — a silent cross-job message leak, which is
#    worse than the socket collision because nothing errors at all.
#
#    Both reproduce ONLY when two jobs overlap on the same shell-executor
#    host, so either can pass hundreds of times before someone hits it. The
#    fix is structural, not defensive: give every job its own socket AND
#    queue path, derived from CI_JOB_ID (GitLab guarantees this is unique
#    instance-wide, including across retries). With nothing to "reuse",
#    neither bug can occur regardless of how many jobs share a host.
# ---------------------------------------------------------------------------
#
# Mechanism summary:
#   - The daemon writes its own pidfile at <socket>.pid before it accepts
#     connections (cassini's resolve_pid_path: socket.with_extension("pid")).
#   - The daemon's SIGTERM/SIGINT handler removes both the socket and the
#     pidfile on its way out.
#   - stop-cassini-daemon's primary mechanism is SIGTERM-to-pid-from-pidfile,
#     same as before, just sourced from a per-job path instead of a shared
#     one. `job kill` is a secondary, best-effort cleanup of Nu's own
#     job-table entry, in case `job spawn`'s "dies with the shell" guarantee
#     is what actually saved you (e.g. the job was cancelled before
#     stop-cassini-daemon ran at all).
#
# Open question, worth resolving before relying on this at CI volume: the
# daemon's SIGTERM handler currently removes its files and exits without
# sending the broker a graceful DisconnectRequest — the broker just sees the
# TCP/TLS connection drop. If the broker is slow to reap abrupt disconnects,
# many short-lived daemons per day could leave stale registrations
# accumulating broker-side. Worth a quick test (start, register, SIGTERM,
# check the broker's session list immediately after) before assuming
# SIGTERM-and-move-on is "clean" at scale.
#
# Verify against your Nu version before relying on this module:
#   - `job spawn` / `job kill` / `job list` semantics (job-control is an
#     actively-developed area of Nu).
#   - The `o+e>` combined stdout+stderr redirection operator used below.
#   - `random uuid` availability (only used for the non-CI fallback path).
# ---------------------------------------------------------------------------

# Internal: build the shared "base" path (no extension) this job's cassini
# state lives under. Derived from CI_JOB_ID so it's unique per job — see note
# 3 above. Falls back to a random id outside CI (e.g. running this module
# locally for development).
def cassini-base-path []: nothing -> string {
    let id = ($env.CI_JOB_ID? | default (random uuid))
    $"/tmp/cassini-pipeline-($id)"
}

# Internal: true if a cassini daemon is listening and responding on $socket.
def cassini-reachable [socket: string]: nothing -> bool {
    let probe = (
        try { ^cassini-client --socket $socket status | complete } catch { {exit_code: 1} }
    )
    $probe.exit_code == 0
}

# Start a per-job cassini-client daemon and wait for it to be ready.
#
# Returns a record: {socket, queue, log, pid, job_id, owned}
#   - socket/queue/log: paths this daemon was given / is writing to.
#   - pid: the daemon's pid, read from its pidfile (0 if unreadable).
#   - job_id: this module's `job spawn` id, or -1 if we didn't spawn one
#     (see `owned` below).
#   - owned: true if THIS call started the daemon and is responsible for
#     stopping it. False only in the (should-be-impossible-with-per-job-
#     paths) case where something is already answering on our derived
#     socket — see the warning logged in that branch for why we don't kill
#     a process we can't account for.
export def --env start-cassini-daemon [--timeout: int = 30]: nothing -> record {
    let base = cassini-base-path
    let socket = $"($base).sock"
    let queue = $"($base).queue.jsonl"
    let log = $"($base).log"
    let pid_path = $"($base).pid"

    if ($socket | path exists) {
        if (cassini-reachable $socket) {
            # Should not happen with per-job paths. Don't guess whose
            # process this is by killing it — just use it, and make sure
            # stop-cassini-daemon knows not to touch it.
            log-warn $"a daemon is already answering at ($socket) — this should not happen with a per-job socket path \(duplicate CI_JOB_ID, or CASSINI_DAEMON_SOCK set externally?\); reusing it rather than guessing whose it is" --component cassini
            let pid = (try { open --raw $pid_path | str trim | into int } catch { 0 })
            $env.CASSINI_DAEMON_SOCK = $socket
            return {socket: $socket, queue: $queue, log: $log, pid: $pid, job_id: -1, owned: false}
        }

        log-warn $"removing stale cassini state at ($base).\{sock,pid\} — leftover from a job that didn't clean up" --component cassini
        try { rm --force $socket }
        try { rm --force $pid_path }
    }

    log-info $"starting cassini daemon at ($socket)" --component cassini

    # --foreground: this shell is the daemon's real parent (see note 2).
    # --queue: per-job offline-queue path (see note 3).
    # o+e>: redirect the daemon's tracing output to its own log file rather
    # than relying on how job-spawned external-process output surfaces in
    # the parent shell, which is still in flux in Nu. Gives us a concrete
    # file to inspect/upload as a CI artifact on failure, regardless.
    let job_id = (job spawn {
        ^cassini-client --daemon --foreground --socket $socket --queue $queue o+e> $log
    })

    let ready = (
        0..($timeout * 2)
        | each {|_|
            if (cassini-reachable $socket) {
                true
            } else {
                sleep 500ms
                false
            }
        }
        | any { $in }
    )

    if not $ready {
        log-error $"cassini daemon did not become ready within ($timeout)s — log at ($log):" --component cassini
        try { open --raw $log | lines | last 20 | each {|l| log-error $l --component cassini} }
        try { job kill $job_id }
        try { rm --force $socket; rm --force $pid_path }
        error make {msg: $"cassini daemon startup timeout \(see ($log)\)"}
    }

    let pid = (try { open --raw $pid_path | str trim | into int } catch { 0 })
    if $pid == 0 {
        log-warn "cassini daemon is reachable but its pidfile is missing/unreadable — stop-cassini-daemon will fall back to `job kill`" --component cassini
    }

    $env.CASSINI_DAEMON_SOCK = $socket
    log-info $"cassini daemon ready \(pid ($pid)\)" --component cassini

    {socket: $socket, queue: $queue, log: $log, pid: $pid, job_id: $job_id, owned: true}
}

# Stop a daemon started by start-cassini-daemon. Pass the record it returned.
#
# SIGTERM-to-pid-from-pidfile is the primary mechanism (the daemon's signal
# handler removes its own socket and pidfile). `job kill` is secondary,
# best-effort cleanup of Nu's job-table entry — its failure once the
# underlying process is already gone is expected, not an error.
export def stop-cassini-daemon [daemon: record, --timeout: int = 3]: nothing -> nothing {
    if not $daemon.owned {
        log-info $"stop-cassini-daemon: ($daemon.socket) wasn't started by this job, leaving it alone" --component cassini
        return
    }

    if $daemon.pid == 0 {
        log-warn "stop-cassini-daemon: no pid recorded, falling back to `job kill`" --component cassini
        try { job kill $daemon.job_id }
        return
    }

    log-info $"stopping cassini daemon \(pid ($daemon.pid)\)" --component cassini

    # Default signal is 15 (SIGTERM) — handled by the daemon's signal handler.
    try { kill $daemon.pid }

    let gone = (
        1..($timeout * 2)
        | each {|_| sleep 500ms; not ($daemon.socket | path exists) }
        | any { $in }
    )

    if not $gone {
        log-warn "cassini socket still present after SIGTERM — force killing" --component cassini
        try { kill --force $daemon.pid }
        try { rm --force $daemon.socket; rm --force ($daemon.socket | str replace ".sock" ".pid") }
    }

    try { job kill $daemon.job_id }
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
    let json = $payload | to json --raw
    cassini-client publish $BUILD_EVENTS_TOPIC $json
}

# Legacy envelope — retained for agent-specific topics still consumed by
# k8s and GitLab agents. Do not use for new pipeline events.
export def emit [subject_suffix: string, payload: record]: nothing -> nothing {
    let envelope = {
        build_id: ($env.POLAR_BUILD_ID? | default "00000000-0000-0000-0000-000000000000")
        stage_exec_id: (
            $env.POLAR_STAGE_EXEC_ID?
            | default "00000000-0000-0000-0000-000000000000"
        )
        pipeline_exec_id: (
            $env.POLAR_PIPELINE_EXEC_ID?
            | default "00000000-0000-0000-0000-000000000000"
        )
        observed_at: (date now | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        payload: ($payload | merge {type: $subject_suffix})
    }
    let json = $envelope | to json --raw
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
