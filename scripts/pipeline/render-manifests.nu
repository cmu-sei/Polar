#!/usr/bin/env nu
# render.nu
#
# Renders all Dhall manifests under a root directory into YAML files,
# then pushes each service's rendered manifests as a separate OCI artifact
# to a registry using oras.
#
# Rendered YAML lands in-place next to the .dhall sources so that relative
# Dhall import paths resolve correctly. The conf directory is rendered the
# same way as service directories — the distinction is only relevant to
# the caller when deciding what to apply to the cluster.
#
# Usage:
#   nu render.nu <dhall_root> <registry> <tag>
#
# Example:
#   nu render.nu ./deploy localhost:31500 latest
#
# Each service directory is pushed as a separate artifact:
#   localhost:31500/manifests/<service>:<tag>
#
# OrbStack / local registry note:
#   Add to ~/.docker/daemon.json before pushing:
#     { "insecure-registries": ["localhost:31500"] }
#   Then restart OrbStack.

const ARTIFACT_TYPE = "application/vnd.polar.k8s-manifests.v1"
const MEDIA_TYPE    = "application/yaml"

# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------

# Render all .dhall files in a single directory to .yaml in the same directory,
# then generate a kustomization.yaml that lists every rendered file.
# Non-.dhall files are silently ignored.
# Returns a list of { src, out, success } records. The kustomization.yaml is
# not included in the returned list — it is a derived artifact, not a rendered
# one — but it will be present in the directory for oras to pick up.
#
# Pass --no-kustomization for directories like `flux` that contain CRDs rather
# than kustomize-managed manifests and must not have a kustomization.yaml.
def render-dhall-dir [
    dhall_dir: path
    --no-kustomization   # Skip kustomization.yaml generation
]: nothing -> list<record> {
    let dhall_files = (
        ls $dhall_dir
        | where type == file
        | where { $in.name | str ends-with ".dhall" }
    )

    if ($dhall_files | is-empty) {
        print $"[WARN] no .dhall files in ($dhall_dir), skipping"
        return []
    }

    let results = ($dhall_files | each {|file|
        let src      = $file.name
        let stem     = ($src | path basename | str replace --regex '\.dhall$' '')
        let yaml_out = ($dhall_dir | path join $"($stem).yaml")

        print $"[INFO] converting ($src | path basename) -> ($yaml_out | path basename)"

        dhall-to-yaml --documents --file $src | save --force $yaml_out

        let ok = ($env.LAST_EXIT_CODE == 0)
        if not $ok {
            print $"[ERROR] dhall-to-yaml failed for ($src)"
        }

        { src: $src, out: $yaml_out, success: $ok }
    })

    # Generate kustomization.yaml from the successfully rendered files.
    # kustomize-controller requires this file at spec.path inside the artifact
    # or reconciliation fails. We derive it from actual output rather than
    # maintaining it by hand so it never drifts from what was rendered.
    # Skipped for directories like `flux` that hold CRDs, not kustomize resources.
    if not $no_kustomization {
        let rendered_names = (
            $results
            | where success == true
            | get out
            | each { path basename }
        )

        if ($rendered_names | is-not-empty) {
            let kustomization_yaml = ($dhall_dir | path join "kustomization.yaml")
            {
                apiVersion: "kustomize.config.k8s.io/v1beta1"
                kind: "Kustomization"
                resources: $rendered_names
            } | to yaml | save --force $kustomization_yaml

            print $"[INFO] wrote kustomization.yaml with ($rendered_names | length) resource\(s\)"
        }
    }

    $results
}

# ---------------------------------------------------------------------------
# OCI push
# ---------------------------------------------------------------------------

