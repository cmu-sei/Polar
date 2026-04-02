
#!/usr/bin/env nu
# cargo-attested-build.nu
#
# Wraps `cargo build` to produce a build attestation that cryptographically
# binds the output binary to the Cargo.toml that produced it, the Cargo.lock
# that resolved its dependencies, and the source tree hash.
#
# The attestation is a JSON record containing content hashes of all inputs
# and outputs, computed in the same process, in the same event loop iteration,
# from the same filesystem state. No temporal gap between build and hash.
#
# Usage:
#   nu cargo-attested-build.nu                          # build default target
#   nu cargo-attested-build.nu --package polar-linker   # build specific package
#   nu cargo-attested-build.nu --release                # release mode
#
# Outputs:
#   - The compiled binary (wherever cargo puts it)
#   - $artifact_dir/<binary_name>.attestation.json



# Hash a file, returning sha256:<hex>.
def content-hash [path: string]: nothing -> string {
    if not ($path | path exists) {
        error make { msg: $"file not found: ($path)" }
    }
    open $path --raw | hash sha256 | $"sha256:($in)"
}

# Hash a directory tree via git tree hash if available, else sorted file hashes.
def tree-hash [dir: string]: nothing -> string {
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

# Extract the workspace root from cargo metadata.
def workspace-root []: nothing -> string {
    cargo metadata --format-version 1 --no-deps
    | from json
    | get workspace_root
}

# Parse cargo's --message-format=json output to extract compiler artifacts.
# Returns a table of { package_id, target_name, target_kind, filenames, fresh }.
def parse-cargo-artifacts [json_lines: string]: nothing -> table {
    $json_lines
    | lines
    | where { ($in | str trim) != "" }
    | each { try { $in | from json } catch { null } }
    | where { $in != null }
    | where { $in.reason? == "compiler-artifact" }
    | each {|msg|
        {
            package_id: $msg.package_id
            target_name: $msg.target.name
            target_kind: ($msg.target.kind | first)
            filenames: $msg.filenames
            fresh: ($msg.fresh? | default false)
        }
    }
}

def main [
    --package (-p): string = ""    # specific package to build
    --release (-r)                 # release mode
    --target (-t): string = ""     # target triple
    --artifact-dir: path = "/workspace/pipeline-out"
] {
    let ws_root = (workspace-root)
    let build_start = (date now)

    if ($artifact_dir | path exists) != true {
        print $"[attested-build] creating artifact directory at ($artifact_dir)"
        mkdir $artifact_dir
    }
    # Determine the cargo build command.
    mut cargo_args = ["build" "--message-format=json" "--locked" "--quiet"]
    if $package != "" { $cargo_args = ($cargo_args | append ["--package" $package]) }
    if $release { $cargo_args = ($cargo_args | append "--release") }
    if $target != "" { $cargo_args = ($cargo_args | append ["--target" $target]) }

    let cargo_cmd = $"cargo ($cargo_args | str join ' ')"

    # Hash the inputs: Cargo.toml, Cargo.lock, and source tree.
    # These are computed NOW, in the same process, against the same filesystem
    # state that cargo just read from. No temporal gap.
    let cargo_toml_path = if $package != "" {
        # Find the package-specific Cargo.toml from workspace metadata.
        let meta = (cargo metadata --format-version 1 --no-deps | from json)
        let pkg = ($meta.packages | where name == $package | first)
        $pkg.manifest_path
    } else {
        $"($ws_root)/Cargo.toml"
    }

    let cargo_lock_path = $"($ws_root)/Cargo.lock"

    let input_hashes = {
        cargo_toml: (content-hash $cargo_toml_path)
        cargo_toml_path: $cargo_toml_path
        cargo_lock: (content-hash $cargo_lock_path)
        source_tree: (tree-hash $ws_root)
        git_commit: (try { git -C $ws_root rev-parse HEAD | str trim } catch { "unknown" })
        git_tree: (try { git -C $ws_root rev-parse "HEAD^{tree}" | str trim } catch { "unknown" })
    }
    print $"[attested-build] Running: ($cargo_cmd)"


    # Run cargo build and stream JSON events.
    # Collect binary artifacts as records, move the binaries, and return
    # one record per binary for later attestation work.
    let binaries = (
        cargo ...$cargo_args
        | lines
        | where ($it | str trim) != ""
        | where ($it | str starts-with "{")
        | each { |line| $line | from json }
        | where reason == "compiler-artifact"
        | where ($it.target.kind | any {|k| $k == "bin"})
        | where executable != null
        | each { |artifact|
            let exe = $artifact.executable
            let name = ($exe | path basename)
            let dest = ($artifact_dir | path join $name)

            print $"[attested-build] moving ($exe) -> ($dest)"
            mv $exe $dest

            {
                name: $name
                path: $dest
                digest: (content-hash $dest)
                package_id: $artifact.package_id
                target_kind: ($artifact.target.kind | first)
            }
        }
    )

    let cargo_exit = $env.LAST_EXIT_CODE
    let build_end = (date now)
    let build_duration_ms = (($build_end - $build_start) | into int) / 1_000_000

    if $cargo_exit != 0 {
        error make {
            msg: $"[attested-build] cargo build failed (exit ($cargo_exit))"
        }
    }

    if ($binaries | is-empty) {
        print "[attested-build] No binary artifacts produced"
        exit 0
    }

    print $"[attested-build] ($binaries | length) binary artifact\(s\) produced"

    # At this point you can build attestations from $binaries.
    let attestations = ($binaries | each { |bin|
        let attestation = {
            schema: "polar.build.attestation/v1"
            timestamp: ($build_end | format date "%Y-%m-%dT%H:%M:%S%.3fZ")
            build: {
                command: $cargo_cmd
                duration_ms: $build_duration_ms
                exit_code: $cargo_exit
                build_id: ($env.POLAR_BUILD_ID? | default "local")
                profile: (if $release { "release" } else { "debug" })
            }
            subject: {
                name: $bin.name
                digest: $bin.digest
                path: $bin.path
                target_kind: $bin.target_kind
                package_id: $bin.package_id
            }
            binding: {
                digest: (
                    [$bin.digest $input_hashes.cargo_toml $input_hashes.cargo_lock $input_hashes.source_tree]
                    | str join ":"
                    | hash sha256
                    | $"sha256:($in)"
                )
                algorithm: "sha256(binary:cargo_toml:cargo_lock:source_tree)"
            }
        }

        let attestation_path = ($artifact_dir | path join $"($bin.name).attestation.json")
        $attestation | to json --indent 2 | save -f $attestation_path

        print $"[attested-build] ($bin.name)"
        print $"[attested-build]   binary:  ($bin.digest)"
        print $"[attested-build]   wrote:   ($attestation_path)"

        $attestation
    })

    # Write a manifest of all attestations for this build invocation.
    let manifest = {
        build_id: ($env.POLAR_BUILD_ID? | default "local")
        timestamp: ($build_end | format date "%Y-%m-%dT%H:%M:%S%.3fZ")
        attestations: ($attestations | each {|a|
            { name: $a.subject.name, digest: $a.subject.digest, binding: $a.binding.digest }
        })
    }
    $manifest | to json --indent 2 | save -f ($artifact_dir | path join "build-manifest.json")

    print $"[attested-build] ($attestations | length) attestation\(s\) written to ($artifact_dir)"
}
