# Local (Minikube) Deployment Configurations (Dhall)

This directory contains the Dhall sources used to generate Kubernetes manifests for a local Minikube environment. The goal is reproducible configuration expressed in Dhall's type system, with zero template magic and no silent defaults. Everything here is meant to be deliberate and inspectable.

The directory is structured around a few expectations:

* Each Kubernetes object is defined as a Dhall expression returning a single, strongly-typed resource.
* Nothing in this directory performs external I/O. Dhall does not generate base64, JSON, or derived artifacts. CI does that.
* Every rendered manifest is meant to be fed directly into `kubectl` without preprocessing.

## Minikube Setup

You need Minikube installed and a functioning local Docker environment. Start Minikube with enough resources to run whatever you're deploying:

```sh
minikube start \
  --cpus=4 \
  --memory=8g \
  --driver=podman # or docker if you prefer

# If you rely on container images built locally, point Minikube at the host podman installation:
# Note that if you want to pull from any remote repos, you'll have to add Image pull secrets.

eval $(minikube podman-env)

# use kubectl to bootstrap the cluster
# we use cert-manager to handle certificates so it should be installed first.
# SEE: https://cert-manager.io/docs/installation/
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.19.2/cert-manager.yaml
```

This prevents you from having to push images to a registry just to test them locally.

## Environment Setup

The Dhall files in this directory read secrets and deployment parameters from environment variables at render time. These must be set before rendering manifests.

### First-time setup

1. Copy the example env file from the repo root:

   ```sh
   cp example.env my.env
   ```

2. Fill in all values marked `<required>` in `my.env`. See the comments in that file for what each variable does and where to get the values.

3. `my.env` is automatically sourced by direnv when you enter the project directory, overriding any defaults set by the Nix devShell. If direnv is not prompting you, run:

   ```sh
   direnv allow
   ```

4. Verify the key deployment variables are set:

   ```sh
   echo $NAMESPACE
   echo $GRAPH_NAMESPACE
   echo $GITLAB_TOKEN
   echo $DOCKER_AUTH_JSON
   ```

### Required variables for manifest rendering

The following variables must be set to render manifests. All others in `my.env` are for running agents locally and are not needed just for rendering.

| Variable | Description |
|---|---|
| `CI_COMMIT_SHORT_SHA` | Image tag (auto-set from `git rev-parse --short HEAD`) |
| `NAMESPACE` | Primary Polar namespace |
| `GRAPH_NAMESPACE` | Neo4j namespace |
| `GRAPH_PASSWORD` | Neo4j password |
| `GITLAB_TOKEN` | GitLab personal access token |
| `GITLAB_USER` | GitLab username |
| `DOCKER_AUTH_JSON` | Base64-encoded docker config.json for image pull secrets |
| `DOCKER_CONFIG_STR` | Docker config string for OCI resolver |
| `OCI_REGISTRY_AUTH` | Base64-encoded OCI registry auth config |
| `NEO4J_AUTH` | Neo4j credentials in `user/password` format (derived from `GRAPH_USER`/`GRAPH_PASSWORD`) |
| `NEO4J_TLS_SERVER_CERT_CONTENT` | PEM content of Neo4j server certificate (required if TLS enabled) |
| `NEO4J_TLS_SERVER_KEY_CONTENT` | PEM content of Neo4j server key (required if TLS enabled) |
| `NEO4J_TLS_CA_CERT_CONTENT` | PEM content of Neo4j CA certificate (required if TLS enabled) |

### Runtime config files

Two config files must exist before rendering. They are not checked into the repo because they contain credentials or are environment-specific:

**`src/deploy/local/conf/git.json`** — Git agent credentials. Create with your GitLab instance details:

```sh
cat > src/deploy/local/conf/git.json << 'EOF'
{
  "hosts": {
    "gitlab.example.com": {
      "http": {
        "username": "your-username",
        "token": "your-token"
      }
    }
  }
}
EOF
```

**`src/deploy/local/conf/cyclops.yaml`** — Build orchestrator config. Compile from the Dhall schema:

