# ---------------------------------------------------------------------------
# OCI image utilities
# Reusable functions for building, scanning, and uploading OCI images.
# These are generic — they don't know about Polar's specific images.
# Your project-specific pipeline script defines WHAT to build;
# these functions define HOW.
# ---------------------------------------------------------------------------
use ./hashing.nu *
use ./logging.nu *
use ./events.nu [emit-container-image-created emit-artifact-produced]

# ===========================================================================
# Image building (Nix)
# ===========================================================================

# Build a single Nix image derivation.
#
# Builds the image, extracts OCI metadata from the resulting tarball,
# and emits a ContainerImageCreated event immediately. The image artifact
# exists the moment nix build completes — we don't wait for an upload,
# a resolver discovery, or an SBOM scan to announce it.
#
# The emitted event carries enough for the build processor to create the
# ContainerImage node and its OCILayer children right away:
#   - Config digest (image identity before registry push)
#   - Ordered layer list with uncompressed diff IDs
#   - OS, arch, entrypoint, cmd
#
# Returns a record including the tarball path AND the extracted
# oci_metadata, so downstream callers don't need to re-extract it.
#
# `flake_ref` is the full flake reference, e.g.:
#   .#polarPkgs.cassini.cassiniImage
#
# `link_name` is the result symlink name, e.g. "cassini"
export def nix-build-image [
    flake_ref: string
    link_name: string
]: nothing -> record {
    log-info $"Building ($link_name) from ($flake_ref)" --component oci

    let result = (nix build --quiet $flake_ref -o $link_name | complete)

    if $result.exit_code != 0 {
        let msg = ($result.stderr? | default $result.stdout | str trim)
        log-warn $"nix build failed for ($link_name): ($msg)" --component oci
        return {
            success: false
            link_name: $link_name
            flake_ref: $flake_ref
            tarball: "",
            oci_metadata: { success: false}
        }
    }

    let tarball = (readlink -f $link_name | str trim)

    if ($tarball | path exists) != true {
        log-warn $"nix build produced no output for ($link_name)" --component oci
        return {
            success: false
            link_name: $link_name
            flake_ref: $flake_ref
            tarball: "",
            oci_metadata: { success: false}
        }
    }

    let oci_metadata = (extract-oci-metadata $tarball)
    let tarball_hash = (content-hash-file $tarball)

    # Emit ContainerImageCreated — the earliest possible announcement.
    # The build processor can create ContainerImage + OCILayer nodes from
    # this alone, without waiting for a registry push or resolver discovery.
    if ($oci_metadata | get --optional success | default false) {
        ( emit-container-image-created
            $link_name
            $tarball_hash
            $oci_metadata.config_digest
            $oci_metadata.layers
            --os $oci_metadata.os
            --arch $oci_metadata.arch
            --created $oci_metadata.created
            --entrypoint $oci_metadata.entrypoint
            --cmd $oci_metadata.cmd
            --repo_tags ($oci_metadata.repo_tags? | default [])
        )
        log-info $"($link_name): ($oci_metadata.layers | length) layers, ($oci_metadata.os)/($oci_metadata.arch)" --component oci
    } else {
        log-warn $"($link_name): built successfully but could not extract OCI metadata" --component oci
    }

    log-info $"Built ($link_name): ($tarball)" --component oci
    {
        success: true
        link_name: $link_name
        flake_ref: $flake_ref
        tarball: $tarball
        oci_metadata: $oci_metadata
    }
}

# ===========================================================================
# Registry operations
# ===========================================================================

# Log in to one or more OCI registries via skopeo.
# Takes a list of { registry, username, password } records.
# Stops on first failure.
export def registry-login [credentials: list<record>] {
    for cred in $credentials {
        log-info $"Logging into ($cred.registry)" --component oci
        let result = (
            skopeo login
                --username $cred.username
                --password $cred.password
                $cred.registry
            | complete
        )
        if $result.exit_code != 0 {
            error make {msg: $"Failed to log into ($cred.registry): ($result.stderr? | default $result.stdout | str trim)"}
        }
    }
}

# Upload an image tarball to a remote registry via skopeo.
#
# Returns a record with the remote ref and the digest skopeo reports.
# The digest is extracted from skopeo's stdout/stderr when available.
export def upload-image [
    tarball_path: path
    remote_ref: string
    --name: string = ""
]: nothing -> record {
    let label     = if ($name | is-not-empty) { $name } else { $remote_ref }
    let real_path = (readlink -f $tarball_path | str trim)

    log-info $"Uploading ($label) -> ($remote_ref)" --component oci

    let result = (skopeo copy $"docker-archive:($real_path)" $remote_ref | complete)

    if $result.exit_code != 0 {
        let msg = ($result.stderr? | default $result.stdout | str trim)
        error make {msg: $"Upload failed for ($label): ($msg)"}
    }

    # Try to extract the digest from skopeo output.
    # skopeo copy sometimes prints "Copying ... digest: sha256:abc..."
    let output = ($result.stdout? | default "" | append ($result.stderr? | default "") | str join "\n")
    let digest = ($output
        | parse --regex 'sha256:[a-f0-9]{64}'
        | get --optional 0
        | default {capture0: ""}
        | get capture0
    )

    log-info $"Uploaded ($label)" --component oci
    {remote_ref: $remote_ref, digest: $digest, name: $label}
}

