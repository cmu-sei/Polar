# scanning.nu — vulnerability scanning and normalization for the Polar pipeline
#
# Runs cargo-audit against the workspace lockfile, normalizes each finding,
# and routes it to one of two graph node shapes (see the Finding-model spike):
#
#   kind in {vulnerability, unsound, notice}  -> emit-security-advisory
#       a SecurityAdvisory: a real identified issue.
#   kind == unmaintained                      -> emit-package-status-found
#       a PackageStatus: a maintenance-state fact, not a discrete flaw.
#
# `affected_package` on both is the *exact* purl already present on a Package
# node in the graph — never a hand-reconstructed one.
#
# The join surface (crate name/version -> canonical purl) lives in state.nu's
# sbom_packages table, populated by process-cargo-sboms in Phase 1 from every
# parsed SBOM component's purl — the same purl handle_sbom_analyzed MERGE-keys
# the Package node on. This module does NOT build its own index; it reads that
# table via get-all-sbom-packages so a finding resolves to a purl guaranteed
# to already exist as a node, instead of one reconstructed by hand that could
# silently MERGE an orphan.
#
# Rust-side ingestion (extending the SecurityAdvisory/PackageStatus fields
# into ProvenanceEvent and writing the graph edges) is not built yet. Until
# it is, ProvenanceEvent's internally-tagged, non-deny_unknown_fields enum
# means these extra fields are parsed and silently ignored — the emitter can
# lead that change safely.
#
# Note: `--fix_version` on emit-security-advisory carries a semver
# CONSTRAINT (e.g. ">=0.6.1"), not a concrete version — cargo-audit's
# `versions.patched` is a range, never a single target version.
#
# ── Wiring into ci.nu (Phase 1.5, right after process-cargo-sboms) ──────────
#   use ./core/scanning.nu *
#   ...
#   process-cargo-sboms $sbom_files $packages
#   let scan_findings = (run-vulnerability-scan $ws_manifest --offline=$audit_offline)
# ---------------------------------------------------------------------------

use logging.nu *
use events.nu [emit-security-advisory emit-package-status-found]
use state.nu [get-all-sbom-packages]

const COMPONENT = "scan"

# ---------------------------------------------------------------------------
# Purl resolution stays a pure function over a table so parse-cargo-audit
# remains unit-testable without touching stor. The table itself now comes
# from state.nu's sbom_packages (see run-cargo-audit below) rather than being
# built here — this module has no business re-deriving what Phase 1 already
# recorded.
# ---------------------------------------------------------------------------

# Resolve a (crate name, version) pair to the canonical purl in the index.
# Exact (name, version) first; then a normalized-name fallback to absorb the
# hyphen/underscore/case edge cases. Returns null when the package isn't in
# the SBOM's closure at all — the caller must treat that as "link to build
# only", never as license to fabricate a purl.
export def resolve-package-purl [index: table, name: string, version: string]: nothing -> any {
    if ($name | is-empty) { return null }

    let exact = ($index | where {|r| $r.name == $name and $r.version == $version })
    if ($exact | is-not-empty) { return ($exact | first | get purl) }

    let nk = ($name | str downcase | str replace --all "_" "-")
    let norm = ($index | where {|r| $r.name_norm == $nk and $r.version == $version })
    if ($norm | is-not-empty) { return ($norm | first | get purl) }

    null
}

# ---------------------------------------------------------------------------
# CVSS v3.1 base score → qualitative severity.
#
# cargo-audit's `advisory.cvss` is a CVSS *vector string* (or null), NOT a
# severity label and NOT a numeric score. We compute the base score per the
# CVSS v3.1 spec so `severity` is a real, filterable band ('high'/'critical'/
# ...). Informational advisories (no CVSS) fall through to 'unknown'. This
# banding is deterministic and can be superseded later by NVD/OSV enrichment.
# ---------------------------------------------------------------------------

# Roundup to 1 decimal place, per the CVSS v3.1 Appendix A definition
# (integer arithmetic to avoid binary-float drift).
def roundup-1dp [x: float]: nothing -> float {
    let int_input = (($x * 100000) | math round | into int)
    if ($int_input mod 10000) == 0 {
        ($int_input / 100000)
    } else {
        (((($int_input / 10000) | math floor | into int) + 1) / 10)
    }
}

