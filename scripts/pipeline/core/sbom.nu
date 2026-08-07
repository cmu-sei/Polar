
export def process-sboms [
    files: list<record>
    packages: list<record>
    artifact_dir: path = "pipeline-out"
    --component: string = ""
]: nothing -> list<record> {
    # Now read whatever landed in artifact_dir.
    let sbom_files = (
        ls $artifact_dir
        | where type == file
        | where { ($in.name | path basename | str ends-with ".cdx.json") }
    )

    $sbom_files
    | each {|f|
            let doc = try { open $f.name } catch {|e|
                log-warn $"could not parse ($f.name): ($e.msg)" --component $component
                null
            }
            if $doc == null { return null }
            if ($doc.bomFormat? | default "") != "CycloneDX" { return null }

            let filename = $f.name | path basename
            let stem = $filename | str replace ".cdx.json" ""
            let content_hash = (content-hash-file $f.name)

            # Provenance: record that this build stage produced an SBOM artifact.
            emit-artifact-produced $content_hash "sbom" --name $filename --content_type "application/vnd.cyclonedx+json"

            # Graph projection: extract nodes + edges, emit as a single event.
            let fragment = (extract-graph-fragment $doc $content_hash)

            if $fragment.root != null {
                emit-sbom-analyzed $fragment $filename
            } else {
                log-warn $"($filename) has no metadata.component — emitting artifact.produced only, no graph fragment" --component $component
            }

            # Return summary for upstream pipeline orchestration.
            let matched = $packages | where name == $stem | first
            {
                name: ($matched.name? | default $stem)
                path: $f.name
                artifact_content_hash: $content_hash
                component_count: ($fragment.components | length)
                edge_count: ($fragment.edges | length)
            }
        }
    | where { $in != null }
}

# ===========================================================================
# SBOM generation
# ===========================================================================

# Generate a CycloneDX SBOM from an OCI image tarball using syft.
#
# Accepts any tarball format: nix-built (docker-archive), `docker save`,
# `podman save`, `skopeo copy --format oci-archive`, etc.
#
# Returns the path to the generated SBOM file.
#
# Syft will attribute each component to the image layer it was found in
# via properties like `syft:location:0:layerID`. These are *uncompressed*
# layer diff IDs (sha256 of the uncompressed tarball). To map them to
# the compressed layer digests in the OCI manifest, you need the image
# config's `rootfs.diff_ids` array — which the resolver agent has access
# to after manifest resolution.
export def generate-image-sbom [
    tarball_path: path       # Path to the image tarball (nix result, docker save output, etc.)
    output_dir: path         # Where to write the SBOM
    --name: string = ""      # Override the SBOM filename stem (default: derived from tarball)
]: nothing -> record {
    let resolved = $tarball_path | path expand

    # Resolve through symlinks — nix build outputs are symlinks to /nix/store.
    let real_path = (^readlink -f $resolved | str trim)

    if ($real_path | path exists) != true {
        error make {msg: $"Image tarball not found: ($tarball_path) (resolved to ($real_path))"}
    }

    let stem = if ($name | is-not-empty) { $name } else { (
        $tarball_path
        | path basename
        | str replace ".tar.gz" ""
        | str replace ".tar" ""
    ) }
    let sbom_filename = $"($stem).image.cdx.json"
    let sbom_path = $output_dir | path join $sbom_filename

    # Syft is crazy strict on the formats it accepts (vs docker and podman) so it doesn't really like the tar.gz files that nix spits out
    # so, we have to do the unfortunate task of forcing it into an oci format
    let new_tarball_path = $"($stem)Image"

    log-info "Marshalling nix output tarball to a oci-archive"
    let conversion_result = (
        ^skopeo copy $"docker-archive:($real_path)" $"oci-archive:($new_tarball_path)" --tmpdir ($env.TMPDIR? | default "./tmp") | complete
    )

    if $conversion_result.exit_code != 0 {
        let msg = (
            $conversion_result.stderr?
            | default $conversion_result.stdout
            | str trim
        )
        log-warn $"skopeo failed to create oci-archive: ($msg)" --component "oci"
        return {success: false, path: "", name: $stem}
    }
    log-info $"Generating image SBOM for ($stem)" --component "oci"

    let result = (
        ^syft $"oci-archive:($new_tarball_path)" -o $"cyclonedx-json=($sbom_path)" | complete
    )

    if $result.exit_code != 0 {
        let msg = $result.stderr? | default $result.stdout | str trim
        log-warn $"syft failed for ($stem): ($msg)" --component "oci"
        return {success: false, path: "", name: $stem}
    }

    if ($sbom_path | path exists) != true {
        log-warn $"syft produced no output for ($stem)" --component "oci"
        return {success: false, path: "", name: $stem}
    }

    log-info $"Image SBOM generated: ($sbom_filename)" --component "oci"
    {success: true, path: $sbom_path, name: $stem}
}

