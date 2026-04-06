use ./pipeline/core.nu *

const COMPONENT = "sbom"
const ELF_BINARY_ARTIFACT = "elf-binary"

# ---------------------------------------------------------------------------
# Build the graph fragment from a parsed CycloneDX document.
#
# This is a pure data transform: CycloneDX JSON → graph projection.
# No side effects, no emissions. Separating this from emit makes it
# testable in isolation.
# ---------------------------------------------------------------------------
def extract-graph-fragment [doc: record, artifact_content_hash: string]: nothing -> record {
    let root_component = ($doc.metadata?.component? | default null)

    # The root package identity. This is the crate/binary the SBOM describes.
    # We key on purl because it's the only stable cross-build identifier.
    # Content hashes change when the SBOM is regenerated; purls don't.
    let root = if $root_component != null {
        {
            purl: ($root_component.purl? | default "")
            name: ($root_component.name? | default "")
            version: ($root_component.version? | default "")
            component_type: ($root_component.type? | default "application")
        }
    } else {
        null
    }

    # Flat node list. We only extract identity fields — no license text,
    # no external references, no tool metadata. Those belong in the SBOM
    # document (which you can always re-fetch from object storage by
    # content hash), not in the graph merge payload.
    let components = ($doc.components? | default [] | each {|c|
        {
            purl: ($c.purl? | default "")
            name: ($c.name? | default "")
            version: ($c.version? | default "")
            component_type: ($c.type? | default "library")
        }
    } | where { ($in.purl | is-not-empty) or ($in.name | is-not-empty) })

    # Edge list from the `dependencies` key in the CycloneDX spec.
    # This is where the actual tree lives. `components` is just an
    # inventory; `dependencies` encodes "A depends on [B, C, D]".
    #
    # If this key is missing (some generators omit it), we degrade
    # gracefully — the agent gets nodes but no edges. That's still
    # useful for "what packages exist in this build" queries, just
    # not for "what depends on what" traversals.
    #
    # FIX: the previous version assigned `let edge = { ... }` inside
    # the each block. In nushell, `let` is a statement that evaluates
    # to nothing — so every iteration returned null and the where
    # filter dropped everything, giving 0 edges. The record must be
    # the last expression in the block to be returned.
    let edges = ($doc.dependencies? | default [] | each {|dep|
        {
            from_ref: ($dep.ref? | default "")
            to_refs: ($dep.dependsOn? | default [])
        }
    } | where { ($in.from_ref | is-not-empty) and ($in.to_refs | length) > 0 })

    {
        artifact_content_hash: $artifact_content_hash
        root: $root
        components: $components
        edges: $edges
    }
}

def workspace-root []: nothing -> string {
    cargo metadata --format-version 1 --no-deps
    | from json
    | get workspace_root
}

def resolve-packages [
    --package (-p): string = ""
]: nothing -> list<record> {
    let ws_root = (workspace-root)
    let manifest = ($ws_root | path join "Cargo.toml")

    let meta = (
        cargo metadata
            --manifest-path $manifest
            --format-version 1
            --no-deps
        | from json
    )

    let packages = if ($package | is-empty) {
        let members = ($meta.workspace_members | default [])
        $meta.packages | where {|pkg| $members | any {|id| $id == $pkg.id } }
    } else {
        $meta.packages | where name == $package
    }

    if ($packages | is-empty) {
        error make { msg: $"no Cargo package matched '($package)'" }
    }

    # Include version, purl, and manifest_path — Phase 2 needs
    # manifest_path to match cargo build artifacts back to packages.
    $packages | each {|pkg| {
        name: $pkg.name
        manifest_path: ($pkg.manifest_path | path expand)
        version: ($pkg.version? | default "0.0.0")
        purl: $"pkg:cargo/($pkg.name)@($pkg.version? | default '0.0.0')"
    }}
}

# ---------------------------------------------------------------------------
# Emit the graph fragment as a single `sbom.analyzed` event.
#
# One SBOM → one event → one batch Cypher merge on the consumer side.
# The envelope carries pipeline/build provenance; the payload carries
# the graph projection. These are separate concerns: provenance tells
# you *when* and *why* this graph was observed, the fragment tells you
# *what* the graph looks like.
# ---------------------------------------------------------------------------
def emit-sbom-analyzed [fragment: record, filename: string] {
    let payload = {
        filename: $filename
        artifact_content_hash: $fragment.artifact_content_hash
        root: $fragment.root
        components: $fragment.components
        edges: $fragment.edges
    }
    emit "sbom.resolved" $payload
}

