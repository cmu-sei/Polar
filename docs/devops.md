
# DevOps.md

## Overview

This repository uses a reproducible CI/CD pipeline defined primarily through Nix, with a shell script (`gitlab-ci.sh`) to simulate GitLab CI locally. This approach enables engineers to test builds, static analysis, artifact creation, image generation, and deploy processes without needing to push code or wait for a remote CI job.

---

## Why?

GitLab's pipeline logic is nontrivial: environment variables, CI/CD-specific logic, and conditional artifact handling are difficult to replicate by hand. `gitlab-ci.sh` mirrors that logic for local testing and debugging, ensuring your changes pass before committing.

This script serves three purposes:

1. **Local Simulation of CI Jobs**: Replicates GitLab environment conditions.
2. **Deterministic, Declarative Builds**: Uses `nix build` for all key artifacts.
3. **Artifact + Image Promotion**: Supports conditional pushes to registry endpoints.

Eventually we'll switch to using a shell like nushell, but for now, bash will do.

---

## Prerequisites

### System Requirements

* **Nix** installed (single-user install is fine unless you control the runner)
* Flake support enabled:

  ```bash
  mkdir -p ~/.config/nix
  echo "experimental-features = nix-command flakes" >> ~/.config/nix/nix.conf
  ```

*  Tools used:
  * `skopeo`
  * `git`
  * `curl`
  * `envsubst` (usually via GNU `gettext`)
  * `dhall-to-yaml`

---

## Usage

Run the script locally from the root of the repository:

```bash
./gitlab-ci.sh
```

You can override environment variables as needed:

```bash
CI=true \
CI_JOB_NAME=local-test \
CI_COMMIT_BRANCH=my-branch \
CI_JOB_TOKEN=dummytoken \
CI_PROJECT_ID=123 \
CI_COMMIT_SHORT_SHA=deadbeef \
CI_COMMIT_SHA=deadbeefcafebabe \
GITLAB_USER_EMAIL=ci@yourcompany.org \
CHART_REPO_TOKEN=xxxxx \
POLAR_DEPLOY_USER=ci \
./gitlab-ci.sh
```

---

## Behavior

### Steps Performed

1. **Git safety workaround** – Marks the repo as safe if necessary.
2. **Static Analysis** – Invokes `./scripts/static-tools.sh`.
3. **Build Artifacts** – Uses `nix build` to compile binaries and images.
4. **Conditional Uploads**

   * GitLab Package Registry for binaries.
   * Skopeo-pushes to Docker registries (GitLab and Azure).
5. **Security Scanning** – `vulnix` analysis with exit-code interpretation.
6. **Chart Generation & Publishing** – Helm charts are rendered and optionally committed.

---

## Optional Config

### Gitlab Uploiads
TODO

### Azure Uploads

To enable image pushes to ACR:

```bash
export ACR_USERNAME=yourusername
export ACR_TOKEN=yourtoken
export AZURE_REGISTRY=yourregistry.azurecr.io
```

# Package Registry Uploads
TODO
