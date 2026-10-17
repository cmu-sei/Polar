# seed-toy-sbom.nu — put the toy crate's dependency graph into the knowledge
# graph, as an instrumented pipeline would.
#
# Generates toy-crate's SBOM with cargo-cyclonedx and publishes it as one
# SbomAnalyzed event through the pipeline library: the same path ci.nu takes
# for Polar's own crates. The build processor then writes a Package node, with
# its published-archive sha256, for each of the ~15 dependencies: a work queue
# small enough to run the analysis scheduler against on a laptop.
#
# Needs: cargo, cargo-cyclonedx, and (unless --dry-run) cassini-client on PATH
# with the usual broker env (BROKER_ADDR, CASSINI_SERVER_NAME, TLS_*).
#
#   nu src/analyzers/fixtures/seed-toy-sbom.nu --dry-run   # show what would be sent
#   nu src/analyzers/fixtures/seed-toy-sbom.nu

use ../../../scripts/pipeline/core/state.nu [init-pipeline-state]
use ../../../scripts/pipeline/core/sbom.nu [extract-graph-fragment]
use ../../../scripts/pipeline/core/hashing.nu [content-hash-file]
use ../../../scripts/pipeline/core/events.nu [emit-sbom-analyzed]
use ../../../scripts/pipeline/core/cassini.nu [start-cassini-daemon stop-cassini-daemon]
use ../../../scripts/pipeline/core/logging.nu [log-info]

const HERE = path self .
const COMPONENT = "seed-toy-sbom"

def main [
    --dry-run # Print the packages that would be published, and publish nothing
]: nothing -> nothing {

    # cargo-cyclonedx writes the SBOM next to Cargo.toml; work on a copy so the
    # repo stays untouched.
    let work = mktemp --directory
    cp --recursive ($HERE | path join toy-crate) $work
    let crate = $work | path join toy-crate
    let sbom_run = ^cargo cyclonedx --format json --manifest-path ($crate | path join Cargo.toml) | complete
    if $sbom_run.exit_code != 0 {
        error make {msg: $"cargo cyclonedx failed: ($sbom_run.stderr)"}
    }
    let sbom_file = $crate | path join polar-toy-crate.cdx.json

    let fragment = extract-graph-fragment (open $sbom_file) (content-hash-file $sbom_file)
    let packages = $fragment.components | select purl archive_sha256
    log-info $"($packages | length) dependencies in the toy crate's SBOM" --component $COMPONENT

    if $dry_run {
        print ($packages | table)
    } else {
        init-pipeline-state $"seed-toy-sbom-(random uuid)"
        let daemon = start-cassini-daemon
        let result = try {
            emit-sbom-analyzed $fragment polar-toy-crate.cdx.json
            null
        } catch {|e| $e }
        stop-cassini-daemon $daemon
        if $result != null {
            error make {msg: $"failed to publish the SBOM: ($result.msg)"}
        }
        log-info "published SbomAnalyzed" --component $COMPONENT
    }
    rm --recursive --force $work
}