# Emitted after a binary is built, linking it to the package (via purl)
# and the SBOM (via content hash) that describes that package's deps.
def emit-binary-linked [
    binary_content_hash: string
    binary_name: string
    root_purl: string
    sbom_content_hash: string
    --binding_digest: string = ""
] {
    mut payload = {
        binary_content_hash: $binary_content_hash
        binary_name: $binary_name
        root_purl: $root_purl
        sbom_content_hash: $sbom_content_hash
    }
    if ($binding_digest | is-not-empty) {
        $payload = ($payload | insert binding_digest $binding_digest)
    }
    emit "binary.linked" $payload
}

# Generate SBOMs for one package via cargo-cyclonedx, move them into
# artifact_dir, and emit an artifact event per SBOM.
#
# Returns a list of records shaped like:
#   { name, path, artifact_id, components }
def generate-workspace-sboms [
    packages: list<record>
    ws_manifest: path
    artifact_dir: path
    --target (-t): string = ""
]: nothing -> list<record> {
    let ws_root = ($ws_manifest | path dirname)

    log-info $"generating SBOMs for ($packages | length) package\(s\)" --component $COMPONENT

    mut args = [--manifest-path $ws_manifest -f json]
    if ($target | is-not-empty) { $args = ($args | append [--target $target]) }

    # use cargo cyclonedx to generate sboms for each package.
    let result = (^cargo cyclonedx ...$args | complete)

    if $result.exit_code != 0 {
        let msg = ($result.stderr | default $result.stdout | str trim)
        log-warn $"cargo-cyclonedx failed: ($msg)" --component $COMPONENT
        return []
    }

    # cargo-cyclonedx just throws sboms in each package's crate root. So we have to move them all to our artifact directory.
    let move_result = (
        bash -c $"find ($ws_root) -type f -name '*.cdx.json' | while read -r sbom; do mv \"$sbom\" \"($artifact_dir)/$\(basename \"$sbom\"\)\"; done" | complete
    )

    if $move_result.exit_code != 0 {
        log-warn $"failed to move SBOMs: ($move_result.stdout)" --component $COMPONENT
        return []
    }

    # Now read whatever landed in artifact_dir.
    return (
        ls $artifact_dir
        | where type == file
        | where { ($in.name | path basename | str ends-with ".cdx.json") }
    )
}

# ---------------------------------------------------------------------------
# Process SBOM files: parse, extract graph fragments, emit events.
# Returns a lookup table (record) mapping package name → SBOM info,
# which Phase 2 uses to link binaries to their SBOMs.
# ---------------------------------------------------------------------------
def process-sboms [
    sbom_files: list<record>
    packages: list<record>
    artifact_dir: path
]: nothing -> record {
    mut sbom_lookup = {}

    for f in $sbom_files {
        let doc = try { open $f.name } catch {|e|
            log-warn $"could not parse ($f.name): ($e.msg)" --component $COMPONENT
            continue
        }

        if ($doc.bomFormat? | default "") != "CycloneDX" { continue }

        let filename = ($f.name | path basename)
        let stem = ($filename | str replace ".cdx.json" "")
        let content_hash = (content-hash-file $f.name)

        # Provenance: record that this build stage produced an SBOM artifact.
        emit-artifact-produced $content_hash "sbom" --name $filename --content_type "application/vnd.cyclonedx+json"

        # Graph projection: extract nodes + edges, emit as a single event.
        let fragment = (extract-graph-fragment $doc $content_hash)

        if $fragment.root != null {
            emit-sbom-analyzed $fragment $filename
            log-info $"($stem): ($fragment.components | length) components, ($fragment.edges | length) edges" --component $COMPONENT
        } else {
            log-warn $"($filename) has no metadata.component — graph fragment not emitted" --component $COMPONENT
        }

        # Match to our resolved package for purl lookup.
        let matched = ($packages | where name == $stem | first | default null)
        let root_purl = if $fragment.root != null {
            $fragment.root.purl
        } else if $matched != null {
            ($matched.purl? | default "")
        } else {
            ""
        }

        # Store in lookup for Phase 2.
        $sbom_lookup = ($sbom_lookup | insert $stem {
            content_hash: $content_hash
            root_purl: $root_purl
            component_count: ($fragment.components | length)
            edge_count: ($fragment.edges | length)
        })
    }

    $sbom_lookup
}

# ===========================================================================
# Phase 2: Build binaries and link them to their SBOMs
#
# sbom_lookup is passed in from Phase 1 — this is why the two phases
# live in the same script. Each binary gets linked to the package it
# was built from (via purl) and the SBOM that describes that package's
# dependencies (via content hash).
# ===========================================================================

