
#!/usr/bin/env nu
# ci.nu — unified Polar build pipeline
#
# Runs the full build sequence under a single Cassini daemon:
#
#   Phase 0 — Static analysis (no Cassini, no artifacts)
#     Parallel: cargo deny, clippy, udeps, semver-checks, geiger, hack
#     Sequential: noseyparker (manages its own datastore), cargo mutants
#     (spawns its own workers). Failures are reported; --strict-analysis
#     aborts the pipeline on any non-zero exit.
#
#   Phase 1 — Cargo SBOMs
#     cargo-cyclonedx generates a CycloneDX SBOM per workspace member,
#     each is parsed into a graph fragment and emitted as sbom_analyzed.
#
#   Phase 2 — Cargo binaries
#     cargo build --message-format=json produces ELF artifacts; each binary
#     is linked back to its Phase 1 SBOM via a cryptographic binding digest.
#     sbom_lookup flows in-process from Phase 1 — no file-based IPC needed.
#
#   Phase 3 — OCI images
#     nix build → skopeo upload → cosign sign, one pass per image in the
#     manifest. Source-level SBOM hashes from Phase 1 are threaded into
#     binary_linked events, closing the full provenance chain:
#       (Binary)-[:BUILT_FROM]->(Package)<-[:DESCRIBES]-(SourceSbom)
#
# All phases share one artifact directory. Phases 1-3 share one Cassini daemon.
# Adding a new image = one row in image-manifest. Nothing else changes.

use ./core/state.nu [init-pipeline-state get-build-id]
use ./core/oci.nu *
use ./core/cassini.nu *
use ./core/logging.nu *
use ./core/events.nu *
use ./core/cargo.nu workspace-root
use ./core/hashing.nu *
use ./core/sbom.nu *
const COMPONENT = "ci"
const ELF_BINARY_ARTIFACT = "elf-binary"


# ===========================================================================
# Phase 0: Static analysis
# ===========================================================================

def check-tools [tools: list<string>] {
    let missing = $tools | where {|t| (which $t | is-empty) }
    if ($missing | is-not-empty) {
        log-error $"missing required tools: ($missing | str join ', ')" --component $COMPONENT
        error make {msg: "preflight tool check failed"}
    }
}

def run-tool [name: string, outfile: string, cmd: list<string>]: nothing -> record {
    let start = (date now)
    log-info (yellow $"  → ($name)...") --component $COMPONENT

    let bin = $cmd.0
    let args = $cmd | skip 1

    let exit_code = (do --ignore-errors {
        ^$bin ...$args out+err> $outfile
        0
    } | if ($in | is-empty) { 1 } else { $in | into int })

    let duration = ((date now) - $start) / 1sec | math round

    {
        name: $name
        outfile: $outfile
        exit_code: $exit_code
        duration_sec: $duration
    }
}

def static-tool-definitions [manifest: string, output_dir: string]: nothing -> list<record> {
    [
        {
            name: "cargo deny"
            outfile: $"($output_dir)/cargo_deny.txt"
            cmd: [cargo deny --manifest-path $manifest check]
        }
        {
            name: "cargo clippy"
            outfile: $"($output_dir)/cargo_clippy.txt"
            cmd: [
                cargo
                clippy
                --manifest-path
                $manifest
                --all-targets
                --all-features
                --
                -D
                warnings
            ]
        }
        {
            name: "cargo udeps"
            outfile: $"($output_dir)/cargo_udeps.txt"
            cmd: [cargo udeps --manifest-path $manifest --all-targets]
        }
        {
            name: "cargo semver-checks"
            outfile: $"($output_dir)/cargo_semver.txt"
            cmd: [cargo semver-checks --manifest-path $manifest]
        }
        {
            name: "cargo geiger"
            outfile: $"($output_dir)/cargo_geiger.txt"
            cmd: [cargo geiger --manifest-path $manifest]
        }
        {
            name: "cargo hack (feature powerset)"
            outfile: $"($output_dir)/cargo_hack.txt"
            cmd: [
                cargo
                hack
                check
                --manifest-path
                $manifest
                --feature-powerset
            ]
        }
    ]
}

