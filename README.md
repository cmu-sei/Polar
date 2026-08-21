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

**On an ARM Mac:** you need a dev container image to get started.
Reach out to the developers if you do not have one.

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

For `local-minimal` specifically, this deploys the full stack: Neo4j,
Cassini, `cert-issuer`, ACE, the ingestion agents, and — as of this
week — Keycloak and its own Postgres backing store, used for
human-facing (as opposed to Kubernetes-workload) certificate issuance.

ACE runs as a Kubernetes `Job` (not a `Deployment`): it requests a
long-lived mTLS client certificate from the cluster's certificate
infrastructure via an init container on every pod creation, which is
what lets it recover automatically if its certificate or environment
ever needs to be refreshed. It authenticates to Neo4j over mutual TLS
and holds no other cluster permissions.

### Loading data into Neo4j

If you have a Neo4j dump file (a `.dump` produced by `neo4j-admin
database dump`, for example from another environment's ingestion
history), load it into the running deployment with:

```bash
just neo4j-load-dump ~/path/to/your.neo4j.dump
```

This scales Neo4j down, restores the dump via a temporary loader pod,
fixes file ownership, scales Neo4j back up, and resets the `neo4j`
user's password to match the cluster's own secret — the whole sequence
is scripted and safe to run against a live deployment, but Neo4j will
be briefly unavailable while it does.

### Accessing the deployment

None of the in-cluster services are exposed outside the cluster by
default. A single recipe brings up detached, auto-restarting
port-forwards for everything you'd normally want reachable from your
own machine:

```bash
just port-forwards-up       # neo4j (7473/7687), keycloak (8080), ace (8050)
just port-forwards-down     # stop them
```

These survive terminal closure and automatically retry on the
occasional dropped connection — a normal one-time step per session,
not something to leave running in a dedicated terminal.

- ACE's dashboard: `http://localhost:8050`
- Neo4j Browser: `http://localhost:7473`
- Keycloak admin console: `http://localhost:8080`

### Bootstrapping Keycloak

`local-minimal`'s Keycloak deploys with no realm, client, or users
configured yet. A single idempotent recipe sets all of it up — safe to
re-run any time:

```bash
just keycloak-bootstrap
```

This creates the `polar` realm, a public OIDC client
(`browser-cert-fetcher`) used for human-facing certificate issuance,
and a test user, using credentials you provide via `sops` (see
`infra/targets/local-minimal/secrets.sops.yaml` — add
`keycloak_test_username`/`keycloak_test_password` there before
running).

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
   environment. In particular, `cert-issuer`'s `issuers` list is where
   trust in Kubernetes and/or an external IdP (Keycloak, Okta, etc.)
   is configured — see the inline comments there for what each field
   means, especially `identityStrategy`'s `clusterDomain` (only needed
   if your cluster's kubelet runs with a non-default
   `--cluster-domain`).
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

### Local development without a cluster

To hack on an individual agent or Cassini itself with plain `cargo
build`/`cargo run` — no Kubernetes, no `cert-issuer`, no Keycloak — use
a static, offline-generated CA and matching certificate pairs instead
of the dynamic issuance flow:

```bash
just tls   # one-time; the generated certs are valid for years
```

This produces `result-tls-certs/`, entirely separate from any cluster's
own CA, with a matching cert pair for Cassini and for Neo4j.

Run the broker directly:

```bash
export TLS_SERVER_CERT_CHAIN=$PWD/result-tls-certs/server/server_cassini_certificate.pem
export TLS_SERVER_KEY=$PWD/result-tls-certs/server/server_cassini_key.pem
export TLS_CA_CERT=$PWD/result-tls-certs/ca_certificates/ca_certificate.pem

cargo run -p cassini-broker
```

And any client connecting to it, on the same machine:

```bash
export TLS_CLIENT_CERT=$PWD/result-tls-certs/client/client_cassini_certificate.pem
export TLS_CLIENT_KEY=$PWD/result-tls-certs/client/client_cassini_key.pem
export TLS_CA_CERT=$PWD/result-tls-certs/ca_certificates/ca_certificate.pem

cargo run -p <agent-binary>
```