# Extract layer-to-package attribution from a syft-generated CycloneDX SBOM.
#
# Syft stores layer info in each component's `properties` array as entries
# with name `syft:location:N:layerID`. We extract these to build
# (layer_diff_id)-[:CONTAINS]->(package_purl) edges.
#
# Returns a list of { layer_diff_id, purl } records.
#
# NOTE: these are UNCOMPRESSED diff IDs (sha256 of the uncompressed layer
# tarball), not the compressed digests in the OCI manifest. The mapping
# between them lives in the image config's `rootfs.diff_ids` array.
# The linker agent handles this join after both the syft scan and the
# manifest resolution have been ingested.
export def extract-layer-attributions [doc: record]: nothing -> list<record> {
    let components = $doc.components? | default []

    $components
    | each {|comp|
        let purl = $comp.purl? | default ""
        if ($purl | is-empty) { return null }

        let props = $comp.properties? | default []

        # Syft uses properties named `syft:location:N:layerID` where N is
        # the location index (a component can appear in multiple locations).
        let layer_ids = ($props
            | where { ($in.name? | default "") | str contains "layerID" }
            | each { $in.value? | default "" }
            | where { $in | is-not-empty }
            | uniq
        )

        $layer_ids | each {|lid|
            { layer_diff_id: $lid, purl: $purl }
        }
    }
    | where { $in != null }
    | flatten
}

# Process an image SBOM: extract graph fragment + layer attributions,
# emit all events. This is the image-level equivalent of process-sboms
# from the cargo pipeline.
#
# Emits:
#   - artifact.produced (SBOM file was created)
#   - image-sbom.analyzed (graph fragment + layer attributions)
export def process-image-sbom [sbom_path: path, image_name: string, --oci_metadata: record]: nothing -> record {
    let doc = try { open $sbom_path } catch {|e|
        log-warn $"Could not parse image SBOM ($sbom_path): ($e.msg)" --component "oci"
        return {success: false}
    }

    if ($doc.bomFormat? | default "") != "CycloneDX" {
        log-warn $"($sbom_path) is not CycloneDX format" --component "oci"
        return {success: false}
    }

    let filename = $sbom_path | path basename
    let content_hash = (content-hash-file $sbom_path)

    # Emit provenance: this pipeline produced this SBOM file.
    emit-artifact-produced $content_hash "image-sbom" --name $filename --content_type "application/vnd.cyclonedx+json"

    # Extract the standard graph fragment (components + dependency edges).
    let fragment = (extract-graph-fragment $doc $content_hash)

    # Extract syft's layer-to-package attribution.
    let layer_attributions = (extract-layer-attributions $doc)

    # Emit the image-specific analyzed event.
    # This carries both the standard graph fragment AND the layer
    # attributions, so the linker can write both DEPENDS_ON edges
    # and CONTAINS edges in one pass.
    emit "image-sbom.analyzed" {
        filename: $filename
        image_name: $image_name
        artifact_content_hash: $content_hash
        root: $fragment.root
        components: $fragment.components
        edges: $fragment.edges
        layer_attributions: $layer_attributions
    }

    log-info $"($image_name): ($fragment.components | length) components, ($layer_attributions | length) layer attributions" --component "oci"

    {
        success: true
        image_name: $image_name
        sbom_content_hash: $content_hash
        component_count: ($fragment.components | length)
        edge_count: ($fragment.edges | length)
        layer_attribution_count: ($layer_attributions | length)
    }
}