def count-pattern [file: string, pattern: string]: nothing -> int {
    if not ($file | path exists) { return 0 }
    open $file | lines | where {|l| $l =~ $pattern } | length
}

def generate-static-summary [results: table, output_dir: string] {
    let summary_file = $"($output_dir)/static-analysis-summary.txt"
    mut sections = [
        (bold "Polar Static Analysis Summary")
        ""
    ]
    $sections = (
        $sections
        | append [
            (bold "=== Tool Results ===")
            ($results | select name exit_code duration_sec | to text)
            ""
        ]
    )
    let clippy_warnings = (count-pattern $"($output_dir)/cargo_clippy.txt" 'warning\[')
    let clippy_errors = (count-pattern $"($output_dir)/cargo_clippy.txt" 'error\[')
    $sections = (
        $sections
        | append [
            (bold "=== cargo clippy ===")
            $"Warnings: ($clippy_warnings)  Errors: ($clippy_errors)"
            ""
        ]
    )
    let deny_errors = (count-pattern $"($output_dir)/cargo_deny.txt" '^error')
    $sections = (
        $sections
        | append [
            (bold "=== cargo deny ===")
            $"Issues: ($deny_errors)"
            ""
        ]
    )
    let udeps_file = $"($output_dir)/cargo_udeps.txt"
    let udeps_lines = if ($udeps_file | path exists) {
        open $udeps_file | lines | where {|l| $l =~ 'unused|crate' } | first 10
    } else { [] }
    let udeps_body = if ($udeps_lines | is-not-empty) {
        $udeps_lines | str join "\n"
    } else { "No unused dependencies found." }
    $sections = ($sections | append [
        (bold "=== cargo udeps ===")
        $udeps_body
        ""
    ])
    let semver_file = $"($output_dir)/cargo_semver.txt"
    let semver_body = if ($semver_file | path exists) {
        open $semver_file | lines | where {|l| $l =~ 'Parsed|Checked|Summary|semver' } | str join "\n"
    } else { "" }
    $sections = ($sections | append [
        (bold "=== cargo semver-checks ===")
        $semver_body
        ""
    ])
    let geiger_file = $"($output_dir)/cargo_geiger.txt"
    let geiger_body = if ($geiger_file | path exists) {
        open $geiger_file | lines | last 15 | str join "\n"
    } else { "" }
    $sections = ($sections | append [
        (bold "=== cargo geiger ===")
        $geiger_body
        ""
    ])
    let np_report = $"($output_dir)/noseyparker_report.txt"
    let np_body = if ($np_report | path exists) {
        open $np_report | lines | first 30 | str join "\n"
    } else { "" }
    $sections = ($sections | append [
        (bold "=== noseyparker ===")
        $np_body
        ""
    ])
    let mutants_file = $"($output_dir)/cargo_mutants.txt"
    let mutants_body = if ($mutants_file | path exists) {
        open $mutants_file | lines | where {|l| $l =~ 'caught|missed|unviable|timeout|ok' } | last 10 | str join "\n"
    } else { "" }
    $sections = ($sections | append [
        (bold "=== cargo mutants ===")
        $mutants_body
        ""
    ])
    let failed = $results | where exit_code != 0
    if ($failed | is-not-empty) {
        let callout = (
            $failed
            | each {|r| $"  ($r.name) — exit ($r.exit_code) — ($r.outfile)" }
            | str join "\n"
        )
        $sections = (
            $sections
            | append [
                (bold "=== Tools with non-zero exit ===")
                $callout
                ""
            ]
        )
    }
    $sections | flatten | str join "\n" | save --force $summary_file
    log-info (green $"summary written to ($summary_file)") --component $COMPONENT
}

