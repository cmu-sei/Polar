#!/usr/bin/env nu
use ./pipeline/core.nu *

# nix-attested-build.nu
#
# Wraps `nix build` for a crane-based flake to produce build attestations.
# Taps --log-format internal-json to emit provenance events per-derivation
# as Nix realizes them, not post-hoc.
#
# Because Nix content-addresses store paths, the store path IS the artifact ID.
# flake.lock IS the input pin. The .drv IS the build recipe.
# We don't recompute what Nix already computed — we just surface it.
#
# Usage:
#   nu nix-attested-build.nu                              # build all flake outputs
#   nu nix-attested-build.nu --attr packages.x86_64-linux.my-package
#   nu nix-attested-build.nu --attr checks.x86_64-linux.all

const NIX_EVENT_TYPE_BUILD = 105  # derivation build activity

def attest-cargo-deps-from-store [
    deps_store_path: string
    exec_id: string
]: nothing -> list<record> {
    # Crane vendors deps into $out/vendor/<crate-name>-<version>/
    # Each directory is one resolved crate. The checksum Nix verified
    # during the fixed-output derivation IS the cargo checksum.
    let vendor_dir = ($deps_store_path | path join "vendor")
    if not ($vendor_dir | path exists) { return [] }

    glob $"($vendor_dir)/*"
    | where { ($in | path type) == "dir" }
    | each {|crate_dir|
        let name = ($crate_dir | path basename)
        # Parse crate name and version from directory name: tokio-1.28.0 -> {name: tokio, version: 1.28.0}
        let parts = ($name | parse --regex '^(?P<crate>.+)-(?P<version>\d+\.\d+.*)$' | first)
        let checksum_file = ($crate_dir | path join ".cargo-checksum.json")
        let checksum = if ($checksum_file | path exists) {
            open $checksum_file | from json | get -o package | default ""
        } else { "" }

        let artifact_id = if ($checksum | is-not-empty) {
            $"cargo:($checksum)"
        } else {
            # Fall back to hashing the crate directory contents
            content-hash-dir $crate_dir
        }

        emit-dependency-resolved $artifact_id --name $parts.crate --version $parts.version --role "cargo-dependency"

        { name: $parts.crate, version: $parts.version, artifact_id: $artifact_id }
    }
}

# Find the crane deps derivation store path from the binary's store path.
# nix why-depends walks the dependency graph — the deps drv is always
# a direct input to the final binary derivation.
def find-crane-deps-store-path [binary_store_path: string]: nothing -> string {
    let deps = (
        nix path-info --json --recursive $binary_store_path
        | from json
        | transpose key value
        | where { $in.key | str contains "-deps-" }
        | first
    )
    if $deps == null { return "" }
    $deps.key
}

def parse-nix-events [events_file: path]: nothing -> list<record> {
    if not ($events_file | path exists) { return [] }

    open $events_file
    | lines
    | where { ($in | str trim) != "" }
    | each { |line|
        try { $line | from json } catch { null }
    }
    | where { $in != null and ($in | describe | str starts-with "record") }

}
# Extract the package name from a store path or drv path.
# /nix/store/abc123-my-package-1.0.0       -> my-package-1.0.0
# /nix/store/abc123-my-package-1.0.0.drv   -> my-package-1.0.0
def store-path-name [path: string]: nothing -> string {
    $path
    | path basename
    | str replace --regex '^[a-z0-9]+-' ''  # strip the hash prefix
    | str replace --regex '\.drv$' ''
}

# Nix store paths are already content-addressed. The hash segment IS the
# content address — we just format it consistently with our other artifact IDs.
def store-path-to-artifact-id [path: string]: nothing -> string {
    let hash = ($path | path basename | split row '-' | first)
    $"nix:($hash)"
}