# Returns the base score as a float, or null if `vector` isn't a parseable
# CVSS v3.x base vector (e.g. an old v2 vector, or a malformed/empty string).
export def cvss3-base-score [vector: string]: nothing -> any {
    if not (($vector | str upcase) | str starts-with "CVSS:3") { return null }

    # "CVSS:3.1/AV:N/AC:L/..." -> { AV: "N", AC: "L", ... }
    let m = ($vector | split row "/" | skip 1 | reduce -f {} {|it, acc|
        let kv = ($it | split row ":")
        if ($kv | length) == 2 { $acc | insert $kv.0 $kv.1 } else { $acc }
    })

    let scope_changed = (($m.S? | default "U") == "C")

    let av = ({N: 0.85, A: 0.62, L: 0.55, P: 0.2}   | get -o ($m.AV? | default ""))
    let ac = ({L: 0.77, H: 0.44}                    | get -o ($m.AC? | default ""))
    let pr = (if $scope_changed { {N: 0.85, L: 0.68, H: 0.5} } else { {N: 0.85, L: 0.62, H: 0.27} }
              | get -o ($m.PR? | default ""))
    let ui = ({N: 0.85, R: 0.62}                    | get -o ($m.UI? | default ""))
    let ci = ({H: 0.56, L: 0.22, N: 0.0}            | get -o ($m.C? | default ""))
    let ii = ({H: 0.56, L: 0.22, N: 0.0}            | get -o ($m.I? | default ""))
    let ai = ({H: 0.56, L: 0.22, N: 0.0}            | get -o ($m.A? | default ""))

    # Any missing/unknown metric → we can't score it honestly.
    if ([$av $ac $pr $ui $ci $ii $ai] | any {|x| $x == null }) { return null }

    let iss = (1 - ((1 - $ci) * (1 - $ii) * (1 - $ai)))
    let impact = if $scope_changed {
        (7.52 * ($iss - 0.029)) - (3.25 * (($iss - 0.02) ** 15))
    } else {
        (6.42 * $iss)
    }
    if $impact <= 0 { return 0.0 }

    let exploitability = (8.22 * $av * $ac * $pr * $ui)
    let combined = ($impact + $exploitability)
    let raw = if $scope_changed {
        ([(1.08 * $combined) 10.0] | math min)
    } else {
        ([$combined 10.0] | math min)
    }
    (roundup-1dp $raw)
}

# Map a base score to the CVSS v3.1 qualitative band.
export def cvss-band [score: any]: nothing -> string {
    if $score == null   { return "unknown" }
    if $score == 0      { return "none" }
    if $score < 4.0     { return "low" }
    if $score < 7.0     { return "medium" }
    if $score < 9.0     { return "high" }
    "critical"
}

# ---------------------------------------------------------------------------
# Pure transform: cargo-audit JSON report -> normalized finding records.
#
# cargo-audit's report has TWO finding buckets, not one:
#   report.vulnerabilities.list   — findings it treats as blocking by default.
#                                   No "kind" field on the entry itself.
#   report.warnings.<bucket>      — "unmaintained" / "unsound" / "yanked" /
#                                   "notice", non-blocking by cargo-audit's
#                                   own default policy but frequently real,
#                                   RUSTSEC-tracked issues (e.g. git2 UB,
#                                   rkyv use-after-free land here, not in
#                                   `vulnerabilities`). Each entry is
#                                   self-tagged with its bucket via `kind`.
#                                   `yanked` entries carry advisory: null —
#                                   there's no RUSTSEC ID, so nothing to key
#                                   a Vulnerability node on; skipped, logged.
#
# Both buckets share the same per-entry shape ({advisory, package, versions,
# affected}), so both route through normalize-audit-entry below.
#
# No side effects, no emission — unit-testable against a captured fixture:
#
#   let report = (open tests/fixtures/cargo-audit.json)
#   let index  = [{name: "tokio", name_norm: "tokio", version: "1.28.0", purl: "pkg:cargo/tokio@1.28.0"}]
#   parse-cargo-audit $report $index
# ---------------------------------------------------------------------------