def run-static-analysis [ws_manifest: path, artifact_dir: path]: nothing -> table {
    check-tools [
        cargo
        cargo-deny
        cargo-udeps
        cargo-semver-checks
        cargo-geiger
        cargo-hack
        cargo-mutants
    ]
    log-info (bold "phase 0 — static analysis") --component $COMPONENT
    let parallel_results = (
        static-tool-definitions ($ws_manifest | path expand | into string) ($artifact_dir | into string)
        | par-each {|tool| run-tool $tool.name $tool.outfile $tool.cmd }
    )
    log-info "  → cargo mutants (slow)..." --component $COMPONENT
    let mutants_out = $"($artifact_dir)/cargo_mutants.txt"
    let mutants_out_dir = $"($artifact_dir)/mutants.out"
    let mutants = (run-tool "cargo mutants" $mutants_out [
        cargo mutants --manifest-path ($ws_manifest | into string) --output $mutants_out_dir
    ])
    let all_results = $parallel_results | append $mutants
    log-info ($all_results | select name exit_code duration_sec | table) --component $COMPONENT
    generate-static-summary $all_results ($artifact_dir | into string)
    $all_results
}

# ===========================================================================
# Image manifest — single source of truth for OCI images in this project.
# ===========================================================================

def image-manifest []: nothing -> list<record> {
    [

        {
            name: "cert-issuer"
            flake: ".#certIssuerImage"
            image: "cert-issuer"
            root_purl: "pkg:cargo/cert-issuer@0.1.0"
        }
        {
            name: "cassini"
            flake: ".#cassiniImage"
            image: "cassini"
            root_purl: "pkg:cargo/cassini@0.1.0"
        }
        {
            name: "kube-observer"
            flake: ".#kubeObserverImage"
            image: "kube-observer"
            root_purl: "pkg:cargo/kube-observer@0.1.0"
        }
        {
            name: "kube-consumer"
            flake: ".#kubeConsumerImage"
            image: "kube-consumer"
            root_purl: "pkg:cargo/kube-consumer@0.1.0"
        }
        {
            name: "git-observer"
            flake: ".#gitObserverImage"
            image: "git-observer"
            root_purl: "pkg:cargo/git-repo-observer@0.1.0"
        }
        {
            name: "git-processor"
            flake: ".#gitProcessorImage"
            image: "git-processor"
            root_purl: ""
        }
        {
            name: "build-processor"
            flake: ".#buildProcessorImage"
            image: "build-processor"
            root_purl: "pkg:cargo/build-processor@0.1.0"
        }
        {
            name: "oci-resolver"
            flake: ".#ociResolverImage"
            image: "oci-resolver"
            root_purl: "pkg:cargo/oci-resolver@0.1.0"
        }
    ]
}

# ===========================================================================
# Cargo: package resolution
# ===========================================================================

def resolve-packages [--package(-p): string = ""]: nothing -> list<record> {
    let ws_root = (workspace-root)
    let manifest = $ws_root | path join "Cargo.toml"
    let meta = (
        cargo metadata --manifest-path $manifest --format-version 1 --no-deps
        | from json
    )

    let packages = if ($package | is-empty) {
        let members = $meta.workspace_members | default []
        $meta.packages | where {|pkg| $members | any {|id| $id == $pkg.id } }
    } else {
        $meta.packages | where name == $package
    }

    if ($packages | is-empty) {
        error make {msg: $"no Cargo package matched '($package)'"}
    }

    $packages | each {|pkg| {
        name:          $pkg.name
        manifest_path: ($pkg.manifest_path | path expand)
        version:       ($pkg.version? | default "0.0.0")
        purl:          $"pkg:cargo/($pkg.name)@($pkg.version? | default '0.0.0')"
    }}
}

# ===========================================================================
# Phase 1: SBOM generation and graph projection
# ===========================================================================