# Query the flake lock to get pinned input hashes — this is the input
# attestation equivalent of hashing Cargo.toml + Cargo.lock.
def pin-flake-inputs [flake_dir: string]: nothing -> record {
    let lock_path = ($flake_dir | path join "flake.lock")
    if not ($lock_path | path exists) {
        error make { msg: $"flake.lock not found at ($lock_path)" }
    }

    let lock = (open $lock_path | from json)
    let lock_hash = (content-hash-file $lock_path)

    let input_pins = (
        $lock.nodes
        | transpose key value
        | where { ($in.value.locked? | default null) != null }
        | each {|node|
            {
                name: $node.key
                nar_hash: ($node.value.locked.narHash? | default "")
                rev: ($node.value.locked.rev? | default "")
                url: ($node.value.locked.url? | default "")
                type: ($node.value.locked.type? | default "")
            }
        }
    )

    {
        lock_hash: $lock_hash
        lock_path: $lock_path
        inputs: $input_pins
    }
}

# Get the .drv path for a flake output attribute — this is the build recipe,
# the closest equivalent to Cargo.toml for a single package.
def get-drv-path [flake_ref: string]: nothing -> string {
    let result = (nix path-info --derivation --json $flake_ref | from json)
    $result | columns | first
}

# Copy built outputs from the Nix store to the artifact directory.
# We don't move them — store paths are immutable and shared.
# We create symlinks or copy binaries depending on what downstream needs.
def collect-outputs [
    store_paths: list<string>
    artifact_dir: path
    copy_binaries: bool
]: nothing -> list<record> {
    $store_paths | each {|store_path|
        let name = (store-path-name $store_path)
        let artifact_id = (store-path-to-artifact-id $store_path)

        # Find executables within the store path.
        let bins = (
            try {
                glob $"($store_path)/bin/*"
                | where { ($in | path type) == "file" }
            } catch { [] }
        )

        if $copy_binaries and ($bins | is-not-empty) {
            for bin in $bins {
                let bin_name = ($bin | path basename)
                let dest = ($artifact_dir | path join $bin_name)
                cp $bin $dest
                log-info $"copied ($bin_name) -> ($dest)" --component "nix-build"
            }
        }

        emit-artifact-produced $artifact_id "nix-store-path" --name $name --content_type "nix-derivation-output"

        {
            name: $name
            store_path: $store_path
            artifact_id: $artifact_id
            binaries: ($bins | each { path basename })
        }
    }
}
def main [
    ...attrs: string               # flake output attrs, e.g. packages.x86_64-linux.foo
                                   # if empty, builds the flake default (equivalent to nix build .)
    --artifact-dir: path = "/workspace/pipeline-out"
    --flake-dir: path = "/workspace"
    --copy-binaries                # copy bin/* outputs to artifact-dir
    --system: string = ""          # e.g. x86_64-linux — used to expand shorthand attrs
] {
    let build_start = (date now)
    mkdir $artifact_dir

    # Resolve flake refs. If attrs were given, qualify each one against the
    # flake dir. If none were given, build the flake default — same as `nix build .`
    let flake_refs = if ($attrs | is-empty) {
        [$flake_dir]
    } else {
        $attrs | each {|attr|
            # If the attr already looks like a full flake ref (contains #), use it as-is.
            # Otherwise qualify it against the flake dir.
            if ($attr | str contains "#") { $attr } else { $"($flake_dir)#($attr)" }
        }
    }

    log-info $"building ($flake_refs | length) output\(s\)" --component "nix-build"
    for ref in $flake_refs { log-info $"  ($ref)" --component "nix-build" }

    # Pin inputs before building — no temporal gap.
    let pinned_inputs = (pin-flake-inputs $flake_dir)
    for input in $pinned_inputs.inputs {
        if ($input.nar_hash | is-not-empty) {
            emit-dependency-resolved $input.nar_hash --name $input.name --role "flake-input"
        }
    }

    let events_file = ($artifact_dir | path join "nix-build-events.json")

    # Build all refs in a single nix build invocation — same as your justfile.
    # Nix will share the evaluation and build cache across all of them.
    let refs_str = ($flake_refs | str join " ")
    let events_file_str = ($events_file | into string)

    let result = (
        bash -c $"set -o pipefail; nix build --log-format internal-json --no-link --json ($refs_str) 2>($events_file_str)" | complete
    )
    let duration_ms = (elapsed-ms $build_start)

    if $result.exit_code != 0 {
        error make { msg: $"nix build failed (exit ($result.exit_code))" }
    }

    # Process the event stream for per-derivation completion events.
    # nix build evaluates the entire dependency graph and builds derivations
    # in parallel — these events fire as each one completes, not at the end.
    let build_events = (parse-nix-events $events_file)

    let completed_drvs = (
        $build_events
        | where { ($in | get -o action) == "result" and ($in | get -o type) == $NIX_EVENT_TYPE_BUILD }
        | where { ($in | get -o fields | default []) | is-not-empty }
        | each {|ev|
            let store_path = ($ev.fields | first)
            let artifact_id = (store-path-to-artifact-id $store_path)
            log-info $"realized: ($store_path)" --component "nix-build"
            emit-artifact-produced $artifact_id "nix-store-path" --name (store-path-name $store_path)
            { store_path: $store_path, artifact_id: $artifact_id }
        }
    )

    # nix build --json gives the top-level requested outputs as structured data.
    # For multi-attr builds this is a list with one entry per requested ref,
    # each containing its own outputs record.
    let built_outputs = try { $result.stdout | from json } catch { [] }

    # Flatten all outputs across all requested attrs into a single list.
    # Each entry in built_outputs is { drvPath, outputs: { out: "/nix/store/..." } }
    let store_paths = (
        $built_outputs
        | each {|out| $out.outputs? | default {} | values }
        | flatten
        | uniq
    )

    let artifacts = (collect-outputs $store_paths $artifact_dir $copy_binaries)

    for artifact in $artifacts {

        let deps_path = (find-crane-deps-store-path $artifact.store_path)
        if ($deps_path | is-not-empty) {
            log-info $"attesting cargo deps from ($deps_path)" --component "nix-build"
            let cargo_deps = (attest-cargo-deps-from-store $deps_path "some-id")
            log-info $"  ($cargo_deps | length) crates attested" --component "nix-build"
        }
    }

    let attestation = {
        schema: "polar.build.attestation/v1"
        builder: "nix"
        timestamp: (date now | format date "%Y-%m-%dT%H:%M:%S%.3fZ")
        build: {
            flake_refs: $flake_refs
            duration_ms: $duration_ms
            exit_code: $result.exit_code
            build_id: ($env.POLAR_BUILD_ID? | default "local")
            realized_derivations: ($completed_drvs | length)
        }
        inputs: {
            flake_lock_hash: $pinned_inputs.lock_hash
            flake_lock_path: ($pinned_inputs.lock_path | into string)
            pinned_inputs: $pinned_inputs.inputs
        }
        outputs: $artifacts
        nix_self_attested: true
    }

    $attestation | to json --indent 2 | save -f ($artifact_dir | path join "nix-build.attestation.json")

    let manifest = {
        build_id: ($env.POLAR_BUILD_ID? | default "local")
        timestamp: (date now | format date "%Y-%m-%dT%H:%M:%S%.3fZ")
        artifacts: ($artifacts | each {|a|
            { name: $a.name, artifact_id: $a.artifact_id, store_path: $a.store_path }
        })
    }
    $manifest | to json --indent 2 | save -f ($artifact_dir | path join "nix-build-manifest.json")

    log-info $"($artifacts | length) top-level output\(s\), ($completed_drvs | length) derivations realized" --component "nix-build"
    log-info $"flake.lock: ($pinned_inputs.lock_hash)" --component "nix-build"
}