A local Neo4j instance can use `result-tls-certs/neo4j/`'s server/client
pair the same way, signed by its own dedicated CA (kept separate from
Cassini's, matching how the two are also isolated in a real
deployment).

**What you actually need depends on which kind of agent you're
running:** an observer only needs to talk to Cassini, so the Cassini
cert pair above is enough. A consumer additionally connects to Neo4j,
so it also needs the matching `TLS_*` env vars pointed at
`result-tls-certs/neo4j/`'s certificates (consult that binary's own
source/config for its exact Neo4j-side env var names, which may differ
from Cassini's).

## Validating human certificate issuance

Kubernetes workloads (ACE, the ingestion agents) get their certificates
automatically, with no manual steps. A **human**, authenticating
against Keycloak, is a newer path this week's work added — there's no
polished client tool for it yet, so this is a manual walkthrough of the
real, verified sequence, useful for confirming the deployment is
actually working end to end.

1. Get a token from Keycloak (after `just port-forwards-up` and `just
   keycloak-bootstrap`, using the same test credentials from
   `secrets.sops.yaml`):

```bash
   TOKEN=$(curl -s -X POST http://localhost:8080/realms/polar/protocol/openid-connect/token \
     -H "Content-Type: application/x-www-form-urlencoded" \
     -d "grant_type=password" \
     -d "client_id=browser-cert-fetcher" \
     -d "username=<your test username>" \
     -d "password=<your test password>" \
     | jq -r '.access_token')
```

2. Decode it to see the exact identity/scope values this specific
   deployment produces (these vary per-realm; don't assume the
   values shown here):

```bash
   echo $TOKEN | cut -d. -f2 | base64 -d | jq .
```

   Look for `preferred_username` and `identity_scope` — the resulting
   certificate's identity is `<preferred_username>.<identity_scope>`.

3. Generate a keypair and CSR whose SAN matches that exact identity:

```bash
   openssl ecparam -genkey -name prime256v1 -noout -out key.pem
   openssl req -new -key key.pem -out csr.pem -subj "/" \
     -addext "subjectAltName=DNS:<preferred_username>.<identity_scope>"
```

4. Port-forward `cert-issuer` itself (not brought up by
   `port-forwards-up`, since workloads reach it in-cluster and this is
   only needed for this kind of manual, from-your-laptop check):

```bash
   kubectl port-forward -n polar svc/cert-issuer 8443:8443 &
```

5. Request the certificate:

```bash
   CSR_JSON=$(jq -n --arg csr "$(cat csr.pem)" \
     '{csr_pem: $csr, cert_type: "CLIENT", extra_sans: []}')

   curl -s -X POST http://localhost:8443/issue \
     -H "Authorization: Bearer $TOKEN" \
     -H "Content-Type: application/json" \
     -d "$CSR_JSON" \
   | jq .
```

   A successful response includes `certificate_pem` and `ca_chain_pem`.
   Decode the certificate (`openssl x509 -noout -text`) and confirm its
   Subject Alternative Name matches what you expected from step 3.

## Data source

ACE reads from Neo4j — it does not talk to Jira, GitLab, or any other
source system directly. The graph it reads from is populated by
**Polar**, the platform this repository is built on: a set of
Cassini-connected ingestion agents (Jira, GitLab, Kubernetes, and
others) that observe source systems and write structured work-item
data into Neo4j. Polar also provides the certificate infrastructure
(cert-issuer, Keycloak) ACE and its users authenticate through, and the
Nix/Dhall/Kubernetes build and deployment pipeline this README's
install steps use.

If you're working on ACE itself, you'll mostly live in src/ace/. If
you're working on the ingestion agents, certificate infrastructure, or
deployment pipeline, see src/agents/ and infra/.

## License

Licensed under a MIT-style license — see license.txt, or contact
permission@sei.cmu.edu for full terms.