```sh
# See src/agents/build-orchestrator/agent/conf/orchestrator.dhall for the full schema.
# At minimum:
cat > src/deploy/local/conf/cyclops.yaml << 'EOF'
backend:
  driver: kubernetes
  kubernetes:
    namespace: polar-builds
    job_labels: []
cassini:
  broker_url: "nats://cassini:4222"
  inbound_subject: "polar.builds.orchestrator.events"
bootstrap:
  builder_image: "your-registry/builder:latest"
credentials:
  git_secret_name: git-secret
  registry_secret_name: registry-secret
storage:
  endpoint_url: "http://minio:9000"
  access_key: your-access-key
  secret_key: your-secret-key
  region: us-east-1
  bucket: polar-builds
log:
  format: json
  level: info
EOF
```

## Render

Once your environment is set up, render all manifests with:

```sh
sh scripts/render-manifests.sh src/deploy/local ./manifests
```

This walks each subdirectory under `src/deploy/local`, finds all `.dhall` files one level deep, runs `dhall-to-yaml --documents` on each, and writes the resulting `.yaml` files into `./manifests/`.

To render and type-check a single file during development:

```sh
dhall-to-yaml --documents --file src/deploy/local/polar/neo4j.dhall
```

To type-check only (no output), useful for debugging:

```sh
dhall type --file src/deploy/local/polar/neo4j.dhall
```

### Troubleshooting render errors

**Missing environment variable** — A Dhall file is reading an env var that isn't set. Check the table above and verify your `my.env` is loaded (`direnv allow`).

**Missing file** — `git.json` or `cyclops.yaml` doesn't exist. See the Runtime config files section above.

**Unbound variable** — A Dhall identifier is referenced but not defined. Usually indicates a commented-out `let` binding that is still referenced elsewhere in the file.

**`[] : List kubernetes.Resource.Type`** — Wrong type annotation on an empty list. Use `[] : List kubernetes.Resource` (the union type itself, not `.Type`).

**Invalid input / unexpected token** — A syntax error in the Dhall file. Common causes: lowercase `true`/`false` (Dhall requires `True`/`False`), missing comma in record literal, or malformed list concatenation with `#`.

## Applying the Manifests

Once rendered, deploy them into Minikube:

```sh
kubectl apply -f ./manifests
```

You can reapply at will; Dhall ensures stable output unless you change inputs.

Check resource health:

```sh
kubectl get all -n <namespace>
kubectl describe <resource> -n <namespace>
kubectl logs <pod> -n <namespace>
```

If you are working with multiple namespaces, ensure your manifests explicitly set them. Nothing here assumes defaults.

## Secret Handling

Secrets in this directory are defined structurally in Dhall but must contain already-encoded values. Dhall does not compute base64 or generate Docker registry auth blobs. CI or a local script should prepare those values and export them as environment variables before rendering.

A typical pattern:

```sh
export DOCKER_AUTH_JSON=$(cat ~/.docker/config.json | base64 -w0)
sh scripts/render-manifests.sh src/deploy/local ./manifests
```

This keeps Dhall deterministic while still allowing your cluster to receive fully-formed secrets.

## Typical Usage Pattern (Local Development)

1. Start Minikube.
2. Build your local images (if applicable).
3. Ensure `my.env` is populated and sourced via direnv (`direnv allow`).
4. Ensure `conf/git.json` and `conf/cyclops.yaml` exist.
5. Run `sh scripts/render-manifests.sh src/deploy/local ./manifests`.
6. `kubectl apply -f ./manifests`.
7. Iterate.

This directory should be treated as an authoritative declarative spec for local cluster state. Anything non-declarative belongs in CI or tooling outside Dhall.

## Important Notes

* Config is setup to use a proxy; the file `proxy-ca-cert.dhall` needs updating with the base64 string for your certificate.
* The GitLab site certificate also needs updating for your host. `gitlab-crt.dhall` needs updating with the base64 string for your certificate.
* The token for access to your GitLab needs updating. The file `gitlab-secret.dhall` contains that secret.
* The `GITLAB_ENDPOINT` variable needs updating to point at your GitLab server. It is located in `values.dhall`.