# Sign a container image in a registry using cosign.
#
# The image must already be pushed — cosign signs by digest in the registry,
# not from a local tarball. The signature is stored as a tag in the same
# registry (cosign's default behavior).
#
# Returns a record with the signature status.
export def sign-image [
    remote_ref: string
    --key: string = ""
    --name: string = ""
]: nothing -> record {
    let label       = if ($name | is-not-empty) { $name } else { $remote_ref }
    let signing_key = if ($key | is-not-empty) { $key } else { ($env.COSIGN_KEY? | default "") }

    if ($signing_key | is-empty) {
        log-warn $"No signing key available, skipping signature for ($label)" --component oci
        return {success: false, reason: "no signing key configured"}
    }

    log-info $"Signing ($label)" --component oci

    let result = (cosign sign --key $signing_key --yes $remote_ref | complete)

    if $result.exit_code != 0 {
        let msg = ($result.stderr? | default $result.stdout | str trim)
        log-warn $"cosign sign failed for ($label): ($msg)" --component oci
        return {success: false, reason: $msg}
    }

    log-info $"Signed ($label)" --component oci
    {success: true}
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
#   manifest.json        — array of [{ Config, RepoTags, Layers }]
#   <config_hash>.json   — the OCI image config
#   <layer_hash>/layer.tar — each layer as a tar
# ===========================================================================

# Extract OCI metadata from an image tarball without any network calls.
#
# Returns:
#   {
#     config_digest: "sha256:...",
#     layers: [ { order: 0, diff_id: "sha256:...", tar_path: "..." }, ... ],
#     os: "linux", arch: "amd64", created: "...", entrypoint: "...", cmd: "",
#     diff_id_to_order: { "sha256:abc...": 0, ... },
#     repo_tags: [...]
#   }
#
# The `diff_id_to_order` map joins syft's layer attributions (keyed on
# uncompressed diff_id) with the ordered layer stack.
export def extract-oci-metadata [tarball_path: path]: nothing -> record {
    let real_path = (readlink -f $tarball_path | str trim)
    log-debug $"Extracting OCI metadata from ($real_path)" --component oci

    let manifest_json = try {
        tar -xf $real_path -O "manifest.json" | from json
    } catch {|e|
        log-warn $"Could not extract manifest.json from ($tarball_path): ($e.msg)" --component oci
        return {success: false}
    }

    let manifest_entry = ($manifest_json | first | default null)
    if $manifest_entry == null {
        log-warn $"Empty manifest.json in ($tarball_path)" --component oci
        return {success: false}
    }

    let config_filename = ($manifest_entry.Config? | default "")
    if ($config_filename | is-empty) {
        log-warn $"No Config entry in manifest.json for ($tarball_path)" --component oci
        return {success: false}
    }

    let config = try {
        tar -xf $real_path -O $config_filename | from json
    } catch {|e|
        log-warn $"Could not extract config ($config_filename) from ($tarball_path): ($e.msg)" --component oci
        return {success: false}
    }

    let config_digest = try {
        tar -xf $real_path -O $config_filename | hash sha256 | $"sha256:($in)"
    } catch { "" }

    let diff_ids    = ($config.rootfs?.diff_ids? | default [])
    let layer_paths = ($manifest_entry.Layers? | default [])

    let layers = ($diff_ids | enumerate | each {|entry| {
        order:    $entry.index
        diff_id:  $entry.item
        tar_path: ($layer_paths | get --optional $entry.index | default "")
    } })

    mut diff_id_to_order = {}
    for layer in $layers {
        $diff_id_to_order = ($diff_id_to_order | insert $layer.diff_id $layer.order)
    }

    {
        success:          true
        config_digest:    $config_digest
        layers:           $layers
        diff_id_to_order: $diff_id_to_order
        repo_tags:        ($manifest_entry.RepoTags? | default [])
        os:               ($config.os? | default linux)
        arch:             ($config.architecture? | default "")
        created:          ($config.created? | default "")
        entrypoint:       ($config.config?.Entrypoint? | default [] | str join " ")
        cmd:              ($config.config?.Cmd? | default [] | str join " ")
    }
}
