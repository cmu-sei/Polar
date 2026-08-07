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

For private registries using a self-signed or internal CA, set `PROXY_CA_CERT` to the path of the PEM-encoded CA certificate. The resolver will configure the OCI client to trust it for all outbound connections.

**Identity tokens** returned by credential helpers are not supported and are skipped in favor of anonymous access. See the Known Limitations section below.

## Known Limitations

**Unqualified image references** — When a pod spec contains an image reference without a registry prefix (e.g. `myimage:latest`), the Kubernetes API reports exactly that string. The resolver cannot determine the registry of origin from an unqualified reference and will skip resolution rather than guess. This is recorded in the graph as a provenance gap: the container was running an image whose origin cannot be verified. This most commonly occurs with locally loaded images in development environments. In production, all image references should be fully qualified.

**Identity tokens** — Some credential helpers return an OAuth2 identity token rather than a username/password pair. Identity tokens are not directly usable for OCI registry API calls — they are inputs to a registry-specific token exchange flow that `oci-client` does not implement. Affected registries will fall back to anonymous access. Use username/password or token-based credentials in `DOCKER_CONFIG` for private registry authentication.

## Environment Variables

| Variable | Purpose |
|---|---|
| `PROXY_CA_CERT` | Path to PEM-encoded CA certificate for private registries |
| `DOCKER_CONFIG` | Path to Docker credential config directory (standard OCI client convention) |

Standard Cassini broker connection variables apply as with all Polar agents.

## Container Image

```bash
nix build .#polarPkgs.resolver.image -o resolver-image
docker load < resolver-image
docker run --rm \
  -e DOCKER_CONFIG=/config \
  -v /path/to/docker-config:/config \
  polar-oci-resolver:latest
```
