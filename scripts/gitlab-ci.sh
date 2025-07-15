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
nix build

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
echo Building agent images

nix build .#polarPkgs.cassini.cassiniImage -o cassini
nix build .#polarPkgs.gitlabAgent.observerImage -o gitlab-observer
nix build .#polarPkgs.gitlabAgent.consumerImage -o gitlab-consumer
nix build .#polarPkgs.kubeAgent.observerImage -o kube-observer
nix build .#polarPkgs.kubeAgent.consumerImage -o kube-consumer

# Run vulnerability scan
# vulnix returns nonzero exit codes, so we need to check for ourselves
# SEE: https://github.com/nix-community/vulnix/issues/79
set +e
vulnix --json result > vulnix-report.json
VULNIX_EXIT=$?
set -e

case $VULNIX_EXIT in
  2) echo "Non-whitelisted vulnerabilities found!" ;;
  1) echo "Only whitelisted vulnerabilities found." ;;
  0) echo "No vulnerabilities detected (store might be empty)." ;;
  *) echo "Unexpected vulnix exit code: $VULNIX_EXIT" ;;
esac

# Skopeo image upload utility
function upload_image() {
  local tag=$1
  local archive_path=$2
  local remote_ref=$3

  echo "Uploading $tag image to $remote_ref"
  skopeo copy "$archive_path" "$remote_ref"
}

# Log in and push to container registries if running in CI on the main branch

skopeo login --username "$CI_REGISTRY_USER" --password "$CI_REGISTRY_PASSWORD" "$CI_REGISTRY"
skopeo login --username "$ACR_USERNAME" --password "$ACR_TOKEN" "$AZURE_REGISTRY"

upload_image cassini "docker-archive:$(readlink -f cassini)" "docker://$CI_REGISTRY_IMAGE/cassini:$CI_COMMIT_SHORT_SHA"
upload_image gitlab-observer "docker-archive:$(readlink -f gitlab-observer)" "docker://$CI_REGISTRY_IMAGE/polar-gitlab-observer:$CI_COMMIT_SHORT_SHA"
upload_image gitlab-consumer "docker-archive:$(readlink -f gitlab-consumer)" "docker://$CI_REGISTRY_IMAGE/polar-gitlab-consumer:$CI_COMMIT_SHORT_SHA"
upload_image kube-observer "docker-archive:$(readlink -f kube-observer)" "docker://$CI_REGISTRY_IMAGE/polar-kube-observer:$CI_COMMIT_SHORT_SHA"
upload_image kube-consumer "docker-archive:$(readlink -f kube-consumer)" "docker://$CI_REGISTRY_IMAGE/polar-kube-consumer:$CI_COMMIT_SHORT_SHA"

# Upload to Azure Container Registry (if configured)
echo "Uploading to Azure Container Registry..."

for image in cassini gitlab-observer gitlab-consumer kube-observer kube-consumer; do
    upload_image "$image" \
    "docker-archive://$(readlink -f $image)" \
    "docker://$AZURE_REGISTRY/$image:$CI_COMMIT_SHORT_SHA"
done

#TODO: Uncomment when we test the build job
# Clone the chart repo
# NOTE: A deploy key should be used used to enable read/write perms
# An access token will do in the meantime
# git clone --depth 1 "https://$POLAR_DEPLOY_USER:$CHART_REPO_TOKEN@gitlab.sandbox.labz.s-box.org/sei/polar-deploy.git"
# # Generate helm charts and push them to a hosted repository
# chmod +x ./scripts/render-manifests.sh
# # delete old charts to make room for new configurations
# rm -rf polar-deploy/manifests/

# sh scripts/render-manifests.sh src/deploy/polar polar-deploy/manifests
# cd polar-deploy

# # Commit and push if there are changes

# git config user.email $GITLAB_USER_EMAIL
# git config user.name ci-job-$CI_JOB_NAME-$CI_JOB_ID
# git add .

# echo "Updated files:"
# git diff --name-only --cached

# echo "Writing metadata and pushing upstream..."
# export TIMESTAMP=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
# envsubst < ../scripts/metadata.yaml.tpl > metadata.yaml
# git add metadata.yaml
# git commit -m "Update manifests from source commit $CI_COMMIT_SHA"
# git push origin "sandbox"
