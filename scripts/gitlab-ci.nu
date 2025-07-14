#!/usr/bin/env nu

# Enable strict mode
use std assert

# Display job context if running in CI
if ($env.CI? | default false) {
  let job = ($env.CI_JOB_NAME? | default "unknown")
  let id = ($env.CI_JOB_ID? | default "unknown")
  let branch = ($env.CI_COMMIT_BRANCH? | default "unknown")
  print $"Running CI job: ($job):($id) on branch ($branch)"
}

# git won't let nix operate if it doesn't think its safe.
let cwd = (pwd)
^git config --global --add safe.directory $cwd

# Run static analysis tooling
^sh ./scripts/static-tools.sh

# Build core agent binaries
^nix build

# Upload binaries to GitLab Package Registry if in CI
if ($env.CI? | default false) {
  for file in (ls result/bin/* | where type == "file") {
    let filename = ($file.name | path basename)
    ^curl
      --header $"JOB-TOKEN: ($env.CI_JOB_TOKEN)"
      --upload-file $file.name
      $"($env.CI_API_V4_URL)/projects/($env.CI_PROJECT_ID)/packages/generic/polar/($env.CI_COMMIT_SHORT_SHA)/($filename)"
    print $"Uploaded binary: ($filename)"
  }
} else {
  print "Not in CI; skipping artifact upload."
}

# Build Docker images
let builds = [
  [alias, target];
  ["cassini", ".#polarPkgs.cassini.cassiniImage"]
  ["gitlab-observer", ".#polarPkgs.gitlabAgent.observerImage"]
  ["gitlab-consumer", ".#polarPkgs.gitlabAgent.consumerImage"]
  ["kube-observer", ".#polarPkgs.kubeAgent.observerImage"]
  ["kube-consumer", ".#polarPkgs.kubeAgent.consumerImage"]
]

for row in $builds {
  ^nix build $row.target -o $row.alias
}

# Run vulnerability scan
let vulnix_out = (
  try {
    ^vulnix --json result | save --force vulnix-report.json; $env.LAST_EXIT_CODE
  } catch {
    $env.LAST_EXIT_CODE
  }
)

match $vulnix_out {
  2 => print "Non-whitelisted vulnerabilities found!"
  1 => print "Only whitelisted vulnerabilities found."
  0 => print "No vulnerabilities detected (store might be empty)."
  _ => print $"Unexpected vulnix exit code: ($vulnix_out)"
}

# Define image upload
def upload-image [tag: string, archive_path: string, remote_ref: string] {
  print $"Uploading ($tag) image to ($remote_ref)"
  ^skopeo copy $archive_path $remote_ref
}

# Push to GitLab registry if in CI and on main
if ($env.CI? | default false) and ($env.CI_COMMIT_REF_NAME == "main") {
  ^skopeo login
    --username $env.CI_REGISTRY_USER
    --password $env.CI_REGISTRY_PASSWORD
    $env.CI_REGISTRY

  upload-image "cassini" $"docker-archive:($'cassini' | path expand)" $"docker://($env.CI_REGISTRY_IMAGE)/cassini:($env.CI_COMMIT_SHORT_SHA)"
  upload-image "gitlab-observer" $"docker-archive:($'gitlab-observer' | path expand)" $"docker://($env.CI_REGISTRY_IMAGE)/polar-gitlab-observer:($env.CI_COMMIT_SHORT_SHA)"
  upload-image "gitlab-consumer" $"docker-archive:($'gitlab-consumer' | path expand)" $"docker://($env.CI_REGISTRY_IMAGE)/polar-gitlab-consumer:($env.CI_COMMIT_SHORT_SHA)"
  upload-image "kube-observer" $"docker-archive:($'kube-observer' | path expand)" $"docker://($env.CI_REGISTRY_IMAGE)/polar-kube-observer:($env.CI_COMMIT_SHORT_SHA)"
  upload-image "kube-consumer" $"docker-archive:($'kube-consumer' | path expand)" $"docker://($env.CI_REGISTRY_IMAGE)/polar-kube-consumer:($env.CI_COMMIT_SHORT_SHA)"
}

# Upload to Azure if creds available
if ($env.ACR_USERNAME? and $env.ACR_TOKEN? and $env.AZURE_REGISTRY?) {
  print "Uploading to Azure Container Registry..."
  for image in ["cassini", "gitlab-observer", "gitlab-consumer", "kube-observer", "kube-consumer"] {
    upload-image $image $"docker-archive://($image | path expand)" $"docker://($env.AZURE_REGISTRY)/($image):($env.CI_COMMIT_SHORT_SHA)"
  }
} else {
  print "ACR credentials not configured; skipping Azure upload."
}

# Clone the chart repo
let repo_url = $"https://($env.POLAR_DEPLOY_USER):($env.CHART_REPO_TOKEN)@gitlab.sandbox.labz.s-box.org/sei/polar-deploy.git"
^git clone --depth 1 $repo_url

# Generate helm charts
chmod +x ./scripts/render-manifests.sh
rm -rf polar-deploy/manifests/
^sh scripts/render-manifests.sh src/deploy/polar polar-deploy/manifests

cd polar-deploy

if ($env.CI? | default false) == false {
  ^git config user.email $env.GITLAB_USER_EMAIL
  ^git config user.name $"ci-job-($env.CI_JOB_NAME)-($env.CI_JOB_ID)"
  ^git add .

  print "Updated files:"
  ^git diff --name-only --cached

  let timestamp = (date now | format date "%Y-%m-%dT%H:%M:%SZ")
  ^envsubst < ../scripts/metadata.yaml.tpl | save metadata.yaml
  ^git add metadata.yaml
  ^git commit -m $"Update manifests from source commit ($env.CI_COMMIT_SHA)"
  ^git push origin "sandbox"
} else {
  print "Skipping manifest upload"
}
