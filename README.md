# ACE — Agile Capacity Estimator
A team completion-projection dashboard for Agile/Scrum teams. ACE reads
work-item data from a Neo4j graph, runs Monte Carlo simulation against
each team's actual historical performance, and shows you — honestly,
with real confidence bands, not a single optimistic line — when a
Program Increment is actually going to finish.
## What ACE does
- **Projection Chart** — pick a team and PI, and see a probability
  distribution for the completion date: probability of finishing by the
  committed date, projected completion date, and projected percent
  complete at PI end.
- **Realtime Alerts** — WIP-delay and completion-probability warnings
  surfaced as soon as a team's actual pace diverges from what's needed
  to hit the committed date.
- **Team Impact Adjustment** — compare multiple teams side by side to
  see where cross-team dependencies are putting a shared PI date at
  risk.
The projection engine is PERT-based Monte Carlo: it samples from each
team's own historical velocity (not an assumed one), and — when
per-issue data is available — models per-issue completion individually
with team-size contention, rather than a single aggregate burn-down
line. See [src/ace/ARCHITECTURE.md](src/ace/ARCHITECTURE.md) for the
full technical detail.
## Using ACE
Once deployed (see Installation below), open the dashboard, select a
team and PI increment, and click **Get Graph**. Advanced options let
you adjust the projection start date and training window. See
[src/ace/README.md](src/ace/README.md) for local development and
direct container usage.
## Installation

ACE ships as a container image, built reproducibly with Nix and
deployed to Kubernetes.

### Prerequisites

- A Kubernetes cluster (any distribution — this pipeline doesn't
  assume a specific one; see **Bring your own cluster**, below)
- A Neo4j instance populated with work-item data (see **Data source**,
  below)
- [Nix](https://nixos.org) with flakes enabled, `podman`, `direnv`,
  and [`just`](https://github.com/casey/just) — or use the provided
  dev container (see below), which already has all of this

### Dev container

If you don't already have Nix/`just`/`skopeo` set up, `just
dev-container` builds a ready-to-use development environment with
everything the build pipeline needs:

```bash
just dev-container

podman run --rm -it --name polar-dev \
  --user 0 --userns=keep-id \
  -e CREATE_USER="$USER" \
  -e CREATE_UID="$(id -u)" \
  -e CREATE_GID="$(id -g)" \
  -v $PWD:/workspace \
  localhost/polar-dev:latest
```

Inside the container, at `/workspace`:

```bash
direnv allow
```

The first `direnv allow` builds the full workspace (every Rust agent,
plus ACE's Python environment) — this takes a while the first time.
Once built, keep the container running (`podman stop`/`podman start`
rather than `--rm`-ing it) so subsequent shells skip the rebuild
entirely.

### Build

```bash
just cluster-build-all      # build ACE + every supporting service
```

### Load images into a registry

Nix-built images aren't pushed anywhere automatically — the pipeline
pushes them to a plain OCI registry, which any Kubernetes distribution
can pull from the same way. For local development:

```bash
just registry-up            # starts a local registry container on :5000
just cluster-load-all       # pushes every built image via skopeo
```

To push to a different registry (a real cluster, or a customer
environment), edit the `_registry_host`/`_registry_port` variables
near the top of the `Justfile`, or run the underlying `skopeo copy`
commands directly — see `cluster-load-all`'s recipe body for the exact
image list and naming convention. See **Bring your own cluster**,
below, for pointing a target's `overrides.dhall` at the result.

### Customize a target

A "target" is a named deployment profile under `infra/targets/`.
`local-minimal` is a small, single-node profile for local development;
`infra/targets/<name>/target.nu` lists what each target deploys, and
`infra/targets/<name>/overrides.dhall` is where you customize
image references, hostnames, and other per-deployment values.

### Deploy

```bash
nu infra/render.nu local-minimal --apply
```

Swap `local-minimal` for whichever target you've set up. `--apply`
renders every chart to `manifests/` and applies them to whatever
cluster your current `$KUBECONFIG` points at, in dependency order.

ACE runs as a Kubernetes `Job` (not a `Deployment`): it requests a
long-lived mTLS client certificate from the cluster's certificate
infrastructure via an init container on every pod creation, which is
what lets it recover automatically if its certificate or environment
ever needs to be refreshed. It authenticates to Neo4j over mutual TLS
and holds no other cluster permissions.

### Bring your own cluster

This pipeline doesn't assume any particular Kubernetes distribution.
To deploy against your own cluster:

1. Point `$KUBECONFIG` at it, same as any other `kubectl` workflow.
2. Push built images to a registry your cluster's nodes can actually
   pull from (see **Load images into a registry**, above — edit the
   `Justfile`'s `_registry_host`/`_registry_port` variables to point
   at it) — a real cluster generally can't import a local image
   tarball directly the way some local dev tools (`kind load`, `k3d
   image import`, etc.) can.
3. Copy `infra/targets/local-minimal/` to a new target directory,
   update `overrides.dhall`'s image references to point at your
   registry, and adjust any hostnames/URLs specific to your
   environment.
4. `nu infra/render.nu <your-target> --apply`.

Some recipes in the `Justfile` (`cluster-up`, `cluster-status`, and a
few others) are conveniences for this repo's own local development
cluster and aren't required for deploying elsewhere.

### Just the container

To build and run ACE's container directly, without the full Kubernetes
deployment:

```bash
cd src/ace
nix build .#aceImage
podman load -i result-ace
podman run -p 8080:8080 \
  -e GRAPH_ENDPOINT=bolt://<host>:7687 \
  -e GRAPH_USER=neo4j \
  -e GRAPH_PASSWORD=<password> \
  ace-viz:latest
```

## Data source
ACE reads from Neo4j — it does not talk to Jira, GitLab, or any other
source system directly. The graph it reads from is populated by
**Polar**, the platform this repository is built on: a set of
Cassini-connected ingestion agents (Jira, GitLab, Kubernetes, and
others) that observe source systems and write structured work-item
data into Neo4j. Polar also provides the certificate infrastructure
(cert-issuer) ACE uses for mTLS, and the Nix/Dhall/Kubernetes build
and deployment pipeline this README's install steps use.
If you're working on ACE itself, you'll mostly live in src/ace/. If
you're working on the ingestion agents, certificate infrastructure, or
deployment pipeline, see src/agents/ and infra/.

## License
Licensed under a MIT-style license — see license.txt, or contact
permission@sei.cmu.edu for full terms.