# Push all .yaml files in a directory as a single OCI artifact.
#
# Each file is passed to oras as "<path>:<media-type>" so the registry
# stores them with the correct content type descriptor. oras push does
# not accept directories — we glob the files explicitly.
#
# The git revision annotation is embedded as org.opencontainers.image.revision
# so that Flux's source-controller surfaces it in
# Kustomization.status.lastAppliedOriginRevision, closing the lead time chain
# back to the commit that produced these manifests.
#
# --plain-http is required for the local insecure registry. Remove it (or
# gate it on the registry host) when pushing to a TLS-enabled registry.
def push-manifests [
    dir:      path    # Directory containing rendered .yaml files
    registry: string  # Registry host, e.g. "localhost:31500"
    repo:     string  # Repository name under the registry, e.g. "manifests/polar"
    tag:      string  # Tag to push, e.g. "latest" or a commit SHA
    --revision: string = ""  # Value for org.opencontainers.image.revision
    --source:   string = ""  # Value for org.opencontainers.image.source
]: nothing -> record {
    let yaml_files = (
        ls $dir
        | where type == file
        | where { $in.name | str ends-with ".yaml" }
        | where { ($in.name | path basename) != "kustomization.yaml" }
    )

    if ($yaml_files | is-empty) {
        print $"[WARN] no .yaml files in ($dir), skipping push"
        return { success: false, ref: "", digest: "" }
    }

    let ref = $"($registry)/($repo):($tag)"

    # Build the file argument list. oras push expects each file as
    # "<path>:<media-type>" when you want to set the layer media type.
    # kustomization.yaml is included explicitly — kustomize-controller
    # requires it at the root of the artifact path to locate resources.
    let kustomization_file = ($dir | path join "kustomization.yaml")
    if not ($kustomization_file | path exists) {
        print $"[WARN] no kustomization.yaml in ($dir) — kustomize-controller will fail to reconcile"
    }

    # Pack rendered yamls into a gzip-compressed tarball before pushing.
    # source-controller requires gzip-compressed OCI layers — raw file blobs
    # with application/yaml media type are not accepted.
    let tarball = ($dir | path join "manifests.tar.gz")
    let yaml_names = ($yaml_files | get name | each { path basename })
    let kustomization_name = "kustomization.yaml"

    ^tar -czf $tarball -C ($dir | into string) ...$yaml_names $kustomization_name

    let file_args = [$"($tarball):application/vnd.oci.image.layer.v1.tar+gzip"]

    # Build annotation arguments. We always set created; revision and
    # source are included only when provided so the manifest stays clean.
    let created = (date now | format date "%Y-%m-%dT%H:%M:%SZ")
    mut annotation_args = [
        "--annotation" $"org.opencontainers.image.created=($created)"
    ]
    if ($revision | is-not-empty) {
        $annotation_args = ($annotation_args | append [
            "--annotation" $"org.opencontainers.image.revision=($revision)"
        ])
    }
    if ($source | is-not-empty) {
        $annotation_args = ($annotation_args | append [
            "--annotation" $"org.opencontainers.image.source=($source)"
        ])
    }

    print $"[INFO] pushing ($yaml_files | length) file\(s\) to ($ref)"

    let result = (
        ^oras push
            --plain-http
            --artifact-type $ARTIFACT_TYPE
            ...$annotation_args
            $ref
            ...$file_args
        | complete
    )

    rm --force $tarball

    if $result.exit_code != 0 {
        let msg = ($result.stderr | default $result.stdout | str trim)
        print $"[ERROR] oras push failed for ($ref): ($msg)"
        return { success: false, ref: $ref, digest: "" }
    }

    # oras prints the digest on stdout as "Digest: sha256:..."
    let digest = (
        $result.stdout
        | lines
        | where { $in | str starts-with "Digest:" }
        | first
        | str replace "Digest: " ""
        | str trim
    )

    print $"[INFO] pushed ($ref) digest ($digest)"
    { success: true, ref: $ref, digest: $digest }
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

# Directories that are rendered in-place but never pushed to the registry.
# conf  — supporting configuration, consumed locally
# flux  — Flux CRDs applied directly via kubectl, not reconciled by kustomize-controller
const RENDER_ONLY_DIRS = ["conf", "flux"]

# The single directory whose rendered output is packaged as an OCI artifact
# and reconciled by Flux. Everything else is a supporting concern.
const MANIFEST_DIR = "polar"

def main [
    dhall_root: path   # Root of the deployment tree, e.g. src/deploy/local
    registry:   string # Registry host:port, e.g. "localhost:31500"
    tag:        string # OCI tag, e.g. "latest" or a git SHA
    --revision: string = ""  # org.opencontainers.image.revision annotation (git SHA)
    --source:   string = ""  # org.opencontainers.image.source annotation (repo URL)
    --skip-push              # Render all dirs but do not push to registry
] {
    if not ($dhall_root | path exists) {
        error make { msg: $"dhall root '($dhall_root)' does not exist" }
    }

    print $"[INFO] dhall root: ($dhall_root)"
    print $"[INFO] registry:   ($registry)"
    print $"[INFO] tag:        ($tag)"

    # ---- Render-only dirs (conf, flux) ----
    # Rendered in-place so local tooling and kubectl can consume them.
    # Never pushed to the registry.
    for name in $RENDER_ONLY_DIRS {
        let dir = ($dhall_root | path join $name)
        if not ($dir | path exists) { continue }
        print $"[INFO] --- ($name) \(render only\) ---"
        let results = (render-dhall-dir $dir --no-kustomization)
        let failures = ($results | where success == false)
        if ($failures | is-not-empty) {
            error make { msg: $"rendering failed in ($name): ($failures | get src | str join ', ')" }
        }
        print $"[INFO] ($name): ($results | length) file\(s\) rendered"
    }

    # ---- Manifest dir (polar) ----
    # Rendered and pushed as a single OCI artifact for Flux to reconcile.
    let manifest_dir = ($dhall_root | path join $MANIFEST_DIR)
    if not ($manifest_dir | path exists) {
        error make { msg: $"manifest directory '($manifest_dir)' does not exist" }
    }

    print $"[INFO] --- ($MANIFEST_DIR) ---"
    let render_results = (render-dhall-dir $manifest_dir)
    let failures = ($render_results | where success == false)
    if ($failures | is-not-empty) {
        error make { msg: $"rendering failed in ($MANIFEST_DIR): ($failures | get src | str join ', ')" }
    }
    print $"[INFO] ($MANIFEST_DIR): ($render_results | length) file\(s\) rendered"

    if $skip_push {
        print "[INFO] --skip-push set, skipping registry push"
        print "[INFO] done"
        return
    }

    let push_result = (
        push-manifests $manifest_dir $registry $"manifests/($MANIFEST_DIR)" $tag
            --revision $revision
            --source $source
    )

    if not $push_result.success {
        error make { msg: $"push failed for ($MANIFEST_DIR)" }
    }

    print ""
    print $"[INFO] pushed ($push_result.ref)"
    print $"[INFO] digest ($push_result.digest)"
    print "[INFO] done"
}