def build-and-link-binaries [
    packages: list<record>
    sbom_lookup: record
    artifact_dir: path
    --release
    --target: string = ""
    --filter_package: string = ""
]: nothing -> list<record> {
    let ws_root = (workspace-root)

    mut cargo_args = ["build" "--message-format=json" "--locked" "--quiet"]
    if ($filter_package | is-not-empty) { $cargo_args = ($cargo_args | append ["--package" $filter_package]) }
    if $release { $cargo_args = ($cargo_args | append "--release") }
    if ($target | is-not-empty) { $cargo_args = ($cargo_args | append ["--target" $target]) }

    log-info $"Running: cargo ($cargo_args | str join ' ')" --component $COMPONENT

    # Hash build inputs for the attestation binding.
    let cargo_lock_hash = (content-hash-file $"($ws_root)/Cargo.lock")
    let source_tree_hash = (tree-hash $ws_root)

    let binaries = (
        cargo ...$cargo_args
        | lines
        | where { ($in | str trim) != "" }
        | each {|line|
            try { $line | from json } catch { null }
        }
        | where { $in != null }
        | where { ($in.reason? | default "") == "compiler-artifact" }
        | where { ($in.target?.kind? | default []) | any {|k| $k == "bin"} }
        | where { ($in.executable? | default null) != null }
        | each {|artifact|
            let exe = $artifact.executable
            let name = ($exe | path basename)
            let dest = ($artifact_dir | path join $name)
            let binary_hash = (content-hash-file $exe)

            mv $exe $dest

            # Match on manifest_path — stable and unambiguous, unlike
            # target.name which is the binary name not the package name,
            # or package_id which has a version-dependent format.
            let pkg_manifest = ($artifact.manifest_path? | default "")
            let pkg = ($packages | where manifest_path == $pkg_manifest | first | default null)

            if $pkg == null {
                log-warn $"($name): no matching package for manifest ($pkg_manifest)" --component $COMPONENT
                return null
            }

            # Emit provenance: "this pipeline produced this binary."
            emit-artifact-produced $binary_hash $ELF_BINARY_ARTIFACT --name $name

            # Cross-reference with the SBOM from Phase 1.
            let sbom_info = ($sbom_lookup | get -o $pkg.name | default null)

            if $sbom_info != null {
                # Compute attestation binding: cryptographic proof that
                # this binary was built from these specific inputs.
                let cargo_toml_hash = (content-hash-file $pkg.manifest_path)
                let binding = (
                    [$binary_hash $cargo_toml_hash $cargo_lock_hash $source_tree_hash]
                    | str join ":"
                    | hash sha256
                    | $"sha256:($in)"
                )

                emit-binary-linked $binary_hash $name $sbom_info.root_purl $sbom_info.content_hash --binding_digest $binding

                log-info $"($name) -> ($sbom_info.root_purl)" --component $COMPONENT
                log-info $"  binary:  ($binary_hash)" --component $COMPONENT
                log-info $"  sbom:    ($sbom_info.content_hash)" --component $COMPONENT
            } else {
                log-warn $"($name): no SBOM found for package ($pkg.name) — binary.linked not emitted" --component $COMPONENT
            }

            {
                name: $name
                path: $dest
                binary_hash: $binary_hash
                package_name: $pkg.name
                root_purl: ($sbom_info.root_purl? | default "")
                sbom_content_hash: ($sbom_info.content_hash? | default "")
            }
        }
        | where { $in != null }
    )

    if ($env.LAST_EXIT_CODE? | default 0) != 0 {
        error make { msg: "cargo build failed" }
    }

    $binaries
}

# ===========================================================================
# Main
# ===========================================================================

def main [
    --package (-p): string = ""
    --release (-r)
    --target (-t): string = ""
    --artifact-dir: path = "pipeline-out"
    --skip-build              # Only generate and analyze SBOMs, skip binary building
] {
    let cassini_job_id = start-cassini-daemon

    mkdir -v $artifact_dir

    let ws_root = (workspace-root)
    let ws_manifest = ($ws_root | path join "Cargo.toml")
    let packages = (resolve-packages --package $package)

    log-info $"($packages | length) package\(s\) to process" --component $COMPONENT

    # Phase 1: Generate SBOMs, extract graph fragments, emit events.
    # Returns a lookup table: { package_name: { content_hash, root_purl, ... } }
    let sboms = (generate-workspace-sboms $packages $ws_manifest $artifact_dir --target $target)
    let sbom_lookup = (process-sboms $sboms $packages $artifact_dir)

    log-info $"($sboms | length) SBOM\(s\) generated and analyzed" --component $COMPONENT

    # Phase 2: Build binaries and link them to their SBOMs.
    # sbom_lookup is in scope — the whole reason both phases are in one script.
    if not $skip_build {
        let binaries = (
            build-and-link-binaries $packages $sbom_lookup $artifact_dir
                --release
                --target $target
                --filter_package $package
        )
        log-info $"($binaries | length) binary\(ies\) built and linked" --component $COMPONENT
    }

    stop-cassini-daemon $cassini_job_id
}
