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
line. See [`src/ace/ARCHITECTURE.md`](src/ace/ARCHITECTURE.md) for the
full technical detail.

## Using ACE

Once deployed (see Installation below), open the dashboard, select a
team and PI increment, and click **Get Graph**. Advanced options let
you adjust the projection start date and training window. See
[`src/ace/README.md`](src/ace/README.md) for local development and
direct container usage.

## Installation

ACE ships as a container image, built reproducibly with Nix and
deployed to Kubernetes.

### Prerequisites

- A Kubernetes cluster
- A Neo4j instance populated with work-item data (see **Data source**,
  below)
- [Nix](https://nixos.org) with flakes enabled, `podman` or `docker`,
  and [`just`](https://github.com/casey/just)

### Build and deploy

```bash
just cluster-build-all      # build ACE + every supporting service
just cluster-load-all       # load built images into the cluster
nu infra/render.nu <target> --apply
```

`<target>` selects a deployment profile from `infra/targets/` (for
example, `local-minimal` for a local single-node cluster). See
`infra/targets/<name>/target.nu` for what each target includes.

ACE runs as a Kubernetes `Job` (not a `Deployment`): it requests a
long-lived mTLS client certificate from the cluster's certificate
infrastructure via an init container on every pod creation, which is
what lets it recover automatically if its certificate or environment
ever needs to be refreshed. It authenticates to Neo4j over mutual TLS
and holds no other cluster permissions.

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
(`cert-issuer`) ACE uses for mTLS, and the Nix/Dhall/Kubernetes build
and deployment pipeline this README's install steps use.

If you're working on ACE itself, you'll mostly live in `src/ace/`. If
you're working on the ingestion agents, certificate infrastructure, or
deployment pipeline, see `src/agents/` and `infra/`.

## License

Licensed under a MIT-style license — see `license.txt`, or contact
permission@sei.cmu.edu for full terms.
