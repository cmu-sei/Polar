
# state.nu — pipeline-scoped singleton state
#
# Process-scoped key/value + relational state for the pipeline, backed by
# nushell's built-in `stor` (in-process, in-MEMORY SQLite). It persists across
# module calls within one `nu` process; it does NOT survive process exit and is
# NOT inherited by subprocess `nu` invocations. That is exactly the right
# lifetime for run-scoped derived state (build_id, per-run SBOM lookups) and
# exactly the wrong store for anything that needs to be durable or audited —
# an append-only event log belongs on disk, not here.
#
# Tables owned by this module:
#   pipeline        one row: the build_id singleton
#   sbom_lookup     one row per analyzed workspace member (root-level)
#   sbom_packages   one row per SBOM component (full closure, for vuln joins)
#
# Design notes on `stor` that the API below has to work around:
#   - `stor create` THROWS if the table already exists. Every create goes
#     through `ensure-table` so re-init within one process (test harnesses)
#     is safe. The original code called `stor create` bare, which made the
#     "safe to call twice" claim in init-pipeline-state false.
#   - `stor` has no PRIMARY KEY / UNIQUE / UPSERT. Logical keys are a
#     convention, not enforced. Writers dedupe before insert and readers use
#     LIMIT 1, so a package that appears in several member SBOMs converges.

use ./logging.nu *

const COMPONENT       = "state"
const TABLE           = "pipeline"
const SBOM_TABLE      = "sbom_lookup"
const SBOM_PKG_TABLE  = "sbom_packages"
const IMG_TABLE       = "image_tarball"

# ---------------------------------------------------------------------------
# stor helpers
# ---------------------------------------------------------------------------

# True if `table` already exists in the in-memory store.
def table-exists [table: string]: nothing -> bool {
    let rows = try {
        stor open
        | query db "SELECT name FROM sqlite_master WHERE type = 'table' AND name = :t" --params {t: $table}
    } catch { [] }
    ($rows | is-not-empty)
}

# Create `table` only if it doesn't already exist. `stor create` throws on a
# pre-existing table, so this is the guard every creation path must use.
def ensure-table [table: string, columns: record]: nothing -> table {
    if (table-exists $table) { return }
    stor create --table-name $table --columns $columns
}

# Normalize a crate name for fallback matching (hyphen/underscore/case). The
# authoritative key is the exact name; this only backstops purl-vs-metadata
# name skew.
def norm-name [name: string]: nothing -> string {
    $name | str downcase | str replace --all "_" "-"
}

# ---------------------------------------------------------------------------
# build_id singleton
# ---------------------------------------------------------------------------

# Initialize pipeline-scoped state with a known, stable build_id, and create
# the SBOM tables for this run. Idempotent: safe to call more than once in a
# single process (replaces the build_id row, leaves table schemas intact).
export def init-pipeline-state [build_id: string]: nothing -> nothing {
    log-info $"initializing pipeline state with build_id: ($build_id)" --component $COMPONENT

    ensure-table $TABLE {build_id: str}
    # Replace any prior build_id so a second init in the same process is safe.
    try { stor open | query db $"DELETE FROM ($TABLE)" }
    stor insert --table-name $TABLE --data-record {build_id: $build_id}

    ensure-table $SBOM_TABLE {
        name:            str
        content_hash:    str
        root_purl:       str
        component_count: int
        edge_count:      int
    }
    ensure-table $SBOM_PKG_TABLE {
        name:                str
        name_norm:           str
        version:             str
        purl:                str
        sbom_content_hash:   str
    }
    ensure-table $IMG_TABLE {
        image_name:   str
        tarball_hash: str
    }

    log-debug "pipeline state initialized" --component $COMPONENT
}

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

    $result | first | get build_id
}

# Clear ALL pipeline state (build_id + SBOM tables). Test teardown only.
export def clear-pipeline-state []: nothing -> nothing {
    log-debug "clearing pipeline state" --component $COMPONENT
    for t in [$TABLE $SBOM_TABLE $SBOM_PKG_TABLE $IMG_TABLE] {
        try { stor delete --table-name $t }
    }
}

