use ./pipeline/core.nu *

const COMPONENT = "sbom"


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
    let edges = ($doc.dependencies? | default [] | each {|dep|

        let edge = {
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

    $packages | each {|pkg| { name: $pkg.name, manifest_path: ($pkg.manifest_path | path expand) } }
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

def main [
    --package (-p): string = ""
    --release (-r)
    --target (-t): string = ""
    --artifact-dir: path = "pipeline-out"
] {

    let cassini_job_id = start-cassini-daemon

    mkdir -v $artifact_dir

    let ws_root = (workspace-root)
    let ws_manifest = ($ws_root | path join "Cargo.toml")
    let packages = (resolve-packages --package $package)

    log-info $"($packages | length) package\(s\) to process" --component $COMPONENT

    let sboms = (generate-workspace-sboms $packages $ws_manifest $artifact_dir --target $target)

    log-debug "processing sbom(s)" --component $COMPONENT

    process-sboms --component $COMPONENT $sboms $packages $artifact_dir

    log-info $"($sboms | length) SBOM\(s\) generated" --component $COMPONENT


    stop-cassini-daemon $cassini_job_id
}