# One finding entry -> a normalized record, or null if it can't be identified
# (no advisory — currently only `yanked` entries lack one).
def normalize-audit-entry [entry: record, index: table, default_kind: string]: nothing -> any {
    let adv = ($entry.advisory? | default null)
    if $adv == null {
        return null
    }

    let pkg     = ($entry.package? | default {})
    let name    = ($pkg.name?    | default "")
    let version = ($pkg.version? | default "")

    let cvss_vec = ($adv.cvss? | default null)
    let score    = if ($cvss_vec != null) { (cvss3-base-score $cvss_vec) } else { null }

    let aliases = ($adv.aliases? | default [])
    let cve  = ($aliases | where {|a| ($a | str upcase) | str starts-with "CVE" }  | first | default null)
    let ghsa = ($aliases | where {|a| ($a | str upcase) | str starts-with "GHSA" } | first | default null)

    # `patched`/`unaffected` are semver CONSTRAINTS (">=0.6.1"), not concrete
    # versions — do not present these as "the version to upgrade to" without
    # the comparator; a range is not a target.
    let patched     = ($entry.versions?.patched?    | default [])
    let unaffected  = ($entry.versions?.unaffected? | default [])

    {
        identifier:            ($adv.id? | default "")
        kind:                  ($entry.kind? | default $default_kind)   # vulnerability | unmaintained | unsound | notice
        cve_id:                $cve
        ghsa_id:               $ghsa
        severity:              (cvss-band $score)   # CVSS-derived; "unknown" absent CVSS — expect this often. Filter on `kind`, not just severity.
        scanner:                "cargo-audit"
        affected_package:      (resolve-package-purl $index $name $version)   # null if unresolved
        patched_constraint:    ($patched    | first | default null)   # e.g. ">=0.6.1" — a range, not a version
        unaffected_constraint: ($unaffected | first | default null)   # e.g. "<0.9.0"
        advisory_url:          ($adv.url? | default null)

        # Carried for logging / later enrichment; the current processor
        # ignores these fields. cvss keeps the raw vector so an enrichment
        # pass can reband without re-running the scanner.
        cvss:                  $cvss_vec
        package_name:          $name
        package_version:       $version
    }
}

export def parse-cargo-audit [report: record, index: table]: nothing -> list<record> {
    # cargo-deny's --audit-compatible-output puts findings as a BARE LIST
    # directly under `vulnerabilities` — no {found, count, list} wrapper the
    # way cargo-audit itself shapes it. Confirmed against a real capture.
    # Empirically a single JSON document as long as exactly one advisory
    # database is configured (the default) — cargo-deny's own docs note
    # output becomes one JSON object PER configured database if more than
    # one is added to deny.toml's [advisories] db-urls, which this parser
    # does not handle. If a second database is ever configured, this needs
    # revisiting (multi-document parse), not a silent re-use.
    let vuln_findings = (
        $report.vulnerabilities? | default []
        | each {|v| normalize-audit-entry $v $index "vulnerability" }
    )

    # report.warnings is a record keyed by bucket name (unmaintained/unsound/
    # yanked/...), each holding a list of entries. Iterate whatever buckets
    # are actually present rather than hardcoding the three cargo-audit
    # currently ships, so a future bucket doesn't silently go unread again.
    let warning_findings = (
        $report.warnings? | default {}
        | transpose kind entries
        | each {|bucket|
            $bucket.entries | each {|e| normalize-audit-entry $e $index $bucket.kind }
        }
        | flatten
    )

    ($vuln_findings | append $warning_findings)
    | where {|f| $f != null and ($f.identifier | is-not-empty) }
}


