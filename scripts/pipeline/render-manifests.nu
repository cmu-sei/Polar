#!/usr/bin/env nu
# render-manifests.nu
#
# Renders all Dhall manifests under a root directory into YAML files and
# pushes them as a single OCI artifact for Flux to reconcile.
#
# Render-only directories (conf, flux) are rendered in-place next to their
# Dhall sources — their YAML output is consumed by local tooling or embedded
# into ConfigMaps, and their relative import paths must stay intact. They are
# never included in the OCI artifact.
#
# Manifest directories (core, infra, agents) are rendered to a separate
# output_dir, structured so that the three Flux Kustomizations defined in
# flux-kustomization.dhall can reconcile each layer independently:
#
#   <output_dir>/
#     core/
#       cert-issuer.yaml
#       kustomization.yaml
#     infra/
#       cassini.yaml
#       neo4j.yaml
#       jaeger.yaml
#       storage.yaml
#       kustomization.yaml
#     agents/
#       agents.yaml
#       polar.yaml
#       kustomization.yaml
#
# Usage:
#   nu render-manifests.nu <dhall_root> <output_dir> <registry> <tag>
#
# Example:
#   nu render-manifests.nu ./local ./manifests localhost:31500 latest
#
# The artifact is pushed as:
#   <registry>/manifests/polar:<tag>
#
# OrbStack / local registry note:
#   Add to ~/.docker/daemon.json before pushing:
#     { "insecure-registries": ["localhost:31500"] }
#   Then restart OrbStack.

const ARTIFACT_TYPE = "application/vnd.polar.k8s-manifests.v1"
const MEDIA_TYPE    = "application/yaml"

# Directories rendered in-place but never pushed to the registry.
# conf — config files embedded into ConfigMaps; must stay next to Dhall sources.
# flux — Flux CRDs applied directly via kubectl, not reconciled by kustomize-controller.
const RENDER_ONLY_DIRS = ["conf", "flux"]

# Mapping from source subdirectory name under local/polar/ to the target
# subdirectory name inside output_dir. The keys are the Dhall source dirs;
# the values are the artifact layer names the Flux Kustomizations reference.
# Order here determines nothing — apply ordering is declared in flux-kustomization.dhall.
const MANIFEST_LAYERS = {
    core:   ["cert-issuer"]        # Files whose stems map to this layer
    infra:  ["cassini", "neo4j", "jaeger", "storage"]
    agents: ["agents", "polar"]
}

# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------

# Render a single .dhall file to a .yaml file at the given output path.
# Returns a { src, out, success } record.
def render-dhall-file [
    src:     path  # Source .dhall file
    out:     path  # Destination .yaml file
]: nothing -> record {
    print $"[INFO] converting ($src | path basename) -> ($out)"
    dhall-to-yaml --documents --file $src | save --force $out
    let ok = ($env.LAST_EXIT_CODE == 0)
    if not $ok {
        print $"[ERROR] dhall-to-yaml failed for ($src)"
    }
    { src: $src, out: $out, success: $ok }
}

# Render all .dhall files in a directory in-place (YAML lands next to the
# source). Used for render-only directories where relative import paths must
# stay intact. Generates no kustomization.yaml.
def render-dhall-dir-inplace [
    dhall_dir: path
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

    $dhall_files | each {|file|
        let stem = ($file.name | path basename | str replace --regex '\.dhall$' '')
        let out  = ($dhall_dir | path join $"($stem).yaml")
        render-dhall-file $file.name $out
    }
}

