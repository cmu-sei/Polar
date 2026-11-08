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

# Polar Deployments

This directory contains Dhall configurations for generating Kubernetes manifests for Polar services, along with a typed Dhall library that defines the public surface of Polar's agent and infrastructure types.

## Structure

```
types/          # Public Dhall library — types, defaults, and constructor functions
local/          # Local development deployment expressions
local/polar/    # Per-component manifests (agents, cassini, cert-issuer, neo4j, ...)
local/flux/     # Flux GitOps resources for local cluster
local/conf/     # Static config files embedded into ConfigMaps at render time
```

## The Types Library

`types/package.dhall` is the single import point for the Polar Dhall library. It exports:

- **`agents`** — type definitions for every Polar agent (`PolarAgent`, `GraphProcessor`, `CassiniTlsConfig`, `CertClientConfig`, and all named agent types). `CertClientConfig` ships with a `default` record covering the standard cert-issuer setup — override individual fields with record completion (`//`) where needed.
- **`cassini`** — the `Cassini` schema with a `defaults` record for standard port and TLS path configuration. Use record completion (`Cassini::{ ... }`) and supply at minimum `image` and `certClient`.
- **`functions`** — constructor functions for Kubernetes resources: `makeDeployment`, `makeNuInitContainer`, `makeGraphEnv`, `makeCertVolumes`, `makeOpaqueSecret`, and others.

`types/lib-constants.dhall` contains values that are fixed by Polar's architecture and safe to close over — cert paths, Cassini's service name and address, the cert-issuer URL, the polar namespace, and so on. It contains no environment variable reads and no file embeds. Consumer deployment expressions import this directly alongside `package.dhall`.

### Remote import

Once the library is published, import it by pinning a specific commit or tag:

```dhall
let Polar =
      https://raw.githubusercontent.com/your-org/polar/COMMIT_SHA/deploy/types/package.dhall
        sha256:HASH

let C =
      https://raw.githubusercontent.com/your-org/polar/COMMIT_SHA/deploy/types/lib-constants.dhall
        sha256:HASH
```

Replace `COMMIT_SHA` and `sha256:HASH` with the values produced by `dhall freeze --all types/package.dhall` after each release. Never pin to `master` — the hash provides the safety guarantee; a moving ref defeats it.

### Keeping the frozen hashes current

`dhall freeze` embeds a `sha256` hash for every import in the file it
processes. That hash is both a cache key and a security guarantee — Dhall will
refuse to evaluate an import whose content no longer matches the recorded hash.
This means **any change to a library file requires re-freezing `package.dhall`
before the change takes effect**, even locally.

The failure mode is silent and confusing: your edited file is on disk, the
change looks correct, but rendered output is stale because Dhall is serving the
previous expression from its on-disk cache at
`~/.cache/dhall/` (or `$XDG_CACHE_HOME/dhall/`). Re-freezing updates the hash,
which busts the cache entry and forces re-evaluation.

After any change to `agents.dhall`, `cassini.dhall`, `functions.dhall`, or
`lib-constants.dhall`:

    dhall freeze --all types/package.dhall

Then re-render all manifests. If you want to verify you are not hitting a stale
cache entry without re-freezing, clear the cache explicitly:

    dhall cache clear

and re-run `dhall-to-yaml`. If the output changes, you had a stale cache and
need to refreeze. If it doesn't, the hash is still valid.

Never edit a frozen file and skip re-freezing on the assumption that the change
is "small" — the hash covers the entire normal form of the expression, not just
the source text, and Dhall will silently ignore your change until the hash is
updated.

### Consuming the library

A minimal agent deployment expression looks like this:

```dhall
let Polar     = ../../types/package.dhall
let C         = ../../types/lib-constants.dhall
let Agent     = Polar.agents
let functions = Polar.functions

let myAgent : Agent.PolarAgent =
      { name       = "my-agent"
      , image      = "my-agent:abc1234"
      , certClient = Agent.CertClientConfig.default
      , tls =
        { broker_endpoint         = C.cassiniAddr
        , server_name             = C.cassiniServerName
        , client_certificate_path = C.certPaths.cert
        , client_key_path         = C.certPaths.key
        , client_ca_cert_path     = C.certPaths.ca
        }
      }
```

Cassini uses record completion against its defaults:

```dhall
let Cassini = Polar.cassini

let cassini : Cassini.Type =
      Cassini.defaults // { image = "cassini:abc1234", certClient = Agent.CertClientConfig.default }
```

## Agents built on polar-agent-builder

`types/builder.dhall` (exported as `Polar.builder`) renders an agent that follows the agent contract
as a `PolarAgent` resource, for the polar-operator to keep alive. The consumer states what is theirs
(`BuilderAgent`: name, namespace, image, the capabilities the deployment provides, a shutdown budget,
its settings, a port, a volume) and the cluster's facts once (`Platform`: where the broker and the
certificate issuer are, the `polar-init` image). The library supplies the rest: the `polar-init` init
container that obtains the pod's client certificate, the TLS and broker variables, the
`POLAR_HEALTH_*` variables and the `polar-healthcheck` liveness probe.

```dhall
let B = Polar.builder
in  [ B.makePolarAgent platform B.BuilderAgent::{ name = "my-agent", namespace = "mine", image = "..." } ]
```