def generate-workspace-sboms [
    packages: list<record>
    ws_manifest: path
    artifact_dir: path
    --target(-t): string = ""
]: nothing -> list<record> {
    let ws_root = $ws_manifest | path dirname
    log-info $"generating SBOMs for ($packages | length) package\(s\)" --component $COMPONENT
    mut args = [--manifest-path $ws_manifest -f json]
    if ($target | is-not-empty) { $args = ($args | append [--target $target]) }
    log-info $"running: cargo cyclonedx ($args | str join ' ')" --component $COMPONENT
    let result = (^cargo cyclonedx ...$args | complete)
    if $result.exit_code != 0 {
        let msg = $result.stderr | default $result.stdout | str trim
        log-warn $"cargo-cyclonedx failed: ($msg)" --component $COMPONENT
        return []
    }
    let found = glob $"($ws_root)/**/*.cdx.json" | where { ($in | path type) == "file" }
    log-debug $"found ($found | length) SBOM file\(s\) under ($ws_root)" --component $COMPONENT
    if ($found | is-empty) {
        log-warn "cargo-cyclonedx reported success but produced no *.cdx.json files" --component $COMPONENT
        return []
    }
    for src in $found {
        let dest = $artifact_dir | path join ($src | path basename)
        if ($src | path expand) == ($dest | path expand) { continue }
        mv --force $src $dest
    }
    let collected = (
        ls $artifact_dir
        | where type == file
        | where { ($in.name | path basename | str ends-with ".cdx.json") }
    )
    $collected
}

def process-cargo-sboms [sbom_files: list<record>, packages: list<record>]: nothing -> record {
    mut sbom_lookup = {}

    for f in $sbom_files {
        let doc = try { open $f.name } catch {|e|
            log-warn $"could not parse ($f.name): ($e.msg)" --component $COMPONENT
            continue
        }
        if ($doc.bomFormat? | default "") != "CycloneDX" { continue }

        let filename = $f.name | path basename
        let stem = $filename | str replace ".cdx.json" ""
        let content_hash = (content-hash-file $f.name)

        emit-artifact-produced $content_hash "sbom" --name $filename --content_type "application/vnd.cyclonedx+json"

        let fragment = (extract-graph-fragment $doc $content_hash)

        if $fragment.root != null {
            emit-sbom-analyzed $fragment $filename
            log-info $"($stem): ($fragment.components | length) components, ($fragment.edges | length) edges" --component $COMPONENT
        } else {
            log-warn $"($filename) has no metadata.component — graph fragment not emitted" --component $COMPONENT
        }

        let matched = (
            $packages
            | where name == $stem
            | first
            | default null
        )
        let root_purl = if $fragment.root != null {
            $fragment.root.purl
        } else if $matched != null {
            ($matched.purl? | default "")
        } else {
            ""
        }

        $sbom_lookup = ($sbom_lookup | insert $stem {
            content_hash:    $content_hash
            root_purl:       $root_purl
            component_count: ($fragment.components | length)
            edge_count:      ($fragment.edges | length)
        })
    }

    $sbom_lookup
}

# ===========================================================================
# Phase 2: Binary build and provenance linking
# ===========================================================================