# Render .dhall files from a source directory into a destination directory,
# then write a kustomization.yaml listing every successfully rendered file.
# The source directory is not modified — all output goes to dest_dir.
def render-dhall-dir-to [
    src_dir:  path  # Source directory containing .dhall files
    dest_dir: path  # Destination directory for rendered .yaml files
]: nothing -> list<record> {
    let dhall_files = (
        ls $src_dir
        | where type == file
        | where { $in.name | str ends-with ".dhall" }
    )

    if ($dhall_files | is-empty) {
        print $"[WARN] no .dhall files in ($src_dir), skipping"
        return []
    }

    mkdir $dest_dir

    let results = ($dhall_files | each {|file|
        let stem = ($file.name | path basename | str replace --regex '\.dhall$' '')
        let out  = ($dest_dir | path join $"($stem).yaml")
        render-dhall-file $file.name $out
    })

    # Write kustomization.yaml from the successfully rendered files.
    # kustomize-controller requires this at spec.path inside the artifact.
    # Generated from actual output so it never drifts from what was rendered.
    let rendered_names = (
        $results
        | where success == true
        | get out
        | each { path basename }
    )

    if ($rendered_names | is-not-empty) {
        let kustomization_path = ($dest_dir | path join "kustomization.yaml")
        {
            apiVersion: "kustomize.config.k8s.io/v1beta1"
            kind: "Kustomization"
            resources: $rendered_names
        } | to yaml | save --force $kustomization_path

        print $"[INFO] wrote ($dest_dir | path basename)/kustomization.yaml with ($rendered_names | length) resource\(s\)"
    }

    $results
}

# ---------------------------------------------------------------------------
# OCI push
# ---------------------------------------------------------------------------

