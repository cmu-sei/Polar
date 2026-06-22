# Local Cluster Deployment (nix-usernetes + Dhall)

This directory contains the Dhall sources used to generate Kubernetes manifests
for the local nix-usernetes development cluster. Configuration is expressed in
Dhall's type system — no template magic, no silent defaults, everything
inspectable.

## Prerequisites

- [Nix](https://nixos.org) with flakes enabled
- [podman](https://podman.io)
- [just](https://just.systems)
- The [nix-usernetes](https://github.com/cmu-sei/nix-usernetes) repo checked out
  alongside this one at `~/Documents/projects/nix-usernetes`
- The [nix-neo4j](https://github.com/cmu-sei/nix-neo4j) repo checked out at
  `~/Documents/projects/nix-neo4j`

## New Machine Setup

### 1. Generate Neo4j TLS certificates

The neo4j deployment requires TLS certificates baked into the environment at
render time. Generate them once and add to `my.env`:

```sh
# Generate CA key and cert
openssl genrsa -out neo4j-ca.key 4096
openssl req -x509 -new -nodes -key neo4j-ca.key -sha256 -days 3650 \
  -subj "/CN=Polar-Neo4j-CA/O=Polar" -out neo4j-ca.crt

# Generate server key and cert signed by CA
openssl genrsa -out neo4j-server.key 4096
openssl req -new -key neo4j-server.key \
  -subj "/CN=polar-db-svc.polar-graph.svc.cluster.local/O=Polar" \
  -out neo4j-server.csr
openssl x509 -req -in neo4j-server.csr -CA neo4j-ca.crt -CAkey neo4j-ca.key \
  -CAcreateserial -out neo4j-server.crt -days 3650 -sha256

# Export to environment (add to my.env)
export NEO4J_TLS_CA_CERT_CONTENT=$(cat neo4j-ca.crt)
export NEO4J_TLS_SERVER_CERT_CONTENT=$(cat neo4j-server.crt)
export NEO4J_TLS_SERVER_KEY_CONTENT=$(cat neo4j-server.key)
```

### 2. Set up environment

```sh
cp example.env my.env
# Fill in all <required> values in my.env
direnv allow
```

### 3. Create runtime config files

**`src/deploy/local/conf/git.json`** — Git agent credentials:

```sh
cp src/deploy/local/conf/git.json.example src/deploy/local/conf/git.json
# Fill in your GitLab instance details
```

**`src/deploy/local/conf/cyclops.yaml`** — Build orchestrator config.
See the existing file for the required fields. Stub values are fine for
local development.

### 4. Build the node image

```sh
cd ~/Documents/projects/nix-neo4j
nix build .#neo4j-image -o result-neo4j-image

cd ~/Documents/projects/nix-usernetes
nix build .#node-image -o result-node-image
just load-node-image
```

## Cluster Startup Sequence

```sh
cd ~/Documents/projects/nix-usernetes
just reset && just up && just init && just kubeconfig
export KUBECONFIG=$(pwd)/kubeconfig

cd ~/Documents/projects/Polar
just cluster-install-cert-manager
just cluster-render && just cluster-apply-storage && just cluster-load-all && just cluster-apply
```

## Alternative: Minikube

If using Minikube instead of nix-usernetes:

```sh
minikube start --cpus=4 --memory=8g --driver=podman

# Point Minikube at the host podman installation for local images
eval $(minikube podman-env)

# Install cert-manager
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.19.2/cert-manager.yaml

# Render and apply
just cluster-render
kubectl apply -f manifests/storage.yaml
kubectl apply -f manifests/
```

Note: `just cluster-load-all` and `just cluster-install-cert-manager` are
specific to nix-usernetes. With Minikube, load images via `eval $(minikube
podman-env)` before building, and install cert-manager via `kubectl apply`
directly.

## Alternative: kind

If using kind:

```sh
kind create cluster --name polar

# Load local images into kind
kind load docker-image <image>:<tag> --name polar

# Install cert-manager
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.19.2/cert-manager.yaml

# Render and apply
just cluster-render
kubectl apply -f manifests/
```

The `just kind-*` recipes in the Justfile handle image loading and cluster
management for kind. See `just --list` for available recipes.

## Required Environment Variables

| Variable | Description |
|---|---|
| `CI_COMMIT_SHORT_SHA` | Image tag (auto-set from git) |
| `NAMESPACE` | Primary Polar namespace |
| `GRAPH_NAMESPACE` | Neo4j namespace |
| `GRAPH_PASSWORD` | Neo4j password |
| `GITLAB_TOKEN` | GitLab personal access token |
| `GITLAB_USER` | GitLab username |
| `DOCKER_AUTH_JSON` | Base64-encoded docker config.json |
| `DOCKER_CONFIG_STR` | Docker config string for OCI resolver |
| `OCI_REGISTRY_AUTH` | Base64-encoded OCI registry auth |
| `NEO4J_TLS_SERVER_CERT_CONTENT` | PEM content of Neo4j server certificate |
| `NEO4J_TLS_SERVER_KEY_CONTENT` | PEM content of Neo4j server key |
| `NEO4J_TLS_CA_CERT_CONTENT` | PEM content of Neo4j CA certificate |

## Rendering Manifests

```sh
just cluster-render
```

This validates prerequisites, selects `values.local.dhall`, and renders all
Dhall sources to `manifests/`.

To render and type-check a single file during development:

```sh
dhall-to-yaml --documents --file src/deploy/local/polar/neo4j.dhall
```

To type-check only:

```sh
dhall type --file src/deploy/local/polar/neo4j.dhall
```

### Troubleshooting render errors

**Missing environment variable** — A Dhall file is reading an env var that
isn't set. Check the table above and verify `my.env` is loaded (`direnv allow`).

**Missing file** — `git.json` or `cyclops.yaml` doesn't exist. See above.

**Unbound variable** — A Dhall identifier is referenced but not defined.
Usually a commented-out `let` binding still referenced elsewhere.

**`[] : List kubernetes.Resource.Type`** — Wrong type annotation. Use
`[] : List kubernetes.Resource`.

**Invalid input / unexpected token** — Syntax error. Common causes: lowercase
`true`/`false` (Dhall requires `True`/`False`), missing comma in record
literal, malformed list concatenation with `#`.

## Applying Manifests

```sh
just cluster-apply
```

Or manually:

```sh
kubectl apply -f ./manifests
```

## Important Notes

- The proxy CA cert (`proxyCACert`) is optional — set to `None Text` in
  `values.dhall` for local development without a corporate proxy.
- GitLab observer will log backoff errors if no GitLab instance is configured.
  This is expected in a dev cluster.
- The `neo4j-bolt-ca` secret is generated from `NEO4J_TLS_CA_CERT_CONTENT`
  at render time and applied automatically by `cluster-apply`.
