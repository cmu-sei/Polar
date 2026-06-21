# state.nu — pipeline-scoped singleton state
#
# Provides a lightweight, process-scoped key-value store for pipeline identity
# that persists across module calls without env var fragility or argument threading.
#
# Usage:
#   1. Call `init-pipeline-state <build_id>` once at the top of main.
#   2. Call `get-build-id` from any emit function to retrieve the same value.
#
# Backed by nushell's built-in `stor` (in-process SQLite). The table is
# process-scoped — no cross-run contamination, no external daemon, no file IPC.
# Subprocess invocations of `nu` will NOT inherit this state — all emission
# must happen within the same process, which is the case for ci.nu.

use ./logging.nu *

const COMPONENT = "state"
const TABLE     = "pipeline"

# Initialize pipeline-scoped state with a known, stable build_id.
#
# Must be called once before any emit function that reads build_id.
# Calling this a second time within the same process replaces the previous
# state — safe for test harnesses that run multiple scenarios sequentially.
#
# Fails loudly if the stor table cannot be created or written, rather than
# silently producing empty build_ids downstream.
export def init-pipeline-state [build_id: string]: nothing -> nothing {
    log-info $"initializing pipeline state with build_id: ($build_id)" --component $COMPONENT

    stor create --table-name $TABLE --columns {build_id: str}
    stor insert --table-name $TABLE --data-record {build_id: $build_id}

    log-debug "pipeline state initialized" --component $COMPONENT
}

# Retrieve the build_id set by init-pipeline-state.
#
# Fails with a clear error if called before init-pipeline-state — this is
# intentional. A missing build_id should never silently produce empty or
# random values; the pipeline must be explicitly initialized.
export def get-build-id []: nothing -> string {
    let result = try {
        stor open | query db $"SELECT build_id FROM ($TABLE) LIMIT 1"
    } catch {|e|
        log-error $"failed to read pipeline state — was init-pipeline-state called? ($e.msg)" --component $COMPONENT
        error make {msg: "pipeline state not initialized — call init-pipeline-state before any emit function"}
    }

    if ($result | is-empty) {
        log-error "pipeline state table exists but contains no rows" --component $COMPONENT
        error make {msg: "pipeline state not initialized — call init-pipeline-state before any emit function"}
    }

    let build_id = ($result | first | get build_id)
    log-debug $"get-build-id → ($build_id)" --component $COMPONENT
    $build_id
}

# Clear all pipeline state. Useful for test teardown.
# No-op if the table doesn't exist.
export def clear-pipeline-state []: nothing -> nothing {
    log-debug "clearing pipeline state" --component $COMPONENT
    try { stor delete --table-name $TABLE }
}