# ---------------------------------------------------------------------------
# Build the graph fragment from a parsed CycloneDX document.
#
# This is a pure data transform: CycloneDX JSON → graph projection.
# No side effects, no emissions. Separating this from emit makes it
# testable in isolation.
# ---------------------------------------------------------------------------
# Normalize a CycloneDX component's `licenses` array into a single string.
# Per spec, each entry is EITHER an `expression` (a full SPDX expression like
# "MIT OR Apache-2.0", covering however many licenses apply) OR a
# `license.{id,name}` pair (a single named license). Multiple entries in
# `licenses` can occur; joined with ", " here rather than trying to compose
# exact AND/OR SPDX semantics across entries, which is genuinely ambiguous
# without knowing the generator's intent — this is a readable capture, not
# a machine-evaluable SPDX expression. Null when the component declares no
# license, which is real and common — not treated as a parse failure.
def normalize-license [comp: record]: nothing -> any {
    let entries = ($comp.licenses? | default [])
    if ($entries | is-empty) { return null }

    let parts = ($entries | each {|l|
        let expr = ($l.expression? | default "")
        if ($expr | is-not-empty) {
            $expr
        } else {
            let id = ($l.license?.id? | default "")
            let name = ($l.license?.name? | default "")
            if ($id | is-not-empty) { $id } else if ($name | is-not-empty) { $name } else { "" }
        }
    } | where { ($in | is-not-empty) })

    if ($parts | is-empty) { null } else { $parts | str join ", " }
}

# Best-effort dependency source classification, derived from purl qualifiers
# rather than a separate CycloneDX field — cargo-cyclonedx has no direct
# equivalent to cargo-audit's `package.source` string. Confirmed: a `vcs_url`
# purl qualifier marks a git-sourced dependency. Everything else defaults to
# a registry classification, which is correct for the overwhelming majority
# (crates.io) but NOT verified against a real path-sourced dependency — your
# own workspace crates depending on each other are exactly that case, and
# exactly the one this function is least trustworthy for. Check by hand
# against a real SBOM containing one before relying on this distinction for
# your own crates specifically.
def derive-source [purl: string]: nothing -> any {
    if ($purl | is-empty) { return null }
    if ($purl | str contains "vcs_url=") {
        "git"
    } else if ($purl | str starts-with "pkg:cargo/") {
        "registry"
    } else {
        null
    }
}

export def extract-graph-fragment [doc: record, artifact_content_hash: string]: nothing -> record {
    let root_component = $doc.metadata?.component? | default null

    # The root package identity. This is the crate/binary the SBOM describes.
    # We key on purl because it's the only stable cross-build identifier.
    # Content hashes change when the SBOM is regenerated; purls don't.
    let root = if $root_component != null {
        {
            purl: ($root_component.purl? | default "")
            name: ($root_component.name? | default "")
            version: ($root_component.version? | default "")
            component_type: ($root_component.type? | default "application")
            license: (normalize-license $root_component)
            source: (derive-source ($root_component.purl? | default ""))
        }
    } else {
        null
    }

    # This originally extracted only identity fields, on the reasoning that
    # descriptive metadata (license, external references, tool info) belonged
    # in the re-fetchable SBOM document, not the graph merge payload — a
    # reasonable position when the graph's job was strictly dependency-graph
    # / supply-chain modeling. That's no longer the whole job: this pipeline
    # now tracks static-analysis findings as first-class facts, and license
    # is exactly that kind of attribute — cheap, static per (name, version),
    # and queried relationally ("which shipped binaries transitively depend
    # on a GPL-family crate") in a way that re-fetching N SBOM files can't
    # support. `license` and `source` are captured for that reason; nothing
    # else (external references, tool metadata) changed — still out of scope.
    let components = ($doc.components? | default [] | each {|c|
        {
            purl: ($c.purl? | default "")
            name: ($c.name? | default "")
            version: ($c.version? | default "")
            component_type: ($c.type? | default "library")
            license: (normalize-license $c)
            source: (derive-source ($c.purl? | default ""))
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