# Push the output_dir as a single OCI artifact. The entire directory tree
# (core/, infra/, agents/, each with their kustomization.yaml) is packed into
# a gzip-compressed tarball as required by source-controller.
#
# The git revision annotation is embedded as org.opencontainers.image.revision
# so Flux surfaces it in Kustomization.status.lastAppliedOriginRevision.
def push-manifests [
    output_dir: path    # Root of the rendered artifact tree
    registry:   string  # Registry host, e.g. "localhost:31500"
    repo:       string  # Repository name, e.g. "manifests/polar"
    tag:        string  # Tag, e.g. "latest" or a commit SHA
    --revision: string = ""
    --source:   string = ""
]: nothing -> record {
    let ref     = $"($registry)/($repo):($tag)"
    let tarball = ($output_dir | path join "manifests.tar.gz")

    # Collect all files under output_dir relative to output_dir itself so
    # tar preserves the core/, infra/, agents/ structure inside the archive.
    let all_files = (
        glob ($"($output_dir)/**/*.yaml")
        | each { path relative-to $output_dir }
    )

    if ($all_files | is-empty) {
        print $"[WARN] no .yaml files under ($output_dir), skipping push"
        return { success: false, ref: $ref, digest: "" }
    }

    ^tar -czf $tarball -C ($output_dir | into string) ...$all_files

    let file_args = [$"manifests.tar.gz:application/vnd.oci.image.layer.v1.tar+gzip"]

    let created = (date now | format date "%Y-%m-%dT%H:%M:%SZ")

    let annotation_args = (
        [["--annotation" $"org.opencontainers.image.created=($created)"]]
        | append (if ($revision | is-not-empty) { [["--annotation" $"org.opencontainers.image.revision=($revision)"]] } else { [] })
        | append (if ($source   | is-not-empty) { [["--annotation" $"org.opencontainers.image.source=($source)"]]   } else { [] })
        | flatten
    )

    print $"[INFO] pushing artifact to ($ref)"

    let result = (
        do { cd $output_dir
            oras push --plain-http --artifact-type $ARTIFACT_TYPE ...$annotation_args $ref ...$file_args
        } | complete
    )

    rm --force $tarball

    if $result.exit_code != 0 {
        let msg = ($result.stderr | default $result.stdout | str trim)
        print $"[ERROR] oras push failed for ($ref): ($msg)"
        return { success: false, ref: $ref, digest: "" }
    }

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

def main [
    dhall_root:  path    # Root of the deployment tree, e.g. ./local
    registry:    string  # Registry host:port, e.g. "localhost:31500"
    tag:         string  # OCI tag, e.g. "latest" or a git SHA
    --output_dir:  path = "manifests"   # Output directory for rendered artifact, e.g. ./manifests
    --revision:  string = ""   # org.opencontainers.image.revision annotation
    --source:    string = ""   # org.opencontainers.image.source annotation
    --skip-push                # Render everything but do not push to registry
] {
    if not ($dhall_root | path exists) {
        error make { msg: $"dhall root '($dhall_root)' does not exist" }
    }

    # resolve output_dir to an absolute path before using it
    let output_dir = ($output_dir | path expand --no-symlink)

    print $"[INFO] dhall root:  ($dhall_root)"
    print $"[INFO] output dir:  ($output_dir)"
    print $"[INFO] registry:    ($registry)"
    print $"[INFO] tag:         ($tag)"

    # ---- Render-only dirs (conf, flux) ----
    # Rendered in-place so relative Dhall import paths remain valid and local
    # tooling can consume them. Never included in the OCI artifact.
    for name in $RENDER_ONLY_DIRS {
        let dir = ($dhall_root | path join $name)
        if not ($dir | path exists) { continue }
        print $"[INFO] --- ($name) \(render only, in-place\) ---"
        let results  = (render-dhall-dir-inplace $dir)
        let failures = ($results | where success == false)
        if ($failures | is-not-empty) {
            error make { msg: $"rendering failed in ($name): ($failures | get src | str join ', ')" }
        }
        print $"[INFO] ($name): ($results | length) file\(s\) rendered"
    }

    # ---- Manifest dirs → output_dir ----
    # Each named layer in MANIFEST_LAYERS maps a set of Dhall stems to a
    # subdirectory in output_dir. Files are rendered from local/polar/ into
    # output_dir/<layer>/ so the Flux Kustomizations can reference each layer
    # at its own path.
    let polar_src = ($dhall_root | path join "polar")
    if not ($polar_src | path exists) {
        error make { msg: $"polar source directory '($polar_src)' does not exist" }
    }

    mkdir $output_dir

    # Walk the layer map. For each layer, find the matching .dhall files in
    # polar_src by stem name, render them to output_dir/<layer>/, and generate
    # a kustomization.yaml for that layer.
    mut all_failures: list<record> = []

    for layer in ($MANIFEST_LAYERS | transpose name stems) {
        let layer_name = $layer.name
        let stems      = $layer.stems
        let layer_dir  = ($output_dir | path join $layer_name)

        print $"[INFO] --- ($layer_name) ---"
        mkdir $layer_dir

        mut layer_results: list<record> = []
        for stem in $stems {
            let src = ($polar_src | path join $"($stem).dhall")
            if not ($src | path exists) {
                print $"[WARN] ($stem).dhall not found in ($polar_src), skipping"
                continue
            }
            let out = ($layer_dir | path join $"($stem).yaml")
            $layer_results = ($layer_results | append (render-dhall-file $src $out))
        }

        let failures = ($layer_results | where success == false)
        if ($failures | is-not-empty) {
            $all_failures = ($all_failures | append $failures)
        }

        # Write the per-layer kustomization.yaml
        let rendered_names = (
            $layer_results
            | where success == true
            | get out
            | each { path basename }
        )
        if ($rendered_names | is-not-empty) {
            let kustomization_path = ($layer_dir | path join "kustomization.yaml")
            {
                apiVersion: "kustomize.config.k8s.io/v1beta1"
                kind: "Kustomization"
                resources: $rendered_names
            } | to yaml | save --force $kustomization_path
            print $"[INFO] wrote ($layer_name)/kustomization.yaml with ($rendered_names | length) resource\(s\)"
        }

        print $"[INFO] ($layer_name): ($layer_results | length) file\(s\) rendered"
    }

    if ($all_failures | is-not-empty) {
        error make { msg: $"rendering failed: ($all_failures | get src | str join ', ')" }
    }

    if $skip_push {
        print "[INFO] --skip-push set, skipping registry push"
        print "[INFO] done"
        return
    }

    let push_result = (
        push-manifests $output_dir $registry "manifests/polar" $tag
            --revision $revision
            --source $source
    )

    if not $push_result.success {
        error make { msg: "push failed" }
    }

    print ""
    print $"[INFO] pushed ($push_result.ref)"
    print $"[INFO] digest ($push_result.digest)"
    print "[INFO] done"
}
