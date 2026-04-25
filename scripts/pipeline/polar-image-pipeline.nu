#!/usr/bin/env nu
# polar-image-pipeline.nu
#
# Builds, scans, and uploads all Polar container images.
#
# This is the project-specific script. It defines WHAT to build
# (the image manifest below). The HOW lives in pipeline/core.nu
# and the OCI functions therein.
#
# Usage:
#   nu polar-image-pipeline.nu
#   nu polar-image-pipeline.nu --tag abc1234
#   nu polar-image-pipeline.nu --filter cassini
#   nu polar-image-pipeline.nu --skip-upload    # build + scan only
#   nu polar-image-pipeline.nu --skip-sbom      # build + upload, no scanning

use ./core.nu *

const COMPONENT = "polar-images"

# ---------------------------------------------------------------------------
# Image manifest
#
# Single source of truth for what images exist in this project.
# Each entry maps a human-readable name to its nix flake reference,
# the registry image name, and the root purl of the package inside.
#
# Adding a new image to the pipeline = adding a row here. Nothing else.
# The loop at the bottom handles everything.
#
# `root_purl` is the purl of the primary binary inside the image.
# This is the join key to the source-level SBOM's dependency tree.
# If you don't have it (e.g., third-party base images), leave it empty
# and the image.linked event won't include package linkage.
# ---------------------------------------------------------------------------
def image-manifest []: nothing -> list<record> {
    [
        { name: "cassini",                flake: ".#cassiniImage",            image: "cassini",                    root_purl: "pkg:cargo/cassini@0.1.0" }
        { name: "gitlab-observer",        flake: ".#gitlabObserverImage",       image: "polar-gitlab-observer",      root_purl: "pkg:cargo/gitlab-observer@0.1.0" }
        { name: "gitlab-consumer",        flake: ".#gitlabConsumerImage",       image: "polar-gitlab-consumer",      root_purl: "pkg:cargo/gitlab-consumer@0.1.0" }
        { name: "kube-observer",          flake: ".#kubeObserverImage",         image: "polar-kube-observer",        root_purl: "pkg:cargo/kube-observer@0.1.0" }
        { name: "kube-consumer",          flake: ".#kubeConsumerImage",         image: "polar-kube-consumer",        root_purl: "pkg:cargo/kube-consumer@0.1.0" }
        { name: "git-observer",           flake: ".#gitObserverImage",               image: "polar-git-observer",         root_purl: "pkg:cargo/git-repo-observer@0.1.0" }
        { name: "git-consumer",           flake: ".#gitConsumerImage",               image: "polar-git-consumer",         root_purl: "" }
        { name: "git-scheduler",          flake: ".#gitSchedulerImage",              image: "polar-git-scheduler",        root_purl: "" }
        { name: "linker",                 flake: ".#provenanceLinkerImage",          image: "polar-linker-agent",         root_purl: "pkg:cargo/provenance-linker@0.1.0" }
        { name: "resolver",              flake: ".#provenanceResolverImage",        image: "polar-resolver-agent",       root_purl: "pkg:cargo/provenance-resolver@0.1.0" }
    ]
}

# Build registry ref templates from environment variables.
# Returns a list of strings with {tag} as the placeholder.
#
# Each entry becomes a skopeo destination like:
#   docker://registry.example.com/org/polar-kube-observer:{tag}
def registry-refs [image_name: string]: nothing -> list<string> {
    mut refs = []

    let ci_registry_image = ($env.CI_REGISTRY_IMAGE? | default "")
    if ($ci_registry_image | is-not-empty) {
        $refs = ($refs | append $"docker://($ci_registry_image)/($image_name):{tag}")
    }

    let azure_registry = ($env.AZURE_REGISTRY? | default "")
    if ($azure_registry | is-not-empty) {
        $refs = ($refs | append $"docker://($azure_registry)/($image_name):{tag}")
    }

    $refs
}

# ---------------------------------------------------------------------------
# Integrate with the cargo build pipeline's SBOM lookup.
#
# If the cargo build pipeline ran first and wrote its sbom_lookup to a
# JSON file, load it so we can pass source-level SBOM content hashes
# to the image.linked events. This closes the chain:
#   (OCIArtifact) -[:BUILT_FROM]-> (Package) <-[:DESCRIBES]- (Sbom)
#
# If the file doesn't exist (images built standalone), we proceed
# without source SBOM linkage.
# ---------------------------------------------------------------------------
def load-sbom-lookup [artifact_dir: path]: nothing -> record {
    let lookup_path = ($artifact_dir | path join "sbom-lookup.json")
    if ($lookup_path | path exists) {
        try {
            open $lookup_path
        } catch {
            log-warn "Could not parse sbom-lookup.json, proceeding without source SBOM linkage" --component $COMPONENT
            {}
        }
    } else {
        {}
    }
}