A `PolarAgent` is a custom resource that `kubernetes.Resource` does not list, so agents render in a
file of their own (`dhall-to-yaml --documents`), and the `makeServiceAccount`, `makeService` and
`makePvc` objects that go with them in another. `examples/` has both, with the rendered output.

Unlike the older functions, these take the namespace as a parameter; nothing here closes over
`polar`. Order of deployment, differences between environments and secrets are not the library's:
a secret is only ever referenced, by name, through an `EnvVarSource` in `settings`.

`types/operator.dhall` (exported as `Polar.operator`) renders the polar-operator itself: its
ServiceAccount, a ClusterRole holding exactly what it calls on the API, and a one-replica
`Recreate` Deployment. Its CustomResourceDefinition is generated from the operator's own types
(`polar-operator crd`) and applied first. See `examples/operator.dhall`.

## Local Development

### Prerequisites

- `dhall-to-yaml` — converts Dhall expressions to Kubernetes YAML
- A local Kubernetes cluster — Minikube, Orbstack, kind or similar
- Container images built for the components you want to deploy. See [`../agents/README.md`](../agents/README.md) for build instructions. Neo4j uses a stock upstream image; everything else is built from source.
- Depending on what you're evaluating, you may also need a local GitLab instance, OCI registry, or Git remote.

### Rendering manifests

Each component under `local/polar/` is a Dhall file that evaluates to a `List kubernetes.Resource`. Render to YAML with:

```sh
dhall-to-yaml --file local/polar/agents.dhall   > local/polar/agents.yaml
dhall-to-yaml --file local/polar/cassini.dhall  > local/polar/cassini.yaml
dhall-to-yaml --file local/polar/cert-issuer.dhall > local/polar/cert-issuer.yaml
dhall-to-yaml --file local/polar/neo4j.dhall    > local/polar/neo4j.yaml
```

Or regenerate all manifests at once if you have a script for it.

### Deployment order

Polar's mTLS bootstrap has a strict dependency chain. Apply in this order and wait for each rollout before proceeding:

```sh
kubectl apply -f local/polar/cert-issuer.yaml
kubectl rollout status deploy/cert-issuer -n polar

kubectl apply -f local/polar/cassini.yaml
kubectl rollout status deploy/cassini -n polar

kubectl apply -f local/polar/agents.yaml
```

Neo4j and Jaeger are independent of the mTLS chain and can be applied in any order relative to each other, but agents that write to the graph depend on Neo4j being healthy.

### Environment variables required at render time

These must be set in your shell when running `dhall-to-yaml`:

| Variable | Description |
|---|---|
| `CI_COMMIT_SHORT_SHA` | Image tag for all Polar service images. Defaults to `latest` if unset. |
| `GITLAB_TOKEN` | GitLab API token. Required if deploying the GitLab observer or consumer. |
| `GRAPH_PASSWORD` | Neo4j password. Written into the `polar-graph-pw` Kubernetes secret. |
| `NEO4J_AUTH` | Neo4j auth string in `username/password` format. Written into `neo4j-secret`. |
| `NEO4J_TLS_CA_CERT_CONTENT` | PEM content of the Neo4j TLS CA. Written into `neo4j-bolt-ca` secret. |
| `DOCKER_AUTH_JSON` | Base64-encoded `config.json` with OCI registry credentials. Used by the provenance resolver. |

### What is and is not configurable

The `polar` namespace is fixed — it is not a parameter and the library provides no mechanism to change it. Polar is a single-instance system; deploying two instances in the same cluster is not a supported configuration.

Image references, TLS paths, broker addresses, and graph credentials are all caller-supplied. The library provides defaults via `CertClientConfig.default` and `Cassini.defaults` that reflect the standard deployment layout, but every field can be overridden via Dhall record completion.

`imagePullPolicy` is not set by any library function. Local deployment expressions append it via record override (`// { imagePullPolicy = Some "Never" }`) on init containers where needed. Set it appropriately for your environment.

## GitOps and Immutability

Manifests are generated from Dhall before deployment and committed to version control. This ensures:

- No deployment drift from ad-hoc `helm install` or `kubectl edit` changes
- Rollbacks to any previous known-good manifest are a `git revert` away
- Full auditability — the exact YAML applied to the cluster is in the repo

Manifests are packaged as OCI artifacts using `oras` so that container images and deployment manifests share the same versioning and can be promoted through environments together.

## CI/CD Environment Variables

The following are required in the CI environment for the full Flux-based deployment pipeline:

| Variable | Description |
|---|---|
| `AZURE_CLIENT_ID` | Client ID of the Azure service principal for key vault access |
| `AZURE_TENANT_ID` | Azure tenant ID |
| `AZURE_CLIENT_SECRET` | Secret for Azure service principal authentication |
| `AZURE_ENVIRONMENT` | Set to `AzureUsGovernment` |
| `AZURE_AUTHORITY_HOST` | Azure Government login endpoint |
| `ACR_USERNAME` | Username for the Azure Container Registry token |
| `ACR_TOKEN` | Token for ACR authentication |
| `GITLAB_USER` | GitLab username for Flux authentication |
| `GITLAB_TOKEN` | GitLab token for Flux authentication |
| `CI_COMMIT_SHORT_SHA` | Populated automatically by GitLab CI |
| `NEO4J_AUTH` | Neo4j credentials in `username/password` format |
| `DOCKER_AUTH_JSON` | Base64-encoded OCI registry `config.json` |

Each Polar agent may require additional environment variables of its own at runtime. See the individual agent README files under `../agents/` for details.