def build-and-link-binaries [
    packages: list<record>
    sbom_lookup: record
    ws_manifest: path
    artifact_dir: path
    --release
    --target: string = ""
    --filter_package: string = ""
]: nothing -> list<record> {
    let ws_root = $ws_manifest | path dirname

    # --manifest-path makes this independent of cwd. The cargo workspace
    # lives at src/agents while flake.nix sits at the repo root — Phase 3's
    # nix build needs to run from the repo root, so this script can't keep
    # assuming "wherever we were invoked from" is also the cargo workspace
    # root. Every other cargo invocation in this file already passes
    # --manifest-path explicitly; this was the one holdout, working only by
    # coincidence of cwd matching src/agents on every run so far.
    mut cargo_args = ["build" "--manifest-path" $ws_manifest "--message-format=json" "--locked" "--quiet"]
    if ($filter_package | is-not-empty) { $cargo_args = ($cargo_args | append ["--package" $filter_package]) }
    if $release { $cargo_args = ($cargo_args | append "--release") }
    if ($target | is-not-empty) { $cargo_args = ($cargo_args | append ["--target" $target]) }

    log-info $"running: cargo ($cargo_args | str join ' ')" --component $COMPONENT

    let cargo_lock_hash = (content-hash-file $"($ws_root)/Cargo.lock")
    let source_tree_hash = (tree-hash $ws_root)

    let binaries = (
        cargo ...$cargo_args
        | lines
        | where { ($in | str trim) != "" }
        | each {|line| try { $line | from json } catch { null } }
        | where { $in != null }
        | where { ($in.reason? | default "") == "compiler-artifact" }
        | where { ($in.target?.kind? | default []) | any {|k| $k == "bin"} }
        | where { ($in.executable? | default null) != null }
        | each {|artifact|
            try {
                let exe         = $artifact.executable
                let name = $exe | path basename
                let dest = $artifact_dir | path join $name
                let binary_hash = (content-hash-file $exe)

                # Cargo "uplifts" unchanged binaries into target/<profile>/<name>
                # via a hardlink to its internal deps-cache artifact, rather than
                # a copy, whenever it doesn't need to recompile. If a previous
                # run already moved this binary into $artifact_dir, the next
                # `cargo build` recreates target/<profile>/<name> the same cheap
                # way — leaving $exe and a leftover $dest as two directory
                # entries for the identical inode. `mv` correctly refuses to
                # "move" a file onto itself in that case (mv-error-same-file).
                # Clearing the destination first makes this idempotent: it
                # only removes one of the two links, the data survives via the
                # other (the one we're about to rename), and the result is the
                # same either way — exactly one copy, living at $dest.
                if ($dest | path exists) {
                    rm --force $dest
                }
                mv $exe $dest

                let pkg_manifest = $artifact.manifest_path? | default ""
                let pkg = $packages | where manifest_path == $pkg_manifest | first | default null

                if $pkg == null {
                    log-warn $"($name): no matching package for manifest ($pkg_manifest)" --component $COMPONENT
                    return null
                }

                emit-artifact-produced $binary_hash $ELF_BINARY_ARTIFACT --name $name

                let sbom_info = $sbom_lookup | get -o $pkg.name | default null

                if $sbom_info != null {
                    let cargo_toml_hash = (content-hash-file $pkg.manifest_path)
                    let binding = (
                        [$binary_hash $cargo_toml_hash $cargo_lock_hash $source_tree_hash]
                        | str join ":"
                        | hash sha256
                        | $"sha256:($in)"
                    )
                    # Emit BinaryLinked — ties this binary to its source package and SBOM.
                    # binding_digest = sha256(binary:cargo_toml:cargo_lock:source_tree)
                    # is a cryptographic attestation of the build inputs.
                    emit-binary-linked $binary_hash $name $sbom_info.root_purl $sbom_info.content_hash --binding_digest $binding
                    log-info $"($name) -> ($sbom_info.root_purl)" --component $COMPONENT
                } else {
                    log-warn $"($name): no SBOM for package ($pkg.name) — binary_linked not emitted" --component $COMPONENT
                }

                {
                    name:              $name
                    path:              $dest
                    binary_hash:       $binary_hash
                    package_name:      $pkg.name
                    root_purl:         ($sbom_info.root_purl?    | default "")
                    sbom_content_hash: ($sbom_info.content_hash? | default "")
                }
            } catch {|e|
                # Without this, a single failing artifact aborts the whole
                # `cargo ... | lines | each ...` pipe — Nushell closes its
                # read end of cargo's stdout mid-stream, cargo (still writing
                # JSON for the rest of the workspace) hits EPIPE, and all you
                # see is cargo's own generic "Broken pipe (os error 32)" on
                # stderr — which is what's been burying the real cause.
                log-error $"failed processing artifact ($artifact.executable? | default '<unknown>'): ($e.msg)" --component $COMPONENT
                null
            }
        }
        | where { $in != null }
    )

    if ($env.LAST_EXIT_CODE? | default 0) != 0 {
        error make {msg: "cargo build failed"}
    }

    $binaries
}

