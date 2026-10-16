<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

# Local Cluster Deployment (Dhall)

This directory contains the Dhall sources used to generate Kubernetes manifests
for a local development cluster. Configuration is expressed in
Dhall's type system — no template magic, no silent defaults, everything
inspectable.

## Prerequisites

- [Nix](https://nixos.org) with flakes enabled
- [podman](https://podman.io)
- [just](https://just.systems)
- A Kubernetes cluster and `kubectl` access to it (see the repository `README.md`, "Bring a cluster up")
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

### 4. Build the Neo4j image

```sh
cd ~/Documents/projects/nix-neo4j
nix build .#neo4j-image -o result-neo4j-image
```

## Cluster Startup Sequence

See "Bring a cluster up: the whole sequence" in the repository `README.md`. It
is the current path, and it works with any Kubernetes distribution.

## Other local clusters (Minikube, kind, k3d, Docker Desktop, ...)

Any of them works, because Polar asks only the things listed in step 1 of
"Bring a cluster up" in the repository `README.md`. What differs between them
is how images get to the nodes, which is the one thing to settle before running
`nu infra/up.nu`:

1. Create the cluster and point `kubectl` at it.
2. Give it a registry the nodes can pull from, and tell Polar two addresses:
   the one this machine pushes to (`POLAR_REGISTRY`) and the one the nodes pull
   from (`registry` in `infra/config/cluster.nuon`). Each of these tools has a
   way to run a local registry that its nodes trust (look for the "registry"
   add-on or the "local registry" guide in the tool's own documentation); a
   registry on the host is usually reached from a node under a different name
   than `localhost`.
3. Set `POLAR_CLUSTER_RUNTIME=none`, so the image step does not try to evict a
   node's image cache the way it does on the maintainers' dev node.
4. Make sure the nodes meet the rest of that list (a storage class, the
   `pkcs8_key_parser` kernel module, pod DNS), then run
   `nu infra/up.nu local-minimal --user <name> --email <email>`.

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
