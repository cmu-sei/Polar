# ---------------------------------------------------------------------------
# OCI image utilities
# Reusable functions for building, scanning, and uploading OCI images.
# These are generic — they don't know about Polar's specific images.
# Your project-specific pipeline script defines WHAT to build;
# these functions define HOW.
# ---------------------------------------------------------------------------
use ./hashing.nu *
use logging.nu *

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
        log info $"Logging into ($cred.registry)" --component "oci"
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