# ===========================================================================
# Phase 3: OCI image build and upload
# ===========================================================================

def registry-refs [image_name: string]: nothing -> list<string> {
    mut refs = []
    let ci_reg = $env.CI_REGISTRY? | default ""
    if ($ci_reg | is-not-empty) { $refs = ($refs | append $"docker://($ci_reg)/polar/($image_name):{tag}") }
    $refs
}

def login-to-registries [] {
    mut creds = []
    let ci_user = $env.CI_REGISTRY_USER? | default ""
    let ci_pass = $env.CI_REGISTRY_PASSWORD? | default ""
    let ci_reg = $env.CI_REGISTRY? | default ""
    if ($ci_user | is-not-empty) and ($ci_reg | is-not-empty) {
        $creds = (
            $creds
            | append {registry: $ci_reg, username: $ci_user, password: $ci_pass}
        )
    }

    if ($creds | is-not-empty) { registry-login $creds }
}

def process-image [entry: record, image_tag: string, --skip-upload]: nothing -> record {
    let build = (nix-build-image $entry.flake $entry.name)
    if not $build.success {
        return {
            success: false
            image_name: $entry.name
            uploads: []
        }
    }
    if $skip_upload {
        return {
            success: true
            image_name: $entry.name
            uploads: []
        }
    }
    build-and-push-image $entry.name $image_tag (registry-refs $entry.image) $build --root_purl $entry.root_purl
}


def build-and-push-image [
    link_name: string
    tag: string
    registries: list<string>
    build: record
    --root_purl: string = ""
]: nothing -> record {
    let oci_metadata = $build.oci_metadata

    if ($registries | length) == 0 {
        log-warn $"No registries configured for ($link_name) — image will not be uploaded anywhere" --component $COMPONENT
    }

    let uploads = ($registries | each {|template|
        let remote_ref = ($template | str replace "{tag}" $tag)
        upload-image $build.tarball $remote_ref --name $link_name
    })

    for upload in $uploads {
        if ($upload.digest | is-empty) {
            log-warn $"no digest for ($upload.remote_ref) — skipping sign" --component $COMPONENT
            continue
        }

        # emit an event saying we created a new build artifact
        emit-artifact-produced $upload.digest "oci-image" --name $link_name
        # then, emit a second message detailing that this artifact was a container image.
        (
        emit-container-image-created
            $link_name
            $build.tarball_hash
            $oci_metadata.config_digest
            $oci_metadata.layers
            --os $oci_metadata.os
            --arch $oci_metadata.arch
            --created $oci_metadata.created
            --entrypoint $oci_metadata.entrypoint
            --cmd $oci_metadata.cmd
            --digest $upload.digest
            --uri $upload.remote_ref
        )

        # ── Cosign signature ───────────────────────────────────────────────────
        # cosign requires a digest ref: registry.io/org/app@sha256:abc...
        let base_ref = (
            $upload.remote_ref
            | str replace "docker://" ""
            | parse "{repo}:{tag}"
            | get -o 0
            | default {repo: ""}
            | get repo
        )
        let digest_ref = $"($base_ref)@($upload.digest)"
        sign-image $digest_ref --name $upload.name
    }

    {
        success: true
        image_name: $link_name
        tarball: $build.tarball
        oci_metadata: $oci_metadata
        uploads: $uploads
    }
}

# ===========================================================================
# Main
# ===========================================================================

