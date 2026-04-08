
const CASSINI_SOCK_ENV = "CASSINI_DAEMON_SOCK"
const CASSINI_SESSION_ENV = "POLAR_CASSINI_SESSION_ID"

export const MANIFEST_PATH = "/etc/pipeline/pipeline.json"
export const SUBJECT_PREFIX = "polar.builds"
export const ELF_BINARY_ARTIFACT = "elf-binary-set"


# ---------------------------------------------------------------------------
# Build the graph fragment from a parsed CycloneDX document.
#
# This is a pure data transform: CycloneDX JSON → graph projection.
# No side effects, no emissions. Separating this from emit makes it
# testable in isolation.
# ---------------------------------------------------------------------------
export def extract-graph-fragment [doc: record, artifact_content_hash: string]: nothing -> record {
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

export def workspace-root []: nothing -> string {
    cargo metadata --format-version 1 --no-deps
    | from json
    | get workspace_root
}

export def process-sboms [files: list<record>, packages: list<record>, artifact_dir: path = "pipeline-out", --component: string = ""]: nothing -> list<record> {
    # Now read whatever landed in artifact_dir.
    let sbom_files = (
        ls $artifact_dir
        | where type == file
        | where { ($in.name | path basename | str ends-with ".cdx.json") }
    )

    $sbom_files | each {|f|
            let doc = try { open $f.name } catch {|e|
                log-warn $"could not parse ($f.name): ($e.msg)" --component $component
                null
            }
            if $doc == null { return null }
            if ($doc.bomFormat? | default "") != "CycloneDX" { return null }

            let filename = ($f.name | path basename)
            let stem = ($filename | str replace ".cdx.json" "")
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
            let matched = ($packages | where name == $stem | first)
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

# ---------------------------------------------------------------------------
# Timing
# ---------------------------------------------------------------------------

def elapsed-ms [start: datetime]: nothing -> int {
    (((date now) - $start) / 1_000_000) | into int
}

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

export def log [level: string, msg: string, --component: string = ""] {
    let ts = (date now | format date "%Y-%m-%dT%H:%M:%S%.3fZ")
    print $"($ts) [($level)] ($component) — ($msg)"
}

export def log-info  [msg: string, --component: string = ""] { log "INFO"  $msg --component $component }
export def log-warn  [msg: string, --component: string = ""] { log "WARN"  $msg --component $component }
export def log-error [msg: string, --component: string = ""] { log "ERROR" $msg --component $component }
export def log-debug [msg: string, --component: string = ""] { log "DEBUG" $msg --component $component }

# ---------------------------------------------------------------------------
# Cassini
# ---------------------------------------------------------------------------

# Start the cassini-client daemon as a background job and wait for it
# # to be ready. Sets CASSINI_DAEMON_SOCK in the environment so all
# # subsequent cassini-client invocations find the socket automatically.
# # Returns the job id so the caller can stop it cleanly on exit.
export def start-cassini-daemon [
    --socket: string = "/tmp/cassini-pipeline.sock"
    --timeout: int = 30
]: nothing -> int {
    # If a daemon is already reachable at the socket, don't start another.
    let status = (try { cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
    if $status.exit_code == 0 {
        log-info "cassini daemon already running, reusing" --component "cassini"
        $env.CASSINI_DAEMON_SOCK = $socket
        return 1  # sentinel: we didn't start it, don't stop it
    }

    log-info $"starting cassini daemon at ($socket)" --component "cassini"

    let job_id = (job spawn {
        cassini-client --daemon --foreground --socket $socket
    })

    # Poll until the socket is accepting commands or we time out.
    let ready = (
        0..($timeout * 2)  # 500ms steps
        | each {|_|
            let probe = (try { cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
            if $probe.exit_code == 0 { true } else { sleep 500ms; false }
        }
        | any { $in == true }
    )

    if not $ready {
        log-error $"cassini daemon did not become ready within ($timeout)s" --component "cassini"
        job kill $job_id
        error make { msg: "cassini daemon startup timeout" }
    }

    $env.CASSINI_DAEMON_SOCK = $socket
    log-info "cassini daemon ready" --component "cassini"
    $job_id
}

# Stop the daemon job started by start-cassini-daemon.
# Pass -1 if you didn't start it (reused an existing daemon) and this is a no-op.
export def stop-cassini-daemon [job_id: int] {
    if $job_id == -1 { return }
    log-info "stopping cassini daemon" --component "cassini"
    try { cassini-client status | complete } catch {}  # flush in-flight
    job kill $job_id
}

# ---------------------------------------------------------------------------
# Cassini provenance emission
# ---------------------------------------------------------------------------

export def emit [subject_suffix: string, payload: record] {
    let envelope = {
        build_id: ($env.POLAR_BUILD_ID? | default "00000000-0000-0000-0000-000000000000")
        stage_exec_id: ($env.POLAR_STAGE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        pipeline_exec_id: ($env.POLAR_PIPELINE_EXEC_ID? | default "00000000-0000-0000-0000-000000000000")
        observed_at: (date now | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        payload: ($payload | merge { type: $subject_suffix })
    }
    # cassini-client publish goes here
    let payload = ($envelope | to json --raw)

    log-debug ($payload | jq)

    cassini-client publish $"($SUBJECT_PREFIX).($subject_suffix)" $payload
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
export def emit-sbom-analyzed [fragment: record, filename: string] {
    let payload = {
        filename: $filename
        artifact_content_hash: $fragment.artifact_content_hash
        root: $fragment.root
        components: $fragment.components
        edges: $fragment.edges
    }
    emit "sbom.resolved" $payload
}

export def emit-build-observed [command: string, working_dir: string, --parent_id: string = ""] {
    let base = { command: $command, working_dir: $working_dir }
    emit "build.observed" (
        if ($parent_id | is-not-empty) { $base | merge { parent_execution_id: $parent_id } } else { $base }
    )
}

export def emit-build-completed [exit_code: int, duration_ms: int] {
    emit "build.completed" { exit_code: $exit_code, duration_ms: $duration_ms }
}

# ---------------------------------------------------------------------------
# Lifecycle event: an artifact was produced by this build stage.
# This is purely about provenance — "stage X produced file Y at time T."
# It does NOT carry dependency semantics. The graph fragment does that.
#
# We still emit this so the knowledge graph can link:
#   (BuildStage)-[:PRODUCED]->(Artifact {content_hash})
# which is a different concern from the dependency subgraph.
# ---------------------------------------------------------------------------
export def emit-artifact-produced [content_hash: string, artifact_type: string, --name: string = "", --content_type: string = ""] {
    mut payload = { artifact_content_hash: $content_hash, artifact_type: $artifact_type }
    if ($name | is-not-empty)         { $payload = ($payload | insert name $name) }
    if ($content_type | is-not-empty) { $payload = ($payload | insert content_type $content_type) }
    emit "artifact.produced" $payload
}

# ---------------------------------------------------------------------------
# Content hashing
# ---------------------------------------------------------------------------

export def content-hash-file [path: string]: nothing -> string {
    open $path --raw | hash sha256 | $"sha256:($in)"
}

export def content-hash-dir [dir: string]: nothing -> string {
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

export def content-hash [path: string]: nothing -> string {
    if ($path | path type) == "dir" {
        content-hash-dir $path
    } else {
        content-hash-file $path
    }
}

export def git-commit-sha [dir: string]: nothing -> record {
    let result = try { git -C $dir rev-parse HEAD | complete } catch { { exit_code: 1 } }
    if $result.exit_code == 0 {
        { available: true, sha: ($result.stdout | str trim) }
    } else {
        { available: false, sha: "" }
    }
}

# Hash a directory tree via git tree hash if available, else sorted file hashes.
export def tree-hash [dir: string]: nothing -> string {
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


# ---------------------------------------------------------------------------
# OCI image utilities for pipeline/core.nu
#
# Reusable functions for building, scanning, and uploading OCI images.
# These are generic — they don't know about Polar's specific images.
# Your project-specific pipeline script defines WHAT to build;
# these functions define HOW.
# ---------------------------------------------------------------------------

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
    let resolved = ($tarball_path | path expand)

    # Resolve through symlinks — nix build outputs are symlinks to /nix/store.
    let real_path = (^readlink -f $resolved | str trim)

    if ($real_path | path exists) != true {
        error make { msg: $"Image tarball not found: ($tarball_path) (resolved to ($real_path))" }
    }

    let stem = if ($name | is-not-empty) { $name } else { ($tarball_path | path basename | str replace ".tar.gz" "" | str replace ".tar" "") }
    let sbom_filename = $"($stem).image.cdx.json"
    let sbom_path = ($output_dir | path join $sbom_filename)

    # Syft is crazy strict on the formats it accepts (vs docker and podman) so it doesn't really like the tar.gz files that nix spits out
    # so, we have to do the unfortunate task of forcing it into an oci format
    let new_tarball_path = ($"($stem)Image")

    log-info "Marshalling nix output tarball to a oci-archive"
    let conversion_result = (
        ^skopeo copy $"docker-archive:($real_path)" $"oci-archive:($new_tarball_path)" --tmpdir ($env.TMPDIR? | default "./tmp") | complete
    )

    if $conversion_result.exit_code != 0 {
        let msg = ($conversion_result.stderr? | default $conversion_result.stdout | str trim)
        log-warn $"skopeo failed to create oci-archive: ($msg)" --component "oci"
        return { success: false, path: "", name: $stem }
    }
    log-info $"Generating image SBOM for ($stem)" --component "oci"

    let result = (
        ^syft $"oci-archive:($new_tarball_path)" -o $"cyclonedx-json=($sbom_path)" | complete
    )

    if $result.exit_code != 0 {
        let msg = ($result.stderr? | default $result.stdout | str trim)
        log-warn $"syft failed for ($stem): ($msg)" --component "oci"
        return { success: false, path: "", name: $stem }
    }

    if ($sbom_path | path exists) != true {
        log-warn $"syft produced no output for ($stem)" --component "oci"
        return { success: false, path: "", name: $stem }
    }

    log-info $"Image SBOM generated: ($sbom_filename)" --component "oci"
    { success: true, path: $sbom_path, name: $stem }
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
    let components = ($doc.components? | default [])

    $components | each {|comp|
        let purl = ($comp.purl? | default "")
        if ($purl | is-empty) { return null }

        let props = ($comp.properties? | default [])

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
export def process-image-sbom [
    sbom_path: path
    image_name: string
]: nothing -> record {
    let doc = try { open $sbom_path } catch {|e|
        log-warn $"Could not parse image SBOM ($sbom_path): ($e.msg)" --component "oci"
        return { success: false }
    }

    if ($doc.bomFormat? | default "") != "CycloneDX" {
        log-warn $"($sbom_path) is not CycloneDX format" --component "oci"
        return { success: false }
    }

    let filename = ($sbom_path | path basename)
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

# ===========================================================================
# Image building (Nix)
# ===========================================================================

# Build a single Nix image derivation.
#
# Returns a record with the build result, including the resolved
# tarball path (through the nix store symlink).
#
# `flake_ref` is the full flake reference, e.g.:
#   .#polarPkgs.cassini.cassiniImage
#
# `link_name` is the result symlink name, e.g. "cassini"
export def nix-build-image [
    flake_ref: string
    link_name: string
]: nothing -> record {
    log-info $"Building ($link_name) from ($flake_ref)" --component "oci"

    let result = (^nix build --quiet $flake_ref -o $link_name | complete)

    if $result.exit_code != 0 {
        let msg = ($result.stderr? | default $result.stdout | str trim)
        log-warn $"nix build failed for ($link_name): ($msg)" --component "oci"
        return { success: false, link_name: $link_name, flake_ref: $flake_ref, tarball: "" }
    }

    let tarball = (^readlink -f $link_name | str trim)

    if ($tarball | path exists) != true {
        log-warn $"nix build produced no output for ($link_name)" --component "oci"
        return { success: false, link_name: $link_name, flake_ref: $flake_ref, tarball: "" }
    }

    log-info $"Built ($link_name): ($tarball)" --component "oci"
    { success: true, link_name: $link_name, flake_ref: $flake_ref, tarball: $tarball }
}

# ===========================================================================
# Registry operations
# ===========================================================================

# Log in to one or more OCI registries via skopeo.
#
# Takes a list of { registry, username, password } records.
# Stops on first failure.
export def registry-login [credentials: list<record>] {
    for cred in $credentials {
        log-info $"Logging into ($cred.registry)" --component "oci"
        let result = (
            ^skopeo login
                --username $cred.username
                --password $cred.password
                $cred.registry
            | complete
        )
        if $result.exit_code != 0 {
            error make { msg: $"Failed to log into ($cred.registry): ($result.stderr? | default $result.stdout | str trim)" }
        }
    }
}

# Upload an image tarball to a remote registry via skopeo.
#
# Returns a record with the remote ref and the digest skopeo reports.
# The digest is extracted from skopeo's output when available.
export def upload-image [
    tarball_path: path        # Path to the image tarball (or symlink to one)
    remote_ref: string        # Full remote reference, e.g. "docker://registry.io/org/app:tag"
    --name: string = ""       # Human-readable name for logging
]: nothing -> record {
    let label = if ($name | is-not-empty) { $name } else { $remote_ref }
    let real_path = (^readlink -f $tarball_path | str trim)

    log-info $"Uploading ($label) -> ($remote_ref)" --component "oci"

    let result = (
        ^skopeo copy $"docker-archive:($real_path)" $remote_ref | complete
    )

    if $result.exit_code != 0 {
        let msg = ($result.stderr? | default $result.stdout | str trim)
        error make { msg: $"Upload failed for ($label): ($msg)" }
    }

    # Try to extract the digest from skopeo output.
    # skopeo copy sometimes prints "Copying ... digest: sha256:abc..."
    let output = ($result.stdout? | default "" | append ($result.stderr? | default "") | str join "\n")
    let digest = ($output
        | parse --regex 'sha256:[a-f0-9]{64}'
        | get -o 0
        | default { capture0: "" }
        | get capture0
    )

    log-info $"Uploaded ($label)" --component "oci"
    { remote_ref: $remote_ref, digest: $digest, name: $label }
}

# ===========================================================================
# High-level pipeline: build → scan → upload → emit
#
# Orchestrates the full lifecycle for a single image. This is the
# function your project-specific pipeline calls in a loop.
#
# Sequence:
#   1. Build the image tarball (nix build)
#   2. Generate an SBOM from the tarball (syft)
#   3. Process the SBOM (extract fragment + layer attrs, emit events)
#   4. Upload to each registry (skopeo copy)
#   5. Emit image.linked for each upload (binary/package linkage)
#
# `registries` is a list of remote ref templates with `{tag}` placeholder:
#   ["docker://registry.io/org/myapp:{tag}", "docker://acr.io/myapp:{tag}"]
# ===========================================================================

export def build-scan-upload [
    flake_ref: string           # Nix flake reference for the image
    link_name: string           # Symlink name for nix build output
    tag: string                 # Image tag (e.g. commit SHA)
    registries: list<string>    # Remote ref templates with {tag} placeholder
    artifact_dir: path          # Where to store SBOMs
    --root_purl: string = ""    # Purl of the root package this image contains
    --sbom_content_hash: string = ""  # Content hash of the source-level SBOM, if known
]: nothing -> record {
    # 1. Build
    let build = (nix-build-image $flake_ref $link_name)
    if not $build.success {
        return { success: false, image_name: $link_name }
    }

    # 2. Generate SBOM from the tarball
    let sbom = (generate-image-sbom $build.tarball $artifact_dir --name $link_name)

    # 3. Process SBOM (extract graph fragment + layer attrs, emit events)
    if $sbom.success {
        process-image-sbom $sbom.path $link_name
    } else {
        log-warn $"Skipping SBOM processing for ($link_name) — generation failed" --component "oci"
    }

    # 4. Upload to each registry
    let uploads = ($registries | each {|template|
        let remote_ref = ($template | str replace "{tag}" $tag)
        upload-image $build.tarball $remote_ref --name $link_name
    })

    # 5. Emit image.linked for each upload that produced a digest.
    #    This connects the image in the registry to the package it was
    #    built from, closing the chain:
    #      (OCIArtifact {digest})-[:BUILT_FROM]->(Package {purl})
    #
    #    The resolver agent will independently discover these images in
    #    the registry and emit OCIArtifactResolved with manifest/layer
    #    data. The linker joins them via digest.
    for upload in $uploads {
        if ($upload.digest | is-not-empty) and ($root_purl | is-not-empty) {
            emit "image.linked" {
                image_digest: $upload.digest
                remote_ref: $upload.remote_ref
                image_name: $link_name
                root_purl: $root_purl
                sbom_content_hash: ($sbom_content_hash | default "")
                image_sbom_content_hash: (if $sbom.success { content-hash-file $sbom.path } else { "" })
            }
        }
    }

    {
        success: true
        image_name: $link_name
        tarball: $build.tarball
        sbom: $sbom
        uploads: $uploads
    }
}
