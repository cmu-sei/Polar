#!/usr/bin/env bash
set -euo pipefail

# CI is set and true
echo "Running CI job: ${CI_JOB_NAME:-unknown}:${CI_JOB_ID:-unknown} on branch ${CI_COMMIT_BRANCH:-unknown}"

# git won't let nix operate if it doesn't think its safe.
# TODO: We could elimiante this when we fully own the test runner's configuration.
git config --global --add safe.directory "$(pwd)"

mkdir -p output/sbom

# TODO: move this to static-tools.sh
# Run cyclonedx once at the root
cargo cyclonedx --manifest-path src/agents/Cargo.toml -v -f json

# Move all generated SBOMs into a centralized location

find . -type f -name '*.cdx.json' | while read -r sbom; do
  echo "moving $sbom -> to output/sbom"
  mv "$sbom" "output/sbom/$(basename "$sbom")"
done

# # run static tools
# echo "Running static analysis tooling"
# sh scripts/static-tools.sh --manifest-path src/agents/Cargo.toml

# Build core agent binaries
echo Building Polar agents...
nix build --quiet --no-update-lock-file

# Upload binaries to GitLab Package Registry
for file in result/bin/*; do
    if [[ -f "$file" ]]; then
      filename=$(basename "$file")
      curl --header "JOB-TOKEN: $CI_JOB_TOKEN" \
           --upload-file "$file" \
           "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/packages/generic/polar/$CI_COMMIT_SHORT_SHA/$filename"
      echo "Uploaded binary: $filename"
    fi
done

# Build Docker images for all agent components
echo Building Container Images...

echo Building Cassini
nix build --quiet .#polarPkgs.cassini.cassiniImage -o cassini
echo Building the Gitlab Observer
nix build --quiet .#polarPkgs.gitlabAgent.observerImage -o gitlab-observer
echo Building the Gitlab Consumer
nix build --quiet .#polarPkgs.gitlabAgent.consumerImage -o gitlab-consumer
echo Building the Kubernetes Observer
nix build --quiet .#polarPkgs.kubeAgent.observerImage -o kube-observer
echo Building the Kubernetes Consumer
nix build --quiet .#polarPkgs.kubeAgent.consumerImage -o kube-consumer
echo Building the Provenance Linker
nix build --quiet .#polarPkgs.provenance.linkerImage -o linker
echo Building the Provenance Resolver
nix build --quiet .#polarPkgs.provenance.resolverImage -o resolver

# Run vulnerability scan
# vulnix returns nonzero exit codes, so we need to check for ourselves
# TODO: We're getting blocked by firewalls (again), unfreeze this when that gets handled.
# SEE: https://github.com/nix-community/vulnix/issues/79
# set +e
# vulnix --json result > vulnix-report.json
# VULNIX_EXIT=$?
# set -e

# case $VULNIX_EXIT in
#   2) echo "Non-whitelisted vulnerabilities found!" ;;
#   1) echo "Only whitelisted vulnerabilities found." ;;
#   0) echo "No vulnerabilities detected (store might be empty)." ;;
#   *) echo "Unexpected vulnix exit code: $VULNIX_EXIT" ;;
# esac

# Skopeo image upload utility
function upload_image() {
  local tag=$1
  local archive_path=$2
  local remote_ref=$3

  echo "Uploading $tag image to $remote_ref"
  skopeo copy "$archive_path" "$remote_ref"
}

# Log in and push to container registries if running in CI on the main branch

# don't upload images unless we're deploying them
if [ "$CI_COMMIT_REF_NAME" = "main" ]; then
    #
    echo "Logging into Artifact registries"
    skopeo login --username "$CI_REGISTRY_USER" --password "$CI_REGISTRY_PASSWORD" "$CI_REGISTRY"
    skopeo login --username "$ACR_USERNAME" --password "$ACR_TOKEN" "$AZURE_REGISTRY"

    oras login --username "$CI_REGISTRY_USER" --password "$CI_REGISTRY_PASSWORD" "$CI_REGISTRY"
    oras login --username "$ACR_USERNAME" --password "$ACR_TOKEN" "$AZURE_REGISTRY"
    upload_image cassini "docker-archive:$(readlink -f cassini)" "docker://$CI_REGISTRY_IMAGE/cassini:$CI_COMMIT_SHORT_SHA"
    upload_image gitlab-observer "docker-archive:$(readlink -f gitlab-observer)" "docker://$CI_REGISTRY_IMAGE/polar-gitlab-observer:$CI_COMMIT_SHORT_SHA"
    upload_image gitlab-consumer "docker-archive:$(readlink -f gitlab-consumer)" "docker://$CI_REGISTRY_IMAGE/polar-gitlab-consumer:$CI_COMMIT_SHORT_SHA"
    upload_image kube-observer "docker-archive:$(readlink -f kube-observer)" "docker://$CI_REGISTRY_IMAGE/polar-kube-observer:$CI_COMMIT_SHORT_SHA"
    upload_image kube-consumer "docker-archive:$(readlink -f kube-consumer)" "docker://$CI_REGISTRY_IMAGE/polar-kube-consumer:$CI_COMMIT_SHORT_SHA"
    upload_image linker "docker-archive:$(readlink -f linker)" "docker://$CI_REGISTRY_IMAGE/polar-linker-agent:$CI_COMMIT_SHORT_SHA"
    upload_image resolver "docker-archive:$(readlink -f resolver)" "docker://$CI_REGISTRY_IMAGE/polar-resolver-agent:$CI_COMMIT_SHORT_SHA"

    skopeo copy docker-archive://$(readlink -f cassini) docker://$AZURE_REGISTRY/cassini:$CI_COMMIT_SHORT_SHA
    skopeo copy docker-archive://$(readlink -f gitlab-observer) docker://$AZURE_REGISTRY/polar-gitlab-observer:$CI_COMMIT_SHORT_SHA
    skopeo copy docker-archive://$(readlink -f gitlab-consumer) docker://$AZURE_REGISTRY/polar-gitlab-consumer:$CI_COMMIT_SHORT_SHA
    skopeo copy docker-archive://$(readlink -f kube-observer) docker://$AZURE_REGISTRY/polar-kube-observer:$CI_COMMIT_SHORT_SHA
    skopeo copy docker-archive://$(readlink -f kube-consumer) docker://$AZURE_REGISTRY/polar-kube-consumer:$CI_COMMIT_SHORT_SHA
    skopeo copy docker-archive://$(readlink -f linker) docker://$AZURE_REGISTRY/polar-linker-agent:$CI_COMMIT_SHORT_SHA
    skopeo copy docker-archive://$(readlink -f resolver) docker://$AZURE_REGISTRY/polar-resolver-agent:$CI_COMMIT_SHORT_SHA

    echo "Generating deployment manifests for revision $CI_COMMIT_SHORT_SHA"
    # Generate kubernetes manifests and push them to a hosted repository
    chmod +x ./scripts/render-manifests.sh

    # Explicitly enable strict secrets mode - this will encrypt any secret manifests
    # --CAUTION --
    # DO NOT REMOVE THIS FLAG, IT MUST BE SET TO RUN THE `render-manifests` scripts
    #
    SECRETS_MODE=strict
    sh scripts/render-manifests.sh src/deploy/sandbox manifests

    echo "uploading deployment manifests"
    # use oras to turn them into an oci artifact and upload
    oras push \
    $CI_REGISTRY/polar-manifests:$CI_COMMIT_SHORT_SHA \
     ./manifests/:application/vnd.kubernetes.manifests.layer.v1+tar


    echo "uploading deployment artifact to $AZURE_REGISTRY"
    oras push \
    $AZURE_REGISTRY/polar-manifests:sandbox \
     ./manifests/:application/vnd.kubernetes.manifests.layer.v1+tar
fi
