
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

export def workspace-root [--manifest-path: string = ""]: nothing -> string {
    if ($manifest_path | is-not-empty) {
        return ($manifest_path | path dirname)
    }
    let git_root = (^git rev-parse --show-toplevel | str trim)
    let default_manifest = ($git_root | path join "src/agents/Cargo.toml")
    if ($default_manifest | path exists) {
        $git_root | path join "src/agents"
    } else {
        error make { msg: $"could not find Cargo.toml at ($default_manifest)" }
    }
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

# ANSI helpers — used only for static analysis terminal output.
# TODO: Consider moving to the core library
# TODO: It would also be interesting if log fns could use this
export def green  [msg: string] { $"(ansi green)($msg)(ansi reset)" }
export def red    [msg: string] { $"(ansi red)($msg)(ansi reset)" }
export def yellow [msg: string] { $"(ansi yellow)($msg)(ansi reset)" }
export def bold   [msg: string] { $"(ansi attr_bold)($msg)(ansi reset)" }

# Private log fn, we label all logs with some level of importance
def log [level: string, msg: string, --component: string = ""] {
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

# ---------------------------------------------------------------------------
# Cassini daemon lifecycle
#
# The cassini-client daemon re-execs itself as an orphan process when called
# with --daemon (no --foreground). The parent invocation returns immediately;
# the child is adopted by PID 1 and runs independently of this script.
#
# Consequences for lifetime management:
#   - Nu's job system cannot track or kill the daemon — it never owned it.
#   - The daemon writes its own PID file at <socket>.pid before accepting
#     connections (resolved by Rust's resolve_pid_path: socket.with_extension("pid")).
#   - The daemon's SIGTERM handler removes both the socket and PID file on exit.
#   - Stopping = sending SIGTERM to the PID in the PID file.
#
# The "already running" path is the normal case in a warm CI environment.
# start-cassini-daemon is idempotent: if the socket is reachable it returns
# a sentinel (-1) and stop-cassini-daemon treats -1 as a no-op.
# ---------------------------------------------------------------------------

export def start-cassini-daemon [
    --socket: string = "/tmp/cassini-pipeline.sock"
    --timeout: int   = 30
]: nothing -> int {
    # Probe first — if reachable, reuse the running daemon.
    # status exits 0 when the daemon responds; non-zero or exception means absent/stale.
    let probe = (try { ^cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
    if $probe.exit_code == 0 {
        log-info "cassini daemon already running, reusing" --component "cassini"
        $env.CASSINI_DAEMON_SOCK = $socket
        return (-1)   # sentinel: caller must NOT signal this daemon on exit
    }

    # Remove a stale socket if one exists — the daemon won't start if it finds
    # a socket file it didn't create.
    if ($socket | path exists) {
        log-warn $"removing stale cassini socket at ($socket)" --component "cassini"
        rm -f $socket
    }

    log-info $"starting cassini daemon at ($socket)" --component "cassini"

    # --daemon causes the Rust process to re-exec itself with --foreground and
    # then exit. The child is detached (stdin/stdout/stderr → /dev/null) and
    # adopted by PID 1. This call returns almost immediately.
    ^cassini-client --daemon --socket $socket

    # Poll the socket until the daemon is accepting connections or we time out.
    # Each iteration is 500ms; total attempts = timeout * 2.
    let ready = (
        0..($timeout * 2)
        | each {|_|
            let check = (try { ^cassini-client --socket $socket status | complete } catch { { exit_code: 1 } })
            if $check.exit_code == 0 {
                true
            } else {
                sleep 500ms
                false
            }
        }
        | any { $in == true }
    )

    if not $ready {
        log-error $"cassini daemon did not become ready within ($timeout)s" --component "cassini"
        # Best-effort cleanup — the process may not have written its PID yet.
        let pid_file = ($socket | path parse | update extension "pid" | path join)
        if ($pid_file | path exists) {
            let pid = (try { open --raw $pid_file | str trim | into int } catch { 0 })
            if $pid > 0 { try { ^kill $pid } catch {} }
        }
        error make { msg: "cassini daemon startup timeout" }
    }

    $env.CASSINI_DAEMON_SOCK = $socket
    log-info "cassini daemon ready" --component "cassini"

    # Return the PID so stop-cassini-daemon can signal the orphan process.
    # We read it from the PID file the daemon wrote rather than tracking a
    # Nu job ID, which would be meaningless for a detached process.
    let pid_file = ($socket | path parse | update extension "pid" | path join)
    let pid = (try { open --raw $pid_file | str trim | into int } catch { 0 })
    if $pid == 0 {
        log-warn "could not read cassini daemon PID — stop-cassini-daemon will be a no-op" --component "cassini"
    }
    $pid
}

# Stop the cassini daemon started by start-cassini-daemon.
#
# Pass -1 (the "reused existing daemon" sentinel) to make this a no-op —
# we must not kill a daemon we didn't start.
#
# For a daemon we did start, sends SIGTERM. The daemon's signal handler
# disconnects from the broker cleanly and removes the socket + PID file.
# We wait up to 3s for the socket to disappear as confirmation.
export def stop-cassini-daemon [pid: int, --socket: string = "/tmp/cassini-pipeline.sock"] {
    if $pid == -1 { return }
    if $pid == 0  {
        log-warn "stop-cassini-daemon: no valid PID, skipping" --component "cassini"
        return
    }

    log-info $"stopping cassini daemon (pid ($pid))" --component "cassini"

    try { ^kill $pid } catch {|e|
        log-warn $"could not send SIGTERM to cassini daemon: ($e.msg)" --component "cassini"
        return
    }

    # Wait for the socket to disappear — the daemon's signal handler removes
    # it as part of clean shutdown. If it's still present after 3s something
    # went wrong, but there's nothing more we can do.
    let gone = (
        1..6
        | each {|_| sleep 500ms; not ($socket | path exists) }
        | any { $in == true }
    )
    if not $gone {
        log-warn "cassini socket still present after SIGTERM — daemon may not have exited cleanly" --component "cassini"
    }
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
    --oci_metadata: record
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
# Builds the image, extracts OCI metadata from the resulting tarball,
# and emits a `container-image.created` event immediately. The image
# artifact exists the moment nix build completes — we don't wait for
# an upload, a resolver discovery, or an SBOM scan to announce it.
#
# The emitted event carries enough for the linker to create the
# OCIArtifact node and its OCILayer children right away:
#   - Config digest (image identity before registry push)
#   - Ordered layer list with uncompressed diff IDs
#   - OS, arch, entrypoint, cmd
#
# Returns a record including the tarball path AND the extracted
# oci_metadata, so downstream callers (build-scan-upload, etc.)
# don't need to re-extract it.
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
        return { success: false, link_name: $link_name, flake_ref: $flake_ref, tarball: "", oci_metadata: { success: false } }
    }

    let tarball = (^readlink -f $link_name | str trim)

    if ($tarball | path exists) != true {
        log-warn $"nix build produced no output for ($link_name)" --component "oci"
        return { success: false, link_name: $link_name, flake_ref: $flake_ref, tarball: "", oci_metadata: { success: false } }
    }

    # Extract OCI metadata from the tarball immediately.
    # The image exists now — announce it now.
    let oci_metadata = (extract-oci-metadata $tarball)

    let tarball_hash = (content-hash-file $tarball)

    # Emit container-image.created: "an OCI image was just built."
    # This is the earliest possible announcement. The linker can
    # create the OCIArtifact + OCILayer nodes from this alone,
    # without waiting for a registry push or resolver discovery.
    if ($oci_metadata | get -o success | default false) {
        emit "container-image.created" {
            image_name: $link_name
            tarball_hash: $tarball_hash
            config_digest: $oci_metadata.config_digest
            layers: $oci_metadata.layers
            os: $oci_metadata.os
            arch: $oci_metadata.arch
            created: $oci_metadata.created
            entrypoint: $oci_metadata.entrypoint
            cmd: $oci_metadata.cmd
            repo_tags: ($oci_metadata.repo_tags? | default [])
        }
        log-info $"($link_name): ($oci_metadata.layers | length) layers, ($oci_metadata.os)/($oci_metadata.arch)" --component "oci"
    } else {
        log-warn $"($link_name): built successfully but could not extract OCI metadata" --component "oci"
    }

    log-info $"Built ($link_name): ($tarball)" --component "oci"
    { success: true, link_name: $link_name, flake_ref: $flake_ref, tarball: $tarball, oci_metadata: $oci_metadata }
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


# Sign a container image in a registry using cosign.
#
# The image must already be pushed — cosign signs by digest in the
# registry, not from a local tarball. This means signing happens
# AFTER upload-image, using the remote ref and digest.
#
# The signature is stored as a tag in the same registry (cosign's
# default behavior), so no additional storage infrastructure is needed.
#
# Returns a record with the signature status and metadata.
export def sign-image [
    remote_ref: string        # Full image ref with digest, e.g. "registry.io/app@sha256:abc..."
    --key: string = ""        # Cosign key URI (default: $env.COSIGN_KEY)
    --name: string = ""       # Human-readable name for logging
]: nothing -> record {
    let label = if ($name | is-not-empty) { $name } else { $remote_ref }
    let signing_key = if ($key | is-not-empty) { $key } else { ($env.COSIGN_KEY? | default "") }

    if ($signing_key | is-empty) {
        log-warn $"No signing key available, skipping signature for ($label)" --component "oci"
        return { success: false, reason: "no signing key configured" }
    }

    log-info $"Signing ($label)" --component "oci"

    let result = (
        ^cosign sign --key $signing_key --yes $remote_ref | complete
    )

    if $result.exit_code != 0 {
        let msg = ($result.stderr? | default $result.stdout | str trim)
        log-warn $"cosign sign failed for ($label): ($msg)" --component "oci"
        return { success: false, reason: $msg }
    }

    log-info $"Signed ($label)" --component "oci"
    { success: true }
}

# ===========================================================================
# OCI tarball introspection
#
# Docker-archive tarballs (from nix, docker save, podman save) contain
# the manifest and config as JSON files. We can extract everything the
# resolver agent would have fetched over the network — layer digests,
# media types, diff IDs, config — without any registry round-trip.
#
# Tarball structure (docker-archive format):
#   manifest.json   — array of [{ Config, RepoTags, Layers }]
#   <config_hash>.json — the OCI image config
#   <layer_hash>/layer.tar — each layer as a tar
#
# We extract manifest.json and the config JSON, which together give us:
#   - Layer diff IDs (uncompressed, from config.rootfs.diff_ids)
#   - Layer tar paths (from manifest.json Layers array, in order)
#   - Architecture, OS, entrypoint, cmd, created timestamp
#   - The mapping between layer order and diff IDs
# ===========================================================================

# Extract OCI metadata from an image tarball without any network calls.
#
# Returns a record with everything the resolver agent would have provided:
#   {
#     config_digest: "sha256:...",
#     layers: [ { order: 0, diff_id: "sha256:...", tar_path: "..." }, ... ],
#     os: "linux",
#     arch: "amd64",
#     created: "2025-01-01T...",
#     entrypoint: "[/bin/myapp]",
#     cmd: "",
#     diff_id_to_order: { "sha256:abc...": 0, "sha256:def...": 1 }
#   }
#
# The `diff_id_to_order` map is the key to joining syft's layer
# attributions (which use uncompressed diff IDs) with the layer
# ordering in the image. When combined with the upload digest from
# skopeo, this gives the linker everything it needs to write
# OCIArtifact, OCILayer, and CONTAINS edges in one pass.
export def extract-oci-metadata [
    tarball_path: path
]: nothing -> record {
    let real_path = (^readlink -f $tarball_path | str trim)

    log-debug $"Extracting OCI metadata from ($real_path)" --component "oci"

    # Extract manifest.json from the tarball.
    # It's always at the root of a docker-archive tar.
    let manifest_json = try {
        ^tar -xf $real_path -O "manifest.json" | from json
    } catch {|e|
        log-warn $"Could not extract manifest.json from ($tarball_path): ($e.msg)" --component "oci"
        return { success: false }
    }

    # docker-archive manifest.json is an array; take the first (and
    # usually only) entry.
    let manifest_entry = ($manifest_json | first | default null)
    if $manifest_entry == null {
        log-warn $"Empty manifest.json in ($tarball_path)" --component "oci"
        return { success: false }
    }

    # Extract the config JSON. The Config field in manifest.json
    # points to the config blob filename inside the tar.
    let config_filename = ($manifest_entry.Config? | default "")
    if ($config_filename | is-empty) {
        log-warn $"No Config entry in manifest.json for ($tarball_path)" --component "oci"
        return { success: false }
    }

    let config = try {
        ^tar -xf $real_path -O $config_filename | from json
    } catch {|e|
        log-warn $"Could not extract config ($config_filename) from ($tarball_path): ($e.msg)" --component "oci"
        return { success: false }
    }

    # Compute the config digest from the raw bytes (not from the parsed
    # JSON, which would change formatting). This matches what registries
    # use as the config digest.
    let config_digest = try {
        ^tar -xf $real_path -O $config_filename | hash sha256 | $"sha256:($in)"
    } catch {
        ""
    }

    # diff_ids from the config — these are the uncompressed layer digests,
    # in the same order as the layer stack. This is what syft references
    # in its layer attribution properties.
    let diff_ids = ($config.rootfs?.diff_ids? | default [])

    # Layer tar paths from manifest.json, in order.
    let layer_paths = ($manifest_entry.Layers? | default [])

    # Build the layer list with order index + diff ID.
    # The diff_ids array and Layers array are in the same order
    # per the OCI image spec.
    let layers = ($diff_ids | enumerate | each {|entry|
        {
            order: $entry.index
            diff_id: $entry.item
            tar_path: ($layer_paths | get -o $entry.index | default "")
        }
    })

    # Build a lookup map: diff_id → layer order.
    # This is what the linker uses to join syft's layer attributions
    # (keyed on diff_id) to the ordered layer stack.
    mut diff_id_to_order = {}
    for layer in $layers {
        $diff_id_to_order = ($diff_id_to_order | insert $layer.diff_id $layer.order)
    }

    let repo_tags = ($manifest_entry.RepoTags? | default [])

    {
        success: true
        config_digest: $config_digest
        layers: $layers
        diff_id_to_order: $diff_id_to_order
        repo_tags: $repo_tags
        os: ($config.os? | default "linux")
        arch: ($config.architecture? | default "")
        created: ($config.created? | default "")
        entrypoint: ($config.config?.Entrypoint? | default [] | str join " ")
        cmd: ($config.config?.Cmd? | default [] | str join " ")
    }
}
