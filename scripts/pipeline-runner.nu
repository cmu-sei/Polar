
#!/usr/bin/env nu
# pipeline_runner.nu
#
# Derivation-style pipeline runner. Reads a pipeline manifest, pins all
# declared inputs (source tree, lockfile, toolchain), executes stages,
# resolves cross-stage artifact references, emits provenance events,
# and writes the fully-pinned manifest to the artifact directory.
#
# The pinned manifest is the build derivation: given the same input pins,
# re-running the pipeline in the same container produces the same outputs
# for all pure stages.
#

# Dependencies (baked into container by Nix / container.dhall):
#   cassini-client, git, bash, coreutils

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

const MANIFEST_PATH = "/etc/pipeline/pipeline.json"
const TOOLCHAIN_PATH = "/etc/pipeline/toolchain.json"
const SUBJECT_PREFIX = "polar.builds.provenance"

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

def log [level: string, msg: string, --component: string = "runner"] {
    let ts = (date now | format date "%Y-%m-%dT%H:%M:%S%.3fZ")
    print $"($ts) [($level)] ($component) — ($msg)"
}

def log-info  [msg: string, --component: string = "runner"] { log "INFO"  $msg --component $component }
def log-warn  [msg: string, --component: string = "runner"] { log "WARN"  $msg --component $component }
def log-error [msg: string, --component: string = "runner"] { log "ERROR" $msg --component $component }
def log-debug [msg: string, --component: string = "runner"] { log "DEBUG" $msg --component $component }

# ---------------------------------------------------------------------------
# Content hashing
# ---------------------------------------------------------------------------

def content-hash-file [path: string]: nothing -> string {
    open $path --raw | hash sha256 | $"sha256:($in)"
}