# ---------------------------------------------------------------------------
# Effectful: run the vulnerability scan, normalize, resolve, emit.
#
# As of the Tier 1 consolidation, this shells out to `cargo deny check
# advisories --audit-compatible-output`, not `cargo-audit` directly.
# cargo-deny's advisories check is built on the same `rustsec` crate
# cargo-audit uses, and --audit-compatible-output reshapes its JSON to match
# cargo-audit's own report structure closely enough that parse-cargo-audit
# needed exactly one change (the vulnerabilities-is-a-bare-list fix above) —
# confirmed against a real captured run, not assumed. parse-cargo-audit keeps
# its name because it parses "the audit-compatible shape," which is the
# format's own name regardless of which binary produced it.
#
#   manifest_path  path to the workspace Cargo.toml — cargo-deny operates via
#                  --manifest-path, not a direct lockfile argument the way
#                  cargo-audit's --file did.
#   --offline      maps to cargo-deny's --disable-fetch (confirmed flag name;
#                  cargo-audit's old --no-fetch does not exist here). There is
#                  no CLI --db-path equivalent — cargo-deny's advisory-db
#                  location is a deny.toml [advisories] concern, not a
#                  per-invocation flag, so that parameter is gone. Point
#                  deny.toml's db-path at a vendored clone for hermetic runs.
#
# Known information loss from the swap, not a bug: cargo-audit's JSON carries
# a `database` block (advisory-db commit / last-updated). cargo-deny's
# --audit-compatible-output has no equivalent field — confirmed absent from a
# real capture. The log line below will report "unknown" for db freshness
# going forward; if that fact matters later (see the ScanContext proposal),
# it has to come from reading cargo-deny's local advisory-db git clone
# directly, not from this tool's own output.
#
# --exclude-dev is deliberately NOT passed. Passing it would narrow
# cargo-deny's scope below what cargo-audit used to see, making the
# SBOM-vs-scanner dependency-scope divergence worse, not better — the
# opposite of where this pipeline is headed on scope.
#
# The resolution index is pulled from state.nu's sbom_packages table
# (get-all-sbom-packages), so this must run after process-cargo-sboms has
# populated it in the same process — stor is process-scoped and won't survive
# a call from a fresh `nu` invocation. If the table is empty (Phase 1 didn't
# run, or ran in a different process), every finding will fail to resolve and
# fall back to a build-level-only link; that's a caller-ordering bug, not a
# purl-matching bug, so check state.nu's assert-sbom-state-populated first if
# resolution looks suspiciously empty.
#
# Returns the normalized findings (for pipeline summaries / assertions).
# ---------------------------------------------------------------------------
export def run-vulnerability-scan [
    manifest_path: path
]: nothing -> list<record> {
    if not ($manifest_path | path exists) {
        log-warn $"vulnerability scan: manifest not found at ($manifest_path) — skipping" --component $COMPONENT
        return []
    }

    let index = (get-all-sbom-packages)
    if ($index | is-empty) {
        log-warn "vulnerability scan: sbom_packages is empty — did process-cargo-sboms run first in this process? all findings will link to the build only" --component $COMPONENT
    }

    let args = [
        deny --manifest-path $manifest_path
        --format json
        check advisories
        --audit-compatible-output
    ]

    log-info $"running: cargo ($args | str join ' ')" --component $COMPONENT
    let res = (^cargo ...$args | complete)

    # cargo-deny, like cargo-audit before it, exits non-zero when it FINDS
    # advisories — that's the success path, not a failure. Parseable JSON on
    # stdout is the reliable success signal.
    let report = try { $res.stdout | from json } catch {
        let msg = ($res.stderr | default $res.stdout | str trim)
        log-warn $"cargo-deny produced no parseable JSON (exit ($res.exit_code)): ($msg)" --component $COMPONENT
        return []
    }

    let dep_count = ($report.lockfile.dependency-count | default '?')

    log-info $"vulnerability scan: auditing ($dep_count) locked deps \(advisory db freshness unavailable from cargo-deny's output\)" --component $COMPONENT

    # Entries with no advisory (currently: yanked crates) can't be linked to
    # a RUSTSEC id and are dropped by parse-cargo-audit. Surface what was
    # dropped so "8 findings" doesn't quietly become "7" with no trace.
    let yanked = ($report.warnings?.yanked? | default [])
    for y in $yanked {
        log-warn $"yanked \(no advisory, not emitted as a Finding — no keying scheme yet\): ($y.package.name)@($y.package.version)" --component $COMPONENT
    }

    let findings = (parse-cargo-audit $report $index)

    for f in $findings {
        let has_package = ($f.affected_package | default "" | is-not-empty)
        if not $has_package {
            # Unresolved = the vulnerable crate isn't in the SBOM closure.
            # Almost always a dependency-scope mismatch. Emitted with no
            # affected_package and no in_artifact — there is currently no
            # build-artifact content hash threaded into this function to link
            # against, so despite appearances, this is NOT actually linked to
            # anything in the graph yet. Still an open decision from before
            # the Tier 1 swap, unaffected by it.
            log-warn $"($f.identifier): ($f.package_name)@($f.package_version) not in SBOM closure — no package or artifact link will be emitted" --component $COMPONENT
        }

        if $f.kind == "unmaintained" {
            if $has_package {
                emit-package-status-found $f.kind $f.identifier $f.package_name $f.package_version --scanner $f.scanner --affected_package $f.affected_package --advisory_url ($f.advisory_url | default "")
            } else {
                emit-package-status-found $f.kind $f.identifier $f.package_name $f.package_version --scanner $f.scanner --advisory_url ($f.advisory_url | default "")
            }
        } else {
            if $has_package {
                emit-security-advisory $f.severity $f.identifier --scanner $f.scanner --kind $f.kind --affected_package $f.affected_package --cve_id ($f.cve_id | default "") --ghsa_id ($f.ghsa_id | default "") --fix_version ($f.patched_constraint | default "") --unaffected_constraint ($f.unaffected_constraint | default "") --advisory_url ($f.advisory_url | default "")
            } else {
                emit-security-advisory $f.severity $f.identifier --scanner $f.scanner --kind $f.kind --cve_id ($f.cve_id | default "") --ghsa_id ($f.ghsa_id | default "") --fix_version ($f.patched_constraint | default "") --unaffected_constraint ($f.unaffected_constraint | default "") --advisory_url ($f.advisory_url | default "")
            }
        }
    }

    let linked = ($findings | where {|f| ($f.affected_package | default "" | is-not-empty) } | length)
    let by_kind = ($findings | group-by kind | transpose kind entries | each {|k| $"($k.kind)=($k.entries | length)" } | str join ", ")
    log-info $"vulnerability scan: ($findings | length) finding\(s\) \(($by_kind)\), ($linked) linked to SBOM package\(s\)" --component $COMPONENT

    $findings
}
