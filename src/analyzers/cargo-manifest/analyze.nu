# analyze.nu — entrypoint of the polar cargo-manifest analyzer image.
#
# Contract with the orchestrator (see SourceRef::pipeline_env and the k8s job
# builder): the verified, extracted package is at $env.BUILD_WORKSPACE, and the
# run is identified by $env.POLAR_BUILD_ID. The orchestrator has already
# recorded which package and analyzer image this run is (PackageAnalysisStarted);
# this script only reports findings against the build_id.
#
# Exit status is the outcome: non-zero means the analysis itself failed (not
# that it found something) and surfaces as ExecutionFailed.
#
# Run locally against an extracted crate (cassini-client must be on PATH):
#   POLAR_BUILD_ID=dev BUILD_WORKSPACE=/path/to/crate nu src/analyzers/cargo-manifest/analyze.nu
#
# The pipeline library is imported by its path relative to this file in the
# repo. The image lays its files out the same way (see package.nix), so these
# imports resolve identically there, in the repo, and under nu-check/nu-lint.

use ../../../scripts/pipeline/core/state.nu [init-pipeline-state]
use ../../../scripts/pipeline/core/events.nu [emit-analysis-finding]
use ../../../scripts/pipeline/core/cassini.nu [start-cassini-daemon stop-cassini-daemon]
use ../../../scripts/pipeline/core/logging.nu [log-info]
use rules.nu [analyze-manifest]

const COMPONENT = "cargo-manifest"

def main []: nothing -> nothing {
    let build_id = $env.POLAR_BUILD_ID? | default ""
    if ($build_id | is-empty) {
        error make {msg: "POLAR_BUILD_ID is not set — findings cannot be attributed to a run"}
    }
    let workspace = $env.BUILD_WORKSPACE? | default "/workspace"
    log-info $"analyzing ($env.CYCLOPS_SOURCE_PURL? | default $workspace)" --component $COMPONENT

    init-pipeline-state $build_id
    # Rules run before the daemon starts: a malformed package fails the run
    # without ever opening a broker connection.
    let findings = (analyze-manifest $workspace)

    let daemon = (start-cassini-daemon)
    let result = (try {
        for f in $findings {
            emit-analysis-finding $f.rule_id $f.message --severity "info" --path ($f.path? | default "")
        }
        null
    } catch {|e| $e })
    stop-cassini-daemon $daemon

    if $result != null {
        error make {msg: $"failed to emit findings: ($result.msg)"}
    }
    log-info $"($findings | length) finding\(s\) reported" --component $COMPONENT
}