def content-hash-dir [dir: string]: nothing -> string {
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

def content-hash [path: string]: nothing -> string {
    if ($path | path type) == "dir" {
        content-hash-dir $path
    } else {
        content-hash-file $path
    }
}

def git-commit-sha [dir: string]: nothing -> record {
    let result = try { git -C $dir rev-parse HEAD | complete } catch { { exit_code: 1 } }
    if $result.exit_code == 0 {
        { available: true, sha: ($result.stdout | str trim) }
    } else {
        { available: false, sha: "" }
    }
}

# ---------------------------------------------------------------------------
# Input pinning
# ---------------------------------------------------------------------------

# Resolve and pin all declared top-level inputs. Returns the inputs record
# with `pin` fields populated from the actual filesystem / toolchain state.
# This is the derivation step: after pinning, the manifest fully specifies
# the build inputs.
def pin-inputs [
    inputs: record
    working_dir: string
]: nothing -> record {
    mut pinned = $inputs

    # Pin each input based on its type.
    for field in ($inputs | columns) {
        let input = ($inputs | get $field)
        let pin = match $input.type {
            "git-tree" => {
                let h = (content-hash-dir $working_dir)
                let commit = (git-commit-sha $working_dir)
                log-info $"pinned input '($field)' -> ($h)" --component "pin"
                {
                    pin: $h
                    git_commit: (if $commit.available { $commit.sha } else { null })
                }
            }
            "nix-closure" => {
                # Read the toolchain manifest baked into the container by Nix.
                let tc = if ($TOOLCHAIN_PATH | path exists) {
                    open $TOOLCHAIN_PATH
                } else {
                    log-warn $"toolchain manifest not found at ($TOOLCHAIN_PATH) — using runtime probe" --component "pin"
                    # Fallback: probe tool versions at runtime. Less precise than
                    # Nix store paths but still captures what's actually present.
                    {
                        rustc: (try { rustc --version | str trim } catch { "unknown" })
                        cargo: (try { cargo --version | str trim } catch { "unknown" })
                        nix: (try { nix --version | str trim } catch { "unknown" })
                        nushell: (try { version | get version } catch { "unknown" })
                    }
                }
                let tc_hash = ($tc | to json --raw | hash sha256 | $"sha256:($in)")
                log-info $"pinned input '($field)' -> ($tc_hash)" --component "pin"
                {
                    pin: $tc_hash
                    toolchain: $tc
                }
            }
            "file" => {
                let file_path = ($working_dir | path join ($input.path? | default ""))
                if ($file_path | path exists) {
                    let h = (content-hash-file $file_path)
                    log-info $"pinned input '($field)' -> ($h)" --component "pin"
                    { pin: $h }
                } else {
                    log-warn $"input file '($field)' not found at ($file_path)" --component "pin"
                    { pin: null }
                }
            }
            _ => {
                log-warn $"unknown input type '($input.type)' for '($field)'" --component "pin"
                { pin: null }
            }
        }
        # Merge the pin data into the input record.
        $pinned = ($pinned | upsert $field ($input | merge $pin))
    }

    $pinned
}

# ---------------------------------------------------------------------------
# Cassini provenance emission
# ---------------------------------------------------------------------------

def emit [subject_suffix: string, payload: record] {
    let subject = $"($SUBJECT_PREFIX).($subject_suffix)"
    let envelope = {
        build_id: ($env.POLAR_BUILD_ID? | default "00000000-0000-0000-0000-000000000000")
        timestamp: (date now | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        payload: ($payload | merge { type: $subject_suffix })
    }

    log-debug "no-op emit"
    # try {
    #     $envelope | to json --raw | cassini-client publish $subject $in
    # } catch {|e|
    #     log-warn $"failed to emit ($subject): ($e.msg)" --component "provenance"
    # }
}

def emit-build-observed [
    exec_id: string
    parent_id: string
    command: string
    working_dir: string
] {
    let base = {
        execution_id: $exec_id
        command: $command
        working_dir: $working_dir
    }
    let payload = if $parent_id != "" {
        $base | merge { parent_execution_id: $parent_id }
    } else {
        $base
    }
    emit "build.observed" $payload
}

def emit-build-completed [
    exec_id: string
    exit_code: int
    duration_ms: int
] {
    emit "build.completed" {
        execution_id: $exec_id
        exit_code: $exit_code
        duration_ms: $duration_ms
    }
}

def emit-artifact-produced [
    exec_id: string
    artifact_id: string
    artifact_type: string
    --name: string = ""
    --content_type: string = ""
] {
    let base = {
        execution_id: $exec_id
        artifact_id: $artifact_id
        artifact_type: $artifact_type
    }
    let opts = (
        [[key value]; [name $name] [content_type $content_type]]
        | where value != ""
        | transpose --header-row --as-record
    )
    emit "artifact.produced" ($base | merge $opts)
}

def emit-dependency-resolved [
    exec_id: string
    artifact_id: string
    --name: string = ""
    --version: string = ""
    --role: string = ""
] {
    let base = {
        execution_id: $exec_id
        artifact_id: $artifact_id
        artifact_type: "dependency"
    }
    let opts = (
        [[key value]; [name $name] [version $version] [role $role]]
        | where value != ""
        | transpose --header-row --as-record
    )
    emit "dependency.resolved" ($base | merge $opts)
}

def emit-source-identified [
    exec_id: string
    artifact_id: string
    git_commit: string
] {
    emit "source.identified" {
        execution_id: $exec_id
        artifact_id: $artifact_id
        git_commit: $git_commit
    }
}

# ---------------------------------------------------------------------------
# Input resolution (per-stage)
# ---------------------------------------------------------------------------

# Resolve a stage's input descriptor against the pinned top-level inputs,
# the artifact directory, and prior stage outputs. Emits provenance events.
# Returns a record with the resolved artifact_id.
def resolve-stage-input [
    exec_id: string
    input_desc: record
    pinned_inputs: record
    working_dir: string
    artifact_dir: string
    stage_outputs: record   # { stage_name: { artifact_name: { hash, path } } }
]: nothing -> record {
    if ($input_desc.ref? | default "") != "" {
        # This is a reference to a top-level pinned input.
        let ref_name = $input_desc.ref
        let input_data = ($pinned_inputs | get -i $ref_name)
        if $input_data == null {
            log-warn $"input ref '($ref_name)' not found in pinned inputs" --component "resolve"
            return { artifact_id: "", type: "ref", ref: $ref_name }
        }

        let pin = ($input_data.pin? | default "")
        if $pin == "" or $pin == null {
            log-warn $"input ref '($ref_name)' has no pin" --component "resolve"
            return { artifact_id: "", type: "ref", ref: $ref_name }
        }

        # Emit appropriate provenance based on input type.
        match $input_data.type {
            "git-tree" => {
                emit-dependency-resolved $exec_id $pin --name "source" --role "source"
                let commit = ($input_data.git_commit? | default "")
                if $commit != "" {
                    emit-source-identified $exec_id $pin $commit
                }
            }
            "file" => {
                emit-dependency-resolved $exec_id $pin --name $ref_name --role "lockfile"
            }
            "nix-closure" => {
                emit-dependency-resolved $exec_id $pin --name "toolchain" --role "toolchain"
            }
        }

        { artifact_id: $pin, type: "ref", ref: $ref_name }

    } else if ($input_desc.type? | default "") == "stage-output" {
        # Reference to a prior stage's output artifact.
        let src_stage = $input_desc.stage
        let src_artifact = $input_desc.artifact
        let stage_data = ($stage_outputs | get -i $src_stage)

        if $stage_data == null {
            log-warn $"stage-output ref: stage '($src_stage)' has no recorded outputs" --component "resolve"
            return { artifact_id: "", type: "stage-output", stage: $src_stage, artifact: $src_artifact }
        }

        let artifact_data = ($stage_data | get -i $src_artifact)
        if $artifact_data == null {
            log-warn $"stage-output ref: artifact '($src_artifact)' not found in stage '($src_stage)'" --component "resolve"
            return { artifact_id: "", type: "stage-output", stage: $src_stage, artifact: $src_artifact }
        }

        emit-dependency-resolved $exec_id $artifact_data.hash --name $src_artifact --role "stage-output"
        { artifact_id: $artifact_data.hash, type: "stage-output", stage: $src_stage, artifact: $src_artifact }

    } else if ($input_desc.type? | default "") == "environment" {
        # Environment inputs are not content-addressable. Logged but not hashed.
        log-debug $"environment input '($input_desc.name? | default '?')' — not pinnable" --component "resolve"
        { artifact_id: "", type: "environment", name: ($input_desc.name? | default "") }

    } else {
        log-warn $"unknown input descriptor: ($input_desc | to json --raw)" --component "resolve"
        { artifact_id: "", type: "unknown" }
    }
}

# ---------------------------------------------------------------------------
# Output resolution (per-stage)
# ---------------------------------------------------------------------------

# Resolve a stage's output descriptor. Hashes produced artifacts, emits
# provenance events. Returns a record with the artifact_id and path.
def resolve-stage-output [
    exec_id: string
    output_desc: record
    artifact_dir: string
]: nothing -> record {
    match ($output_desc.type? | default "none") {
        "artifact" => {
            let art_name = ($output_desc.name? | default "")
            let art_path = ($artifact_dir | path join $art_name)
            if ($art_path | path exists) {
                log-debug $"located artifact at ($art_path)"
                let art_hash = (content-hash $art_path)
                let ct = ($output_desc.content_type? | default "")
                emit-artifact-produced $exec_id $art_hash "artifact" --name $art_name --content_type $ct
                { artifact_id: $art_hash, type: "artifact", name: $art_name, path: $art_path }
            } else {
                log-warn $"output artifact '($art_name)' not found at ($art_path)" --component "resolve"
                { artifact_id: "", type: "artifact", name: $art_name, path: $art_path }
            }
        }
        "assertion" => {
            # Assertions are pass/fail gates. No artifact to hash.
            # The stage exit code is the assertion result.
            { artifact_id: "", type: "assertion", name: ($output_desc.name? | default "") }
        }
        "report" => {
            { artifact_id: "", type: "report", name: ($output_desc.name? | default "") }
        }
        "none" => {
            { artifact_id: "", type: "none" }
        }
        _ => {
            log-warn $"unknown output type '($output_desc.type)'" --component "resolve"
            { artifact_id: "", type: "unknown" }
        }
    }
}

# ---------------------------------------------------------------------------
# Stage execution
# ---------------------------------------------------------------------------

def execute-stage [
    stage: record
    stage_index: int
    pipeline_exec_id: string
    working_dir: string
    artifact_dir: string
    prior_exit: int
    pinned_inputs: record
    stage_outputs: record
]: nothing -> record {
    let stage_name = $stage.name
    let stage_command = $stage.command
    let failure_mode = ($stage.failureMode? | default "fail")
    let condition = ($stage.condition? | default "always")
    let is_pure = ($stage.pure? | default false)

    log-info $"=== Stage ($stage_index): ($stage_name) ===" --component "stage"
    if not $is_pure {
        let reason = ($stage.impurity_reason? | default "unspecified")
        log-debug $"impure stage: ($reason)" --component "stage"
    }

    # Condition gate.
    let should_skip = match $condition {
        "always" => false
        "previous_success" => ($prior_exit != 0)
        _ => {
            let val = ($env | get --optional $condition | default "")
            ($val | is-empty)
        }
    }

    if $should_skip {
        let reason = if $condition == "previous_success" {
            "prior stage failed"
        } else {
            $"condition env var '($condition)' not set"
        }
        log-info $"skipping '($stage_name)' — ($reason)" --component "stage"
        return {
            exit_code: $prior_exit
            skipped: true
            stage_name: $stage_name
            produced: {}
            pure: $is_pure
        }
    }

    let stage_exec_id = (random uuid)
    emit-build-observed $stage_exec_id $pipeline_exec_id $stage_command $working_dir

    # Resolve inputs.
    let consumed = ($stage.inputs | each {|input|
        resolve-stage-input $stage_exec_id $input $pinned_inputs $working_dir $artifact_dir $stage_outputs
    })

    # Execute.
    let stage_log = ($artifact_dir | path join $"($stage_name).log")
    let start_time = (date now)

    #Execute stage command
    let result = try {
        cd $working_dir
        log-debug $"($stage_command)"
        bash -c $"set -o pipefail; ($stage_command) 2>&1 | tee ($stage_log)" | complete
    } catch {|e|
        $"Execution error: ($e.msg)\n" | save -f $stage_log
        { exit_code: 1, stdout: "", stderr: $"Execution error: ($e.msg)" }
    }

        let d = (((date now) - $start_time) / 1_000_000)
        let duration_ms = ($d | into int)
    let stage_exit = $result.exit_code

    $"($stage_exit)" | save -f ($artifact_dir | path join $"($stage_name).exit")
    log-info $"stage '($stage_name)' exited ($stage_exit) (($duration_ms)ms)" --component "stage"

    # Resolve outputs — only on success.
    mut produced = {}
    if $stage_exit == 0 {
        for output in $stage.outputs {
            let resolved = (resolve-stage-output $stage_exec_id $output $artifact_dir)
            if $resolved.artifact_id != "" {
                let name = ($resolved.name? | default "")
                if $name != "" {
                    $produced = ($produced | merge { ($name): { hash: $resolved.artifact_id, path: ($resolved.path? | default "") } })
                }
            }
        }
    }

    emit-build-completed $stage_exec_id $stage_exit $duration_ms

    # Failure mode.
    let effective_exit = match $failure_mode {
        "fail-fast" => $stage_exit
        "fail" => $stage_exit
        "collect" => {
            if $stage_exit != 0 {
                log-warn $"stage '($stage_name)' failed — continuing failureMode=collect" --component "stage"
            }
            0
        }
        "warn" => {
            if $stage_exit != 0 {
                log-warn $"stage '($stage_name)' failed — continuing failureMode=warn" --component "stage"
            }
            0
        }
        "ignore" => 0
        _ => $stage_exit
    }

    {
        exit_code: $effective_exit
        skipped: false
        stage_name: $stage_name
        produced: $produced
        pure: $is_pure
    }
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

# Usage:
#   nu pipeline_runner.nu                              # defaults
#   nu pipeline_runner.nu ./manifest.json              # custom manifest
#   nu pipeline_runner.nu ./manifest.json test-unit    # single stage
def main [manifest_path?: string, target_stage?: string] {
    let manifest_file = ($manifest_path
        | default ($env.PIPELINE_MANIFEST? | default $MANIFEST_PATH))
    let target = ($target_stage
        | default ($env.PIPELINE_TARGET? | default "all"))

    let manifest = (open $manifest_file)

    let pipeline_name = $manifest.name
    let artifact_dir = $manifest.artifactDir
    let working_dir = $manifest.workingDir
    let stages = $manifest.stages

    mkdir $artifact_dir

    log-info $"starting pipeline '($pipeline_name)' build_id: ($env.POLAR_BUILD_ID? | default 'local')"
    log-info $"manifest: ($manifest_file)"
    log-info $"target:   ($target)"
    log-info $"stages:   ($stages | length)"

    # ── Pin inputs ──────────────────────────────────────────────────────────
    # Compute content hashes for all declared inputs. This is the derivation
    # step: after pinning, the manifest fully specifies the build.
    let pinned_inputs = if ($manifest.inputs? | default null) != null {
        log-info "pinning declared inputs" --component "pin"
        pin-inputs $manifest.inputs $working_dir
    } else {
        log-debug "no inputs section in manifest — legacy mode" --component "pin"
        {}
    }

    # ── Pipeline execution envelope ─────────────────────────────────────────
    let pipeline_exec_id = (random uuid)
    let pipeline_start = (date now)
    emit-build-observed $pipeline_exec_id "" $"pipeline:($pipeline_name)" $working_dir

    # ── Filter stages ───────────────────────────────────────────────────────
    let active_stages = if $target == "all" {
        $stages
    } else {
        let matched = ($stages | where name == $target)
        if ($matched | is-empty) {
            log-error $"no stage named '($target)' — available: ($stages | get name | str join ', ')"
            exit 1
        }
        $matched
    }

    # ── Stage loop ──────────────────────────────────────────────────────────
    # Fold over stages, threading exit code and accumulated stage outputs.
    # stage_outputs is a record keyed by stage name, containing each stage's
    # produced artifacts — this is how stage-output input refs resolve.
    let final_state = try {
        $active_stages
        | enumerate
        | reduce --fold { exit_code: 0, results: [], stage_outputs: {} } {|entry, acc|
            let result = (
                execute-stage
                    $entry.item
                    $entry.index
                    $pipeline_exec_id
                    $working_dir
                    $artifact_dir
                    $acc.exit_code
                    $pinned_inputs
                    $acc.stage_outputs
            )

            let new_exit = if $result.exit_code != 0 and $acc.exit_code == 0 {
                $result.exit_code
            } else {
                $acc.exit_code
            }

            # Register this stage's produced artifacts for downstream stage-output refs.
            let updated_outputs = if ($result.produced | columns | length) > 0 {
                $acc.stage_outputs | merge { ($result.stage_name): $result.produced }
            } else {
                $acc.stage_outputs
            }

            {
                exit_code: $new_exit
                results: ($acc.results | append $result)
                stage_outputs: $updated_outputs
            }
        }
    } catch {|e|
        log-error $"pipeline loop failed: ($e.msg)"
        { exit_code: 1, results: [], stage_outputs: {} }
    }

    let d = (((date now) - $pipeline_start) / 1_000_000)
    let pipeline_duration_ms = ($d | into int)
    emit-build-completed $pipeline_exec_id $final_state.exit_code $pipeline_duration_ms

    # ── Write pinned manifest ───────────────────────────────────────────────
    # The pinned manifest is the build derivation: the original manifest with
    # all input pins populated. A verifier can use this to reproduce the build.
    let pinned_manifest = ($manifest | merge { inputs: $pinned_inputs })
    $pinned_manifest | to json --indent 2 | save -f ($artifact_dir | path join "pipeline-manifest.pinned.json")
    log-info $"wrote pinned manifest to ($artifact_dir)/pipeline-manifest.pinned.json" --component "pin"

    # ── Summary ─────────────────────────────────────────────────────────────
    let passed  = ($final_state.results | where {|r| (not $r.skipped) and $r.exit_code == 0 })
    let failed  = ($final_state.results | where {|r| (not $r.skipped) and $r.exit_code != 0 })
    let skipped = ($final_state.results | where skipped)
    let pure_count = ($final_state.results | where {|r| (not $r.skipped) and $r.pure } | length)
    let impure_count = ($final_state.results | where {|r| (not $r.skipped) and (not $r.pure) } | length)

    let result_str = if $final_state.exit_code == 0 { "pass" } else { "fail" }

    # Machine-readable summary.
    let summary = {
        schema: "polar.pipeline.summary/v1"
        pipeline: $pipeline_name
        build_id: ($env.POLAR_BUILD_ID? | default "local")
        target: $target
        duration_ms: $pipeline_duration_ms
        result: $result_str
        passed:  ($passed  | get stage_name)
        failed:  ($failed  | get stage_name)
        skipped: ($skipped | get stage_name)
        pure_stages: $pure_count
        impure_stages: $impure_count
        input_pins: ($pinned_inputs | columns | each {|col|
            let input = ($pinned_inputs | get $col)
            { name: $col, type: $input.type, pin: ($input.pin? | default null) }
        })
        stage_outputs: $final_state.stage_outputs
    }
    $summary | to json --indent 2 | save -f ($artifact_dir | path join "summary.json")

    # Human-readable summary.
    let summary_lines = [
        $"Pipeline: ($pipeline_name)"
        $"Build ID: ($env.POLAR_BUILD_ID? | default 'local')"
        $"Target:   ($target)"
        $"Duration: ($pipeline_duration_ms)ms"
        $"Result:   ($result_str | str upcase)"
        ""
        $"Passed  \(($passed | length)\):  ($passed | get stage_name | str join ', ' | default 'none')"
        $"Failed  \(($failed | length)\):  ($failed | get stage_name | str join ', ' | default 'none')"
        $"Skipped \(($skipped | length)\): ($skipped | get stage_name | str join ', ' | default 'none')"
        ""
        $"Pure stages:   ($pure_count)"
        $"Impure stages: ($impure_count)"
        ""
        "Input pins:"
        ...($pinned_inputs | columns | each {|col|
            let input = ($pinned_inputs | get $col)
            $"  ($col): ($input.pin? | default 'unpinned')"
        })
    ]
    $summary_lines | str join "\n" | save -f ($artifact_dir | path join "summary.txt")

    # Console.
    log-info "════════════════════════════════════════════════════"
    log-info $"pipeline '($pipeline_name)' complete in ($pipeline_duration_ms)ms"
    log-info $"  passed:  ($passed | length)"
    log-info $"  failed:  ($failed | length)"
    log-info $"  skipped: ($skipped | length)"
    log-info $"  pure/impure: ($pure_count)/($impure_count)"
    log-info $"  result:  ($result_str | str upcase)"
    log-info "════════════════════════════════════════════════════"

    exit $final_state.exit_code
}