def main [
    build_id: string            # Stable unique ID for this pipeline run.
                                # In GitLab CI: $CI_PIPELINE_ID or a UUIDv5 derived from it.
                                # Locally: pass any stable string, e.g. (random uuid) once per session.
    --package(-p): string = ""
    --release(-r)
    --target(-t): string = ""
    --tag: string = ""
    --filter: string = ""
    --skip-upload
    --artifact-dir: path = "pipeline-out"
    --skip-analysis
    --strict-analysis
    --skip-build
    --skip-images
] {
    let ws_root = (workspace-root)
    let ws_manifest = $ws_root | path join "Cargo.toml"
    let commit_sha = ($env.CI_COMMIT_SHA? | default (^git rev-parse HEAD | str trim))

    # Initialize pipeline-scoped singleton state. All emit-* functions that
    # need build_id (artifact domain events) read it from here rather than
    # from an env var. This guarantees every event in this pipeline run carries
    # the same build_id regardless of call depth or module boundaries.
    init-pipeline-state $build_id

    let daemon = (start-cassini-daemon)

    let result = try {
        let start_time = (date now)

        let ref_name   = (^git branch --show-current | str trim)
        let repo_url   = (^git config --get remote.origin.url | str trim)

        emit-execution-started $commit_sha $ref_name $repo_url

        rm --force --recursive $artifact_dir
        mkdir -v $artifact_dir

        if not $skip_analysis {
            let analysis_results = (run-static-analysis $ws_manifest $artifact_dir)
            let failed = $analysis_results | where exit_code != 0
            if ($failed | is-not-empty) {
                let names = $failed | get name | str join ", "
                if $strict_analysis {
                    error make {msg: $"static analysis failed (strict mode): ($names)"}
                } else {
                    log-warn $"static analysis non-zero exits: ($names) — continuing \(pass --strict-analysis to abort\)" --component $COMPONENT
                }
            }
        }

        # Use full SHA for image tag when available — short SHA or "latest" as fallback.
        let image_tag = if ($tag | is-not-empty) {
            $tag
        } else {
            $env.CI_COMMIT_SHORT_SHA? | default ($commit_sha | str substring 0..8) | default "latest"
        }

        let packages = (resolve-packages --package $package)
        log-info $"($packages | length) package\(s\) in scope, image tag: ($image_tag)" --component $COMPONENT

        # Phase 1 — Cargo SBOMs
        #
        #
        let sbom_files  = (generate-workspace-sboms $packages $ws_manifest $artifact_dir --target $target)
        let sbom_lookup = (process-cargo-sboms $sbom_files $packages)
        log-info $"($sbom_files | length) SBOM\(s\) generated and analyzed" --component $COMPONENT

        # Phase 2 — Cargo binaries
        if not $skip_build {
            let binaries = (
                build-and-link-binaries $packages $sbom_lookup $ws_manifest $artifact_dir
                    --release
                    --target $target
                    --filter_package $package
            )
            log-info $"($binaries | length) binary\(ies\) built and linked" --component $COMPONENT
        }

        # Phase 3 — OCI images
        if not $skip_images {
            let manifest = if ($filter | is-not-empty) {
                image-manifest | where { $in.name | str contains $filter }
            } else {
                image-manifest
            }

            if ($manifest | is-empty) {
                log-warn $"no images match filter '($filter)' — skipping Phase 3" --component $COMPONENT
            } else {
                if not $skip_upload { login-to-registries }
                log-info $"building ($manifest | length) image\(s\)" --component $COMPONENT

                let results = ($manifest | each {|entry|
                    log-info $"--- ($entry.name) ---" --component $COMPONENT
                    process-image $entry $image_tag --skip-upload=$skip_upload
                })

                let succeeded = $results | where success == true | length
                let failed    = $results | where success == false | length
                log-info $"($succeeded) image\(s\) succeeded, ($failed) failed" --component $COMPONENT

                if $failed > 0 {
                    let failures = ($results | where success == false | get image_name | str join ", ")
                    log-warn $"failed images: ($failures)" --component $COMPONENT
                }
            }
        }

        let elapsed = (date now) - $start_time
        let duration_secs = ($elapsed / 1sec | math round)
        emit-execution-completed $duration_secs
        log-info $"pipeline complete \(duration: ($elapsed | format duration 'ms')\)" --component $COMPONENT

        null
    } catch {|e|
        let msg = $e.msg
        log-error $msg --component $COMPONENT
        emit-execution-failed $msg
        $msg
    }

    stop-cassini-daemon $daemon

    if $result != null {
        error make {msg: $result}
    }
}
