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

# OCI Resolver

## Overview

The OCI resolver is an event-driven agent in the Polar observability framework responsible for validating OCI artifact references discovered by other agents. When the Kubernetes observer, GitLab processor, or any other agent encounters a container image reference, it emits a discovery event. The resolver receives that event, pulls the image manifest from the appropriate registry, and emits a resolved event carrying the verified digest and manifest data for downstream processing by the build processor.

The resolver enforces a deliberate trust boundary: observer agents report what they see, which may be unverified references of unknown origin. The resolver's job is to follow those references to their source and retrieve the manifest — an irreversible, mechanically verifiable action — so that the build processor's provenance linker can record a ground-truth artifact node in the knowledge graph rather than an unverified claim.

## Architecture

```text
[Observer / Processor]
  emits OCIArtifactDiscovered / ImageRefDiscovered
          │
          ▼
    [OCI Resolver]
  pulls manifest from registry
  verifies digest
  emits OCIArtifactResolved / ImageRefResolved
          │
          ▼
  [Build Processor → Provenance Linker]
  upserts OCIRegistry, OCIArtifact, and
  ContainerImageRef nodes and edges
  in the knowledge graph
```

The resolver has no startup scrape and maintains no internal list of registries to watch. It is purely reactive — it does nothing until a discovery event arrives, at which point it resolves exactly that reference and forwards the result.

## Authentication

Registry credentials are provided via the standard `DOCKER_CONFIG` environment variable, which points to a `config.json` file in the Docker credential format. The resolver delegates all credential lookup to the `docker-credential` crate, which reads this file and handles per-registry credential resolution transparently. In Kubernetes deployments, mount a Secret containing a valid `config.json` at the path `DOCKER_CONFIG` references.

No resolver-specific credential configuration exists. Credentials are an operational concern handled at deploy time via the rendered manifest pattern.

For a private registry with an internal CA, or a proxy that inspects TLS, the target sets `cluster.egress` (docs/architecture/egress.md): `POLAR_EGRESS_CA_CERT` names the PEM CA certificates the OCI client trusts beside the system's, every one of them, and `POLAR_EGRESS_PROXY` and `POLAR_EGRESS_NO_PROXY` the proxy it reaches a registry through and the hosts it reaches directly.

**Identity tokens** returned by credential helpers are not supported and are skipped in favor of anonymous access. See the Known Limitations section below.

## Known Limitations

**Images in a node's own store** — The resolver reads a reference by Docker's rule (`oci_client::Reference`, a port of Docker's `normalize.go`): a name with no registry host belongs to Docker Hub, so `postgres:17.5` is `docker.io/library/postgres:17.5`, and a name whose registry is `localhost` (`localhost/blitz-portal:latest`) is in a node's own image store, as podman names what it builds and as Polar and Blitz name the images they build and load by hand. `localhost:5000/…` is a registry like any other and is resolved.

A node-local image fails closed. The resolver cannot see a node's image store (it may run on another node), so it never fetches such an image and never vouches for one: it logs `not verified: … is a node-local image` and emits no event, so the container gets no `USES_IMAGE` edge. Calling an image local can only take a verification away, never give one. The gap is a query, not a node:

```cypher
MATCH (c:PodContainer)
WHERE c.image STARTS WITH 'localhost/' AND NOT (c)-[:USES_IMAGE]->(:OCIArtifact)
RETURN c
```

In production, every image should come from a registry.

**Identity tokens** — Some credential helpers return an OAuth2 identity token rather than a username/password pair. Identity tokens are not directly usable for OCI registry API calls — they are inputs to a registry-specific token exchange flow that `oci-client` does not implement. Affected registries will fall back to anonymous access. Use username/password or token-based credentials in `DOCKER_CONFIG` for private registry authentication.

## Environment Variables

| Variable | Purpose |
|---|---|
| `POLAR_EGRESS_CA_CERT` | PEM CA certificates trusted beside the system's (the egress section, `polar::egress`) |
| `POLAR_EGRESS_PROXY`, `POLAR_EGRESS_NO_PROXY` | The proxy a registry is reached through, and the hosts reached directly |
| `DOCKER_CONFIG` | Path to Docker credential config directory (standard OCI client convention); `$HOME/.docker` when unset, and anonymous access when that is not there |
| `OCI_INSECURE_REGISTRIES` | Registries reached over plain HTTP, comma-separated (default `registry.local-registry.svc.cluster.local:5000`) |

Standard Cassini broker connection variables apply as with all Polar agents.

## Container Image

```bash
nix build .#polarPkgs.resolver.image -o resolver-image
docker load < resolver-image
docker run --rm \
  -e DOCKER_CONFIG=/config \
  -v /path/to/docker-config:/config \
  localhost/oci-resolver:latest
```