# ---------------------------------------------------------------------------
# SBOM state — root level (#231)
#
# Written once per analyzed workspace member in Phase 1. Read by Phase 2
# (binary linking) and Phase 3 (manifest purl cross-validation) via accessors,
# so `sbom_lookup` no longer threads through four call signatures.
# ---------------------------------------------------------------------------

# Record a member SBOM's root-level facts. Idempotent per `name`: a re-analysis
# in the same run replaces the prior row rather than duplicating it.
export def record-sbom [
    name: string
    content_hash: string
    root_purl: string
    component_count: int
    edge_count: int
]: nothing -> table {
    try { stor open | query db "DELETE FROM sbom_lookup WHERE name = :n" --params {n: $name} }
    stor insert --table-name $SBOM_TABLE --data-record {
        name:            $name
        content_hash:    $content_hash
        root_purl:       $root_purl
        component_count: $component_count
        edge_count:      $edge_count
    }
}

# Root-level SBOM info for a member, or null if this run never analyzed it.
# Replaces `$sbom_lookup | get -o <name>`.
export def get-sbom-info [name: string]: nothing -> any {
    let rows = try {
        stor open
        | ( query db "SELECT name, content_hash, root_purl, component_count, edge_count
                    FROM sbom_lookup WHERE name = :n LIMIT 1" --params {n: $name} )
    } catch { [] }
    if ($rows | is-empty) { null } else { $rows | first }
}

# Every root purl analyzed this run. The set the manifest cross-check tests
# membership against.
export def get-sbom-root-purls []: nothing -> list<string> {
    let rows = try {
        stor open
        | query db "SELECT DISTINCT root_purl FROM sbom_lookup WHERE root_purl <> ''"
    } catch { [] }
    $rows | get root_purl
}

# Cross-validate a manifest's static root_purl against what was actually
# analyzed. Returns a diagnostic the caller logs; keeping the decision here
# (rather than a bare bool) lets `process-image` warn with specifics.
#
# status:
#   "unset"    manifest declares no root_purl — nothing to check
#   "match"    exact purl was analyzed this run
#   "version_drift"  same crate analyzed, different version (the common bug)
#   "absent"   crate not analyzed at all this run
export def check-manifest-purl [manifest_purl: string]: nothing -> record {
    if ($manifest_purl | is-empty) {
        return {status: "unset", manifest_purl: $manifest_purl, analyzed_purl: null}
    }

    let known = (get-sbom-root-purls)
    if ($manifest_purl in $known) {
        return {status: "match", manifest_purl: $manifest_purl, analyzed_purl: $manifest_purl}
    }

    # Same crate name, different version? Split "pkg:cargo/<name>@<version>".
    let stem = ($manifest_purl | split row "@" | first)
    let same_crate = ($known | where {|p| ($p | split row "@" | first) == $stem })
    if ($same_crate | is-not-empty) {
        return {status: "version_drift", manifest_purl: $manifest_purl, analyzed_purl: ($same_crate | first)}
    }

    {status: "absent", manifest_purl: $manifest_purl, analyzed_purl: null}
}

# ---------------------------------------------------------------------------
# SBOM state — component closure (#217)
#
# Written once per SBOM component in Phase 1 (transitive deps included). This
# is the join surface for vulnerability attribution: a scanner resolves its
# (crate, version) finding to the exact purl the graph's Package node was
# keyed on, so the AFFECTS edge lands on an existing node instead of MERGE-ing
# an orphan.
# ---------------------------------------------------------------------------

# Record one component. Idempotent on (name, version): the same dep pulled in
# by several members converges to one row.
export def record-sbom-package [
    name: string
    version: string
    purl: string
    sbom_content_hash: string
]: nothing -> table {
    if ($purl | is-empty) { return }
    try {
        stor open
        | ( query db "DELETE FROM sbom_packages WHERE name = :n AND version = :v"
            --params {n: $name, v: $version} )
    }
    stor insert --table-name $SBOM_PKG_TABLE --data-record {
        name:              $name
        name_norm:         (norm-name $name)
        version:           $version
        purl:              $purl
        sbom_content_hash: $sbom_content_hash
    }
}

# Resolve (crate, version) to the canonical purl in this run's closure. Exact
# match first, normalized-name fallback second, null if absent. Absent means
# the crate isn't in the SBOM closure (usually a dev/build-dep scope mismatch)
# — the caller links the finding to the build only, never fabricates a purl.
export def lookup-package-purl [name: string, version: string]: nothing -> any {
    if ($name | is-empty) { return null }

    let exact = try {
        stor open
        | ( query db "SELECT purl FROM sbom_packages WHERE name = :n AND version = :v LIMIT 1"
            --params {n: $name, v: $version} )
    } catch { [] }
    if ($exact | is-not-empty) { return ($exact | first | get purl) }

    let norm = try {
        stor open
        |  ( query db "SELECT purl FROM sbom_packages WHERE name_norm = :n AND version = :v LIMIT 1"
            --params {n: (norm-name $name), v: $version} )
    } catch { [] }
    if ($norm | is-not-empty) { return ($norm | first | get purl) }

    null
}

# The full component index as a table, for callers that want to resolve in
# bulk without per-finding round trips (e.g. scanning.nu's pure
# parse-cargo-audit, which takes the index as an argument to stay testable).
export def get-all-sbom-packages []: nothing -> table {
    try {
        stor open
        | query db "SELECT name, name_norm, version, purl FROM sbom_packages"
    } catch { [] }
}

# ---------------------------------------------------------------------------
# Image tarball hash (#231)
#
# nix-build-image computes tarball_hash via content-hash-file. Recording it
# here immediately makes it retrievable independently, so build-and-push-image
# no longer depends on the field surviving on a return record — the exact
# contract whose accidental breakage produced a generic "Eval block failed".
# The write belongs inside nix-build-image (oci.nu), next to content-hash-file;
# the read side lives wherever emission happens.
# ---------------------------------------------------------------------------

# Idempotent per image_name.
export def record-tarball-hash [image_name: string, tarball_hash: string]: nothing -> table {
    try { stor open | query db "DELETE FROM image_tarball WHERE image_name = :n" --params {n: $image_name} }
    stor insert --table-name $IMG_TABLE --data-record {
        image_name:   $image_name
        tarball_hash: $tarball_hash
    }
}

# Tarball hash for an image, or null if never recorded. Callers that treat a
# missing hash as fatal should check for null and fail loudly (see the guard
# added to build-and-push-image).
export def get-tarball-hash [image_name: string]: nothing -> string {
    let rows = try {
        stor open
        | query db "SELECT tarball_hash FROM image_tarball WHERE image_name = :n LIMIT 1" --params {n: $image_name}
    } catch { [] }
    if ($rows | is-empty) { null } else { $rows | first | get tarball_hash }
}

# ---------------------------------------------------------------------------
# Preflight guard
#
# Fails loudly at the top of Phase 3 if the SBOM state a run expects isn't
# populated — the class of regression that previously surfaced as a generic
# "Eval block failed" when a load-bearing return-record field went missing.
# ---------------------------------------------------------------------------
export def assert-sbom-state-populated []: nothing -> nothing {
    let n = try {
        stor open | query db "SELECT count(*) AS c FROM sbom_lookup" | first | get c
    } catch { 0 }
    if $n == 0 {
        log-error "no SBOM state recorded — Phase 1 did not run or did not persist" --component $COMPONENT
        error make {msg: "sbom_lookup is empty; refusing to proceed to image work"}
    }
    log-debug $"sbom state preflight ok: ($n) member SBOM\(s\) recorded" --component $COMPONENT
}