def main [
    --tag: string = ""             # Image tag (default: CI_COMMIT_SHORT_SHA or "latest")
    --filter: string = ""          # Only build images matching this name
    --artifact-dir: path = "pipeline-out"
    --skip-upload                  # Build and scan only, don't push
    --skip-sbom                    # Build and upload, skip syft scanning
] {
    let cassini_job_id = start-cassini-daemon

    mkdir -v $artifact_dir

    let image_tag = if ($tag | is-not-empty) {
        $tag
    } else {
        ($env.CI_COMMIT_SHORT_SHA? | default "latest")
    }

    let manifest = if ($filter | is-not-empty) {
        let found = image-manifest | where { $in.name | str contains $filter }
        log-debug $"Using image manifest \n ($found)"
        $found
    } else {
        let $m = image-manifest
        log-debug $"Using image manifest \n ($m)"
    }

    if ($manifest | is-empty) {
        log-info $"No images match filter '($filter)'" --component $COMPONENT
        # stop-cassini-daemon $cassini_job_id
        log-info $"Available manifests:\n (image-manifest)"
        exit 0
    }

    log-info $"Building ($manifest | length) image\(s\) with tag ($image_tag)" --component $COMPONENT

    # Load source-level SBOM lookup if available from a prior cargo build pipeline run.
    let sbom_lookup = (load-sbom-lookup $artifact_dir)

    # Log into registries before starting uploads.
    if not $skip_upload {
        mut creds = []

        let ci_user = ($env.CI_REGISTRY_USER? | default "")
        let ci_pass = ($env.CI_REGISTRY_PASSWORD? | default "")
        let ci_reg = ($env.CI_REGISTRY? | default "")
        if ($ci_user | is-not-empty) and ($ci_reg | is-not-empty) {
            $creds = ($creds | append { registry: $ci_reg, username: $ci_user, password: $ci_pass })
        }

        let acr_user = ($env.ACR_USERNAME? | default "")
        let acr_pass = ($env.ACR_TOKEN? | default "")
        let acr_reg = ($env.AZURE_REGISTRY? | default "")
        if ($acr_user | is-not-empty) and ($acr_reg | is-not-empty) {
            $creds = ($creds | append { registry: $acr_reg, username: $acr_user, password: $acr_pass })
        }

        if ($creds | is-not-empty) {
            registry-login $creds
        }
    }

    # Process each image: build → scan → upload → emit.
    let results = ($manifest | each {|entry|
        log-info $"--- ($entry.name) ---" --component $COMPONENT

        # Resolve source-level SBOM hash for this image's root package.
        # The sbom_lookup maps package names to { content_hash, root_purl, ... }.
        # We try to match on the root_purl's package name portion.
        let source_sbom_hash = if ($entry.root_purl | is-not-empty) {
            # Extract the crate name from the purl: pkg:cargo/my-crate@0.1.0 → my-crate
            let crate_name = ($entry.root_purl | parse "pkg:cargo/{name}@{ver}" | get -o 0 | default { name: "" } | get name)
            let lookup_entry = ($sbom_lookup | get -o $crate_name | default null)
            if $lookup_entry != null { $lookup_entry.content_hash? | default "" } else { "" }
        } else {
            ""
        }

        if $skip_upload and $skip_sbom {
            # Just build.
            let build = (nix-build-image $entry.flake $entry.name)
            { success: $build.success, image_name: $entry.name, uploads: [] }
        } else if $skip_upload {
            # Build + scan, no upload.
            let build = (nix-build-image $entry.flake $entry.name)
            if not $build.success { return { success: false, image_name: $entry.name } }

            let sbom = (generate-image-sbom $build.tarball $artifact_dir --name $entry.name)
            if $sbom.success {
                process-image-sbom $sbom.path $entry.name
            }
            { success: true, image_name: $entry.name, uploads: [] }
        } else if $skip_sbom {
            # Build + upload, no scan.
            let build = (nix-build-image $entry.flake $entry.name)
            if not $build.success { return { success: false, image_name: $entry.name } }

            let registries = (registry-refs $entry.image)
            let uploads = ($registries | each {|template|
                let remote_ref = ($template | str replace "{tag}" $image_tag)
                upload-image $build.tarball $remote_ref --name $entry.name
            })
            { success: true, image_name: $entry.name, uploads: $uploads }
        } else {
            # Full pipeline: build → scan → upload → emit.
            let registries = (registry-refs $entry.image)
            build-scan-upload $entry.flake $entry.name $image_tag $registries $artifact_dir --root_purl $entry.root_purl --sbom_content_hash $source_sbom_hash
        }
    })

    # Summary.
    let succeeded = ($results | where success == true | length)
    let failed = ($results | where success == false | length)

    log-info $"($succeeded) image\(s\) succeeded, ($failed) failed" --component $COMPONENT

    if $failed > 0 {
        let failures = ($results | where success == false | get image_name | str join ", ")
        log-warn $"Failed images: ($failures)" --component $COMPONENT
    }

    stop-cassini-daemon $cassini_job_id
}
