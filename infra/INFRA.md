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

# Polar Infrastructure Pipeline

How Polar is deployed to Kubernetes, and how to add to it: an agent, a
capability, a service, an image, a target. If you are new to this system, read
this first before touching anything under `infra/`.

If you only want to bring a cluster up, read [Day to day](#day-to-day) and stop.
If you are adding something, read [How it fits together](#how-it-fits-together)
once, then go to the recipe you need.

---

## Why this exists

Polar needs infrastructure across several deployment targets: a local
cluster for development, a `sandbox-local` target for a Flux-based deployment (see [How a target works](#how-a-target-works)),
which a cloud cluster (AKS, AWS) would then reuse, and more to come. Each shares a large amount of structural intent
but differs in image registries, TLS configuration, storage classes, service
topology and platform tooling (Flux, Istio, and so on).

The first system (`src/deploy/`) had no clean abstraction for any of this.
Chart files reached for environment variables at Dhall evaluation time, a
`values-active.dhall` was written at runtime by a bash script to simulate target
selection, `constants.dhall` held fully constructed Kubernetes resources, and a
single bash script knew nothing about targets, ordering or dependencies. Secrets,
render logic, target selection and resource definitions were all entangled.

The layered pipeline fixed that: Dhall as pure transformation, nushell owning the
pipeline, charts as self-contained modules, targets that declare rather than
implement.

It left a second problem. Every agent was still a hand-written chart repeating the
same Job skeleton, and the facts that distinguish one agent from another (its
image, its dependencies, its secrets, its RBAC) were scattered across `target.nu`,
`overrides.dhall`, `constants.dhall`, each chart's `main.nu`, `render.nu` and the
Rust code. Per-agent TLS configuration was already identified as the largest
single source of configuration drift (see
`docs/architecture/cert-issuer-architecture.md`). The **configuration catalog**
addresses that: an agent is a short entry of data, and what each dependency means
is stated once. The design record is
`docs/architecture/config-catalog-design.md`.

---

## The idea in one table

Everything under `infra/` belongs to one of three tiers. Each has one job, and a
change should land in exactly one of them.

| Tier | Job | Lives in | Rule |
|---|---|---|---|
| **Configuration** | The facts: what exists and what it needs | nuon files (`infra/config/**`, `infra/images.nuon`, `infra/targets/<t>/config.nuon`) | Data only. No logic. |
| **Structure** | The shape of a Kubernetes object | Dhall (`infra/schema/**`, the custom charts under `infra/layers/**`) | A pure function of a typed record. No facts of its own. |
| **Automation** | The verbs: resolve, render, apply, wait | nushell (`up.nu`, `render.nu`, `resolve.nu`, `render-catalog.nu`, `images.nu`) | Orchestration. As thin as it can be. |

Between configuration and structure sits the **resolver** (`resolve.nu`). It reads
the data, applies layering and capabilities, and produces one fully resolved
record per workload. It is a pure function: no cluster, no network, no files
written. That is what makes it testable and what makes `explain` possible.

**Variation between agents is data.** An agent that writes to the graph says
`needs: ["graph-write"]`. It does not get a `hasGraph` flag in a template. If you want a
boolean in a template to make one workload fit, stop: either the variation belongs
in a capability (data), or the workload is a custom chart (see
[the escape hatch](#add-a-custom-chart-the-escape-hatch)).

---

## Design principles

**1. Dhall is purely a data transformation layer.** Dhall files take a well-typed
record as input and produce Kubernetes resources. They do not reach for
environment variables, make target-specific decisions, or know about other
charts. A Dhall file is a pure function: given values, produce resources. Keep it
simple: plain records and lists, `Optional` instead of unions, no Text comparison.
Complex Dhall is its own hell, so push logic into the resolver, where it can be
tested.

**2. Nushell owns the pipeline.** Target selection, environment validation, secret
threading, render ordering and apply ordering are nushell concerns. Nushell is
data-native and sits naturally between Dhall objects. The pipeline has explicit
seams at every level.

**3. Configuration is data, and it is stated once.** A fact (an address, an image
tag, a dependency) has one home. Other places derive it.

**4. Custom charts are self-contained modules.** A chart directory holds its Dhall
files (one per functional concern) and a `main.nu` that is its local entrypoint.
`render.nu` does not know the internals of any chart.

**5. Layers express real dependencies.** Platform primitives must exist before
services, services before workloads, workloads before anything there is to
observe. The directory structure makes that order visible.

**6. Targets declare, they do not implement.** A target declares which charts it
needs, in what order, and what to override. It does not re-implement chart logic.

**7. Rendered manifests are the contract.** A refactor is correct when the render
before and after is the same; a feature is correct when only the intended
differences appear. Compare, do not eyeball.

---

## Day to day

Run everything from the repo root with `KUBECONFIG` pointing at the cluster.

```fish
# Bring a cluster up from a target (resumable: re-run the same command after a failure or pause)
nu infra/up.nu local-minimal --dev          # --dev builds and pushes the images the target needs

# Only render (nothing is applied); output goes to manifests/ (gitignored). An agent says what it takes in its own image, so the
# render needs each agent's scaffold; up.nu collects them from the cluster (cached by the image's digest), and so can you:
nu infra/resolve.nu collect-scaffolds /tmp/scaffolds --target local-minimal
POLAR_SCAFFOLD_DIR=/tmp/scaffolds nu infra/render.nu local-minimal

# A cluster whose nodes name the registry differently (the default, 10.0.2.2:5000, is the host as a podman dev node sees it)
POLAR_NODE_REGISTRY=polar-registry:5000 nu infra/up.nu local-minimal --user U --email E

# Render without the cache, to see the true cost or to rule it out (see "The render cache")
POLAR_CACHE=off nu infra/render.nu local-minimal

# What does the catalog say about a workload, and where did each value come from?
nu infra/resolve.nu explain kube-consumer

# Is the whole catalog consistent? (no cluster, no render)
nu infra/resolve.nu check

# Images
nu infra/images.nu list                     # every image the repo knows how to produce
just images-build kube-observer             # stage 1: build only (needs nix; no registry, no cluster)
just images-push kube-observer              # stage 2: build and push to the target's registry
just images-mirror                          # stage 3: copy third-party images into the target's registry
just image kube-observer                    # every stage (alias: cluster-load)
just images-all                             # every stage for everything in the table
# Each prints the images.nu command it runs. The target is POLAR_TARGET, else local-minimal (just target=<t> ...); its
# registry is the one in its cluster, through a forward the recipe starts. `just target= ...` uses POLAR_REGISTRY instead.
nu infra/images.nu import --from docker.io/<account>    # copy published images into your registry (this machine's architecture)
nu infra/images.nu publish --to docker.io/<account> --arch arm64    # publish them (a mirror only when named)

# A Flux (gitops) target: up.nu runs these; each can be run alone
nu infra/deploy-repo.nu ensure sandbox-local        # the deployment git server (in the cluster, namespace polar-deploy) and its PKI
nu infra/deploy-repo.nu publish sandbox-local       # commit the layers there, signed with the pipeline's certificate
nu infra/flux.nu install sandbox-local              # Flux, pointed at it (refuses a head that is not signed)
nu infra/flux.nu wait sandbox-local                 # until every layer is Ready at the commit that was published
```

The `build`, `push` and `mirror` recipes take no names to mean every image. A
failing image does not stop the stage: the rest are attempted and the stage ends
with a summary and a non-zero exit. The node cache step of `image` and
`images-all` is skipped unless `POLAR_CLUSTER_RUNTIME` is `u7s` (the default) and a nix-usernetes node runs here: it drops
the rebuilt image from every node of the cluster `KUBECONFIG` reaches, the one `u7s` container or each `<cluster>-<node>`
of a multi-node cluster such as `u7c` (not its hub). The pods pull Polar's images `Always`, so this frees disk and
nothing more.

`up.nu` prints its plan first and skips what the target does not need. Its steps:
preflight, images (`--dev` or `--import-from`: every image the target runs, before the render), render, namespaces, cluster CA, secrets and
infrastructure, waiting for core services, port-forwards, admin flow, the seed of the schedules repository,
browser certificate and Neo4j password. A gitops target (`gitops: true`) replaces "secrets and infrastructure" with
secrets, the deployment git server, publish, install Flux and wait for Flux, and then runs the same steps from "waiting for
core services" on. It stops for a human only where a human is
required, and looks first, read-only, so it stops only for what is missing: the browser step when the
Librewolf profile does not hold this cluster's CA and the user's current certificate (closing the browser
and importing it), and the Neo4j password step only when Neo4j is still on its factory password. With the
`nix-neo4j` image that applies `NEO4J_AUTH` on its first start (the one this repository deploys), Neo4j starts
with the password the agents use and there is no password step at all.

---

## Directory structure

```
infra/
  up.nu                  brings a cluster up from a target (applies); plan.nu decides what it runs
  render.nu              renders a target (nothing is applied)
  resolve.nu             the resolver: explain, json, resources, diff, check, scaffolds, collect-scaffolds
  render-catalog.nu      renders every catalog workload (through the render cache)
  dhall-render.nu        dhall-to-yaml for the hand-written charts (through the render cache)
  cache.nu               the render cache: where it lives, how a key is made, read or compute
  digest.nu              an image's digest in a registry, by a manifest HEAD request
  registry.nu            the registry the nodes pull from: POLAR_NODE_REGISTRY, the target's, the default
  cluster-inputs.nu      what belongs to one cluster and exists only after it does (the git server's administrators)
  agent-pods.nu          asks an agent's image about itself (--scaffold, --check) as a pod on the cluster
  document.nu            how the resolver fills an agent's configuration document
  rules.nu               checks on what a workload resolves to and on a render (image pull policies, restarts in place, Job replacement)
  compare-manifests.nu   compares two renders by meaning
  layers.nu              groups a target's files into layers (check, split)
  images.nu  images.nuon which images exist, and building, pushing, importing and publishing them
  lock.nu                the image lock: which bytes each image is (images.lock.nuon; images.dev.lock.nuon is this machine's)
  release.nu  semver.nu  changelog.nu    a release: the version, the gates, the lock, the changelog and the tag
  deploy-repo.nu         the deployment git server (ensure) and the signed commit of the layers to it (publish)
  deploy-objects.nu      what that commit holds besides the manifests: the Flux objects and the provenance
  flux.nu                Flux: export its controllers, install it, wait for the layers
  seed.nu                what a cluster is given before anyone writes anything: signed documents
  secrets-lib.nu         what the targets' secrets.nu scripts share
  tools.nu  runtime.nu   the standalone Polar tools; which container runtime (podman or docker)
  validate.nu            environment validation helpers
  apply.nu               the decisions up.nu makes while applying: what to restart or roll out, which images are stale
  retry.nu               try again for a while, and stop at once on a failure that cannot be fixed
  forwards.nu            wait for a port-forward to answer; restart it if it does not
  admin.nu               what to do with ~/.config/polar-admin that belongs to another cluster
  browser.nu             does the browser hold this cluster's CA and the user's certificate, and has Neo4j's
                         password been changed (read-only)
  egress.nu              the check at the end of up.nu: can a pod reach a host outside the cluster, and if not, why
                         (a note, never a failure)
  jira-check.nu          jira-observer's Jira connection, checked from a pod in the cluster, in facts safe to share
                         (no host, URL, address, email or token), and what they mean
  check-sources.nu       every git source in a Cargo.lock or flake.lock still fetchable anonymously at its pinned
                         revision (just check-sources; also run by just static-analysis and ci.nu's phase 0)
  cassini-shutdown.nu  harness.nu  measure-render-cache.nu     asking the broker to stop; reading the secret harness; timing the cache
  tests/                 one test file per script: nu infra/tests/<name>.nu
  config/                the catalog
    cluster.nuon         domain, namespaces, registry, token audience
    services.nuon        in-cluster services other things connect to
    kinds.nuon           defaults shared by every workload of a kind
    capabilities.nuon    what each `needs` entry contributes
    facets.nuon          the operations every capability must answer
    workloads/*.nuon     one entry per catalog workload
  schema/                Dhall vocabulary
    agent.dhall  agent-spec.dhall          the agent template (a Deployment, or a run's Job) and its input type
    resources.dhall  resources-spec.dhall  RBAC and token Secrets alongside a workload
    functions.dhall  constants.dhall ...   shared helpers and static constants
  layers/                custom charts, by dependency
    1-platform/          must exist before everything else
      infra/             namespaces, storage
      services/          flux (the controllers, regenerated by flux.nu export); cert-manager and networking
                         are unused: cert-issuer replaced cert-manager, and no target renders either
    2-services/          shared services: cert-issuer, cassini, neo4j, jaeger, ...
    3-workloads/agents/  agents not yet in the catalog
    4-observed/          infrastructure Polar observes (empty so far)
  targets/<t>/
    target.nu            charts to render and the apply order
    overrides.dhall      values for the custom charts
    config.nuon          overrides for the catalog (cluster facts, per-workload fields)
    secrets.sops.yaml  secrets.nu   encrypted secrets and the script that creates them
    conf/                static config files (neo4j conf, ...)
```

The numeric prefixes on the layers are deliberate: the dependency order shows in
every directory listing.

### Layer model

- **1-platform.** Everything that must exist before any service or workload.
  `infra/` is the raw substrate (namespaces, storage classes) applied first;
  `services/` holds platform services (Flux, and the unused cert-manager and Istio networking charts). Not
  every target uses all of them: `local` has no Flux or Istio. That is a target
  declaration, not a chart concern.
- **2-services.** Shared stateful infrastructure workloads depend on: Neo4j,
  Cassini (the mTLS broker), Jaeger, cert-issuer, Keycloak, Postgres. They are not
  Polar-specific.
- **3-workloads.** The agents that observe infrastructure and populate the graph.
  Agents that are in the catalog have no directory here; they are entries in
  `config/workloads/`.
- **4-observed.** Staging and test infrastructure Polar watches. Empty so far, but
  its place in the dependency order is right.

---

## How it fits together

```
infra/config/*.nuon             cluster, services, kinds, capabilities, facets
infra/config/workloads/*.nuon   one entry per catalog workload
infra/images.nuon               which images exist and where they come from
infra/targets/<t>/config.nuon   per-target overrides
        │
        ▼   resolve.nu          pure; layering, capabilities, substitution
a resolved record (JSON)
        │
        ▼   json-to-dhall       typechecks it against schema/agent-spec.dhall
        ▼   schema/agent.dhall  the one template for agents (a Deployment, or a run's Job)
        ▼   dhall-to-yaml
manifests/<name>.yaml           (+ manifests/<name>-resources.yaml if it has RBAC etc.)
```

`render.nu` does this for every catalog workload (through `render-catalog.nu`),
after rendering the custom charts. Then it runs checks over the whole output: each
workload has a unique ServiceAccount (it becomes the SAN in the workload's
certificate), cert-issuer's TLS identity matches the URL callers use, and the CA
provisioning mode is consistent. It emits a ServiceAccount for every workload
identity no manifest declares. `up.nu` then applies the files in the target's
`apply_order`.

Custom charts follow the original pipeline:

```
target.nu  ->  render.nu  ->  chart main.nu (one per chart)  ->  manifests/*.yaml
  config      validates env,   dhall-to-yaml per Dhall file,
  record      builds context   writes YAML
```

### The vocabulary

A workload declares `needs`, a list of **capabilities**. Each capability answers
every **facet**:

| Facet | What it answers |
|---|---|
| `pod` | env, volumes and mounts the agent container gets |
| `identity` | client certificates polar-init obtains from cert-issuer (each also adds its volume, mounts, and a health-checked cert path) |
| `dependency` | the service whose certificate must be valid before the agent starts, and for how long |
| `init` | what polar-init is given besides the certificates it issues: environment and mounts |
| `resources` | Kubernetes objects that live alongside the workload (cluster roles, token Secrets, persistent volume claims) |

Today's capabilities are `cassini`, `graph-write`, `graph-read` (the same Neo4j
environment and certificate as `graph-write`, with no `dependency` and no graph
watch in the health check: the state agent's, `docs/architecture/soft-state.md`), `encryption`,
`kube-api-read`, and others that describe the shape of the pod rather than what
the code needs: `persistent-data` (a claim named for the workload, mounted at
`/var/lib/<workload>`), `pinned-ca` (polar-init and the container trust the Polar CA
from a mounted file), `tracing` (every workload sends its traces to Jaeger, from its kind) and `oci-registry-auth` (registry credentials mounted as a Docker config). An agent on a document says what its
code requires in its own scaffold; its entry adds what else the deployment gives
it with `provides`, lists services it watches but does not wait for at start with
`no_start_wait`, and, if it answers requests, a `service` (a port, which becomes
the container port, a Service, and the value of the variable the agent reads it
from). The credentials agent (`config/workloads/credentials.nuon`) uses all of
them. Every `agent` gets `cassini` from its kind. `encryption` is an
opt-in marker with no footprint yet (see
[Capabilities and the Rust agent builder](#capabilities-and-the-rust-agent-builder)).
From a workload's `needs` the resolver derives, without anyone writing them per
agent:

- the init container's `--check-dep-cert host:port:ttl` arguments (don't start an agent whose dependency's certificate is about to expire)
- `POLAR_HEALTH_DEP_ENDPOINTS` and `POLAR_HEALTH_CERTS` for the health actor
- `POLAR_CAPABILITIES`, the comma-joined `needs`, which the agent's own builder checks at startup
- the volumes, mounts and `--cert` specs for every client certificate

From the kind's `health` and `shutdown` blocks (a workload overrides any of them) it
derives the rest of the health and shutdown configuration:

- `POLAR_HEALTH_EXPIRY_SECS`, `_TICK_SECS`, `_REJUVENATION_SECS` and `_STALE_SECS`, which the
  health actor validates together when the agent starts and the liveness probe reads
- `POLAR_SHUTDOWN_BUDGET_SECS`, the most the agent may take to drain once told to shut down
- the pod's `terminationGracePeriodSeconds`, which is the budget plus `shutdown.grace_margin_secs`
  and is **never set on its own**, so it cannot disagree with the budget. Kubernetes sends
  SIGKILL when it ends, and the agent's own watchdog exits at the budget, so the pod has to
  be allowed to live longer than that.

`nu infra/resolve.nu settings <workload>` prints the numbers the rules below read.

### Layering

For scalar and record fields, later wins: **kind defaults** (`kinds.nuon`) <
**workload entry** (`workloads/*.nuon`) < **target override**
(`targets/<t>/config.nuon`, under `workloads.<name>`). Lists (`needs`, `env`) are
unioned in declared order instead of replaced. `explain` prints each value with
its origin: `[kind]`, `[entry]`, `[target]`, `[<capability>]` or `[derived]`.


### What the catalog can and cannot express (read from the code, 2026-10-23)

Read while planning cert-issuer's move into the catalog (`docs/architecture/cert-issuer-isolation.md`, step 1c).

- **One template for agents, a Job, always with polar-init.** `render-catalog.nu:48` maps `agent` and `service` to
  `schema/agent.dhall`, and a kind with no template fails the render (`render-catalog.nu:91`: "no template for kind").
  *Since 2026-10-23 there is a second template, for the `ca` kind, cert-issuer's alone (`schema/ca.dhall`,
  `schema/ca-spec.dhall`): the same Job with no polar-init, the kind's projected token mounted in its own container, the
  Kubernetes API token mounted, a Secret's file `mode`, and the pod's `fs_group`; see
  `docs/architecture/cert-issuer-isolation.md`, step 1c.*
  *Since 2026-10-28 it renders a Deployment of one that restarts in place, or a `run`'s Job
  (`functions.workloadOf`; `docs/architecture/deployments.md`, step 4).*
  The template renders one `Job` (`agent.dhall:226-246`), and its init container is unconditional
  (`agent.dhall:173-194`, `functions.dhall:223-259`); capabilities' `init` parts only add env and mounts to it. Its
  header: "If a workload does not fit this shape, it stays a custom chart (the escape hatch). Do not add a Bool to the
  Spec to make it fit" (`agent.dhall:30-31`).
- **Only document agents.** `resolve.nu:297` refuses an entry without `config: "document"`, and `:301` one that lists
  `needs`: the scaffold says what the code requires, and `provides` adds what the deployment gives (`:307-308`).
- **The scaffolds need the registry, and usually the cluster.** `scaffold_of` (`resolve.nu:172-182`) reads
  `$POLAR_SCAFFOLD_DIR/<workload>.json` only. `collect-scaffolds` (`agent-pods.nu:302-327`) asks the registry for each
  image's digest, and on a cache miss runs `<image> --scaffold` in a pod on the cluster. `resolve.nu check` also runs
  each agent's `--check --config` there (`:211-221`). With the cluster down, `render.nu <target> --charts-into <dir>`
  renders the charts alone, and a full render works only when `POLAR_SCAFFOLD_DIR` already holds every scaffold.
  `infra/tests/resolve.nu` collects them on the cluster, unless `POLAR_SCAFFOLD_DIR` is already set: then it uses that
  directory and leaves it in place. The four tests that run the agents' own `--check` still need the cluster.
  `rules.nu` and `charts.nu` need nothing. *Since 2026-10-27:* each agent binary prints its own scaffold
  (`<binary> --scaffold`), so the workspace's builds give a scaffold directory without a cluster.
- **Volumes.** A capability's `pod.volumes` entry is a Secret (with `items`), a claim, a ConfigMap or, with none of
  them, an emptyDir (`capabilities.nuon:32-50`). There is no `defaultMode`, and the pod has no `securityContext`
  (`agent.dhall:166-224`). `persistent-data` is a claim named `{workload}-data` at `/var/lib/{workload}`
  (`capabilities.nuon:136-145`).
- **The projected token** is the kind's `sa_token` only (`kinds.nuon:77`, `resolve.nu:315-317`), for
  `cluster.nuon`'s `sa_token_audience`, and is mounted in polar-init alone (`functions.dhall:256`). A capability's
  volume never has an audience (`resolve.nu:378`).
- **The Kubernetes API token** is not mounted (`automountServiceAccountToken: false`) unless a capability sets
  `sa_token_secret` (`resolve.nu:518`); only `kube-api-read` does, and it grants read on everything
  (`capabilities.nuon:113-129`).
- **Health dependencies** come from `needs`: every dependency's endpoint is in `POLAR_HEALTH_DEP_ENDPOINTS`
  (`resolve.nu:436`, `:447`), which a rejuvenation waits on; `no_start_wait` only drops it from polar-init's checks
  (`:312`, `:532`).
- **The custody daemon (2026-10-27; `docs/architecture/key-custody.md`, step 4).** With `cluster.nuon`'s
  `custody.enabled` on, which is the default, and the kind's and entry's `custody` true (the `ca` kind's is false), the
  resolver (`custody_of`) makes every `--cert` `custody`, leaves out each capability env entry marked `key_of`, adds the
  socket's emptyDir in all three containers and `POLAR_CUSTODY_SOCKET` to the document and polar-init, refuses two
  certificates of one type (`rules.nu`, `custody_cert_problems`), and gives the template the record `custody`. The
  template renders the daemon as a native sidecar before polar-init, and polar-init's client environment without
  `TLS_CLIENT_KEY` (`Constants.custodyClientEnv`, `functions.makePolarInitContainerWith`). `resolve.nu images` lists
  the daemon's image for every pod that has one. A target with it off renders byte for byte what rendered before. A
  workload whose code can only read a key file sets `custody: false` (the scheduler observer, whose `git` reads one).
- **The floor** (`resolve.nu floor`) builds each workload's record from its spec and scaffold alone (`floor_record`,
  since 2026-10-23; it fully resolved every workload before, which cert-issuer's own resolve could not do without
  recursing) and writes, for a
  bootstrap workload with exactly one certificate, an entry with its SANs, its declared and family topics and its
  scaffold's groups; for a non-bootstrap workload on the credentials agent with a server certificate, a `vouchedOnly`
  entry (`rules.nu floor_entries`, `:194-238`). The resolver writes it into cert-issuer's document
  (`CERT_ISSUER_BOOTSTRAP`) when the floor is enforced, with the entries `render.nu` hands it for what is rendered
  outside the catalog (Neo4j's, from its chart's values, in `POLAR_FLOOR_EXTRA`).
- **Waiting at apply.** A layer's `wait_for` with `ref: "name=…"` resolves only to a Deployment, StatefulSet or
  DaemonSet; `job-name=…` resolves to the Endpoints of a Service selecting the Job's pods (`layers.nu:36-37`,
  `:72-80`, `deploy-objects.nu:81-111`), which is how Cassini, a Job, is waited for. *Since 2026-10-28 the targets wait on
  `name=…`, every agent being a Deployment, checked as itself.*
- **`render.nu`'s cert-issuer checks** (`check_cert_issuer_identity`, `check_ca_provisioning`) read the chart's
  `cert-issuer-configmap.yaml` and returned without a word when it was absent. *Since 2026-10-23 they read cert-issuer's
  catalog document (`cert-issuer-resources.yaml`), and fail when cert-issuer is rendered and its document is not there.*
- **A value in an entry or a target** may be a list or a record, which the resolver writes as JSON (`value_text`): the
  issuers cert-issuer trusts are written so in each target's `config.nuon`.
- **Health dependencies and Cassini:** an agent whose scaffold says `depends_on_cassini: false` (cert-issuer) is not
  given Cassini as a health dependency, so its rejuvenation does not wait on the broker.
- **`served`** (`topics.nu:32-38`), the workloads a topic family expands for, counts kind `agent` only, so neither
  Cassini (`service`) nor cert-issuer (`ca`) is given a scheduler configuration topic.
---

## How a custom chart works

```
neo4j/
  statefulset.dhall     one Dhall file per functional concern
  service.dhall
  configmap.dhall
  pvcs.dhall
  values.dhall          canonical default values for this chart
  main.nu               local entrypoint: owns rendering for this chart
```

`values.dhall` is the chart's public interface: the typed defaults it expects. The
Dhall files are pure functions over a values record, importing types and helpers
from `schema/`. `main.nu` receives a context record from `render.nu` (`chart_dir`,
`output_dir`, `overrides`, and any extra facts `render.nu` passes for that chart),
merges the target's overrides over `values.dhall`, runs `dhall-to-yaml` on each
Dhall file in order, and writes YAML to the output directory. It is the only place
that knows the chart's internals.

## How a target works

```
targets/local-minimal/
  target.nu         setup logic + the configuration record
  overrides.dhall   per-deployment values for the custom charts
  config.nuon       overrides for the catalog
  secrets.sops.yaml encrypted secrets
  secrets.nu        creates the Secrets and ConfigMaps from them
  conf/             static config files
```

**`target.nu`** is an executable nushell script, not a data file. It can run setup
logic (resolve paths, read the environment) and prints one nuon record: `charts`
(the `main.nu` paths to render, in dependency order), `apply_order` (the output
files, in order), `output_dir`, `overrides`, `required_env`, plus the cluster
facts still passed to the custom charts (`neo4j_bolt_addr`, `cert_issuer_url`,
`polar_init_image`, `identity_lifetime_overrides`, and so on). Different targets
can have completely different orderings.

`sandbox-local` is the target for a Flux-based deployment: `local-minimal` plus a `flux` layer holding the controllers
(`nu infra/flux.nu export` regenerates them). The design, the artifacts and what was found by running it are in
`docs/architecture/cluster-bootstrap.md`. It declares `gitops: true`, so `up.nu` does not apply its layers:

1. It creates the namespaces, the cluster CA and the secrets, which Flux does not own and never prunes.
2. `deploy-repo.nu ensure` applies the deployment git server in the cluster, in a namespace of its own (`polar-deploy`) that no layer
   holds, so Flux neither applies nor prunes it, with a PKI of its own (`~/.config/polar-deploy/<target>/`: a CA, the server's certificate,
   and client certificates for `pipeline`, which writes, and `flux`, which reads). The CA key never leaves this machine; the cluster holds
   the server's certificate and key and the CA certificate, in a Secret. The server is `polar-deploy-git.polar-deploy.svc.<domain>` on its
   certificate; this machine reaches it through a port-forward (`just port-forwards-up deploy`, localhost:9443). A server whose repository
   is empty (a new cluster) is given the history of the mirror `publish` keeps beside the PKI (`mirror.git`).
3. `deploy-repo.nu publish` commits the layers there as one tree, signed with the pipeline's certificate (git's x509 support, `gpgsm`, in a
   GnuPG home beside the PKI; it never touches `~/.gnupg` and never prompts). The tree is `layers/<layer>/` (every layer but the `flux`
   one), `flux/` (a GitRepository and a Kustomization per layer), `provenance.json` and a README. It refuses a render that is not there, an
   image not pinned by digest, and a dirty tree unless `up.nu` is given `--allow-dirty`, and it commits nothing when the render has not changed.
4. `flux.nu install` clones that repository as the `flux` identity and refuses a head that is not signed by the pipeline's certificate before
   it touches the cluster; then it applies the controllers, the `flux` identity's Secret, and the Flux objects. Flux reaches the server
   by its Service.
5. `flux.nu wait` follows every layer until it is Ready at the published commit. If a layer is waiting on a health check begun under an
   older spec or revision (Flux finishes a reconcile under the spec it began with, up to its ten minute timeout), it restarts the
   kustomize-controller after 15 seconds, at most three times.

Then `up.nu` runs the steps that need workloads: the admin flow, the seed and the browser.

How a layer's `wait_for` becomes a Flux health check: a `statefulset` or a Deployment is checked as itself, and every agent is a
Deployment (waited on by its pods' `name` label). A Job is not: Flux's own Job reader counts a Job as healthy only when it has completed,
so one that runs for good would be waited for through the Endpoints of the Service that selects its pods, and one with no Service is not
waited for; no layer holds such a Job now (the load harness's are rendered and never applied). A layer with a Job is applied with
`force: true`, since a Job's pod template is immutable. A workload that reads a ConfigMap carries a fingerprint of it in its pod template
when it is published (`polar.dev/config-hash`), so a changed ConfigMap changes the Deployment and Flux rolls it out.

**The registry.** The registry as the nodes name it is the prefix of every image in a manifest, and it is one value, resolved by
`infra/registry.nu`: `POLAR_NODE_REGISTRY` in the environment, else the target's `cluster.registry` in its `config.nuon`, else
`config/cluster.nuon` (`10.0.2.2:5000`, the host as a podman dev node sees it). `render.nu` resolves it once and hands it to the hand-written
charts in the environment, where their Dhall reads it (`env:POLAR_NODE_REGISTRY as Text`); the render cache's key includes the value of every
environment variable a Dhall file imports. It is not the registry as this machine reaches it (`POLAR_REGISTRY`, `localhost:5000`: where
images are pushed and digests asked).

**Cluster inputs.** What belongs to one cluster and exists only after it does is not committed. Today that is the git server's
administrators: the first admin's identity is made by the admin flow on the live cluster, and the git server must know it. `up.nu`'s seed
step writes `targets/<target>/cluster-inputs.nuon` (gitignored; `infra/cluster-inputs.nu`), in the shape of `config.nuon`'s
`workloads.<name>.env`, tied to the cluster by the sha256 of its CA, and renders and publishes again. The render lets those entries replace a
same-named one in the target's config; a file made for another cluster is discarded before the render, so a reset starts clean. The second
signed commit's `provenance.json` names the file, its digest, the cluster and the values (`cluster_inputs`), so the provenance trail shows
who was made administrator, and the tree stays clean.

**Pull policy.** An image named by a tag that moves (`latest`, or no tag) is pulled `Always`: a node that keeps the first copy it pulled
(`IfNotPresent`) never sees a rebuild, and the pod runs old code that looks like a bug in the new. The catalog sets it in `kinds.nuon`, the
custom charts in their `values.dhall`, and `rules.nu` (`pull_policy_problems`) fails a render that leaves a moving tag on anything else; a
digest pin or a version tag may stay `IfNotPresent`. The cost is a request to the registry for every container that starts, which the
kubelet limits (5 a second, a burst of 10): 24 pods at once took 18 s against 1.3 s. The `u7s` node lifts the limit
(`registryPullQPS: 0`, nix-usernetes); on another cluster set the kubelet's `registryPullQPS`, or accept the retries. After the core
services are up, `up.nu` also says, by pod, when a running container's image is not the one the registry serves.

**The image lock.** A manifest that names an image by tag alone does not say which bytes run: a rebuilt image under the
same tag is a different image behind the same name, and a GitOps tool cannot see the change. A target with
`pin_images: true` (`sandbox-local`) has every image of the node registry written as `name:tag@sha256:...`, from the
lock (`infra/lock.nu`): `infra/images.lock.nuon`, committed, with this machine's `infra/images.dev.lock.nuon` (ignored)
laid over it. An entry is `{image, tag, arch, digest}`, and `arch` (`cluster.nuon`) picks the digest, since an image has one
per architecture. `images.nu` records the digest after every push and mirror (and pushes with an explicit
`--format v2s2`: the digest depends on how an image is pushed, so it is read from the registry, never recomputed);
`nu infra/images.nu lock refresh` records what the registry already holds, `lock show` prints what a render would use, and
`lock promote` makes this machine's lock the committed one, which a release does. A render fails, listing them, for an image
the lock has no digest for. A digest pin also ends the stale-node-cache problem for a pinned target: the node's cache is
keyed by digest.

**The platform version.** One version for every Polar image, calculated, not chosen: `nu infra/semver.nu suggest` classifies every
non-merge commit since the previous release (the latest `vX.Y.Z` tag, else `infra/release-baseline.nuon`) and the highest significance
wins. A conventional commit header is trusted; a revert and what it reverts cancel; a message that is not conventional is *inferred*
from its leading verb (never as breaking) or, if it says nothing and touches code, counted as a patch; a `BREAKING CHANGE:` footer makes any
commit breaking. `infra/release-overrides.nuon` corrects a commit by sha without rewriting history. Until the major version is 1, breaking
bumps the minor version and a feature or a fix the patch. From the baseline (v0.1.0, before the first conventional commit) the history
gives `v0.2.0`, with or without the inferred commits (`infra/tests/semver.nu` replays it).

**The changelog.** `nu infra/changelog.nu preview|write` writes the section of `CHANGELOG.md` for a release, from the same
classification that chose the version, so the two cannot disagree. A section has, only where there is something to say: the highlights
(a few sentences a person writes at the release), the breaking changes with what each says to do (from its `BREAKING CHANGE:` footer),
the images that changed (from the lock), then the features, fixes and performance changes grouped by scope, one sentence that counts
what is not listed (documentation, housekeeping, refactoring, tests), and a collapsed list of the changes that were not written as
conventional commits. A type that was inferred from a commit's wording carries a dagger. A list longer than `fold_over` (40) becomes a
count per scope with the entries collapsed, which is how the first release, 368 features, stays readable. Writing replaces only that
version's section; older sections, including hand edits, are untouched. The look is `infra/changelog.nuon`.

**The release.** `nu infra/release.nu run` cuts one, and never pushes. It checks the tree is clean and on a branch, runs the gates
(`gates` alone runs them without releasing; `--only` and `--skip-gate a,b` choose), shows the calculated version and its evidence and
asks for the version (the default is the suggestion; it must be after the previous release and not yet a tag), asks for the highlights,
builds and pushes every image once, writes the committed `infra/images.lock.nuon` and the changelog section, and commits
`chore(release): vX.Y.Z` with an annotated tag. `--dry-run` stops after the changelog preview. The gates (`infra/release.nuon`):
`unit` (the infra tests), `deny` (advisories, licenses, bans, sources), `secrets` (nothing in the published history beyond
`infra/release-secrets-baseline.nuon`), `clippy` (report-only until the workspace's warnings are cleared), `resolve` and
`determinism` (two renders identical). The secret scan shows a finding's rule, path and commit, never what it matched;
`release.nu secrets accept ID... --reason` (or `--placeholder-like`) records one you have looked at, with the reason.

A target can also group its files into **layers** (`apply_layers`; `local-minimal` has six: platform,
cert-issuer, data, cassini, identity, agents; `sandbox-local` adds `flux`). A layer is a set of files applied together after the layers it
`depends_on` are healthy, with the workloads to wait for in `wait_for` (the same pod selectors and
StatefulSets `up.nu` waits for, except one that cannot be Ready until a later layer is up: Neo4j needs the credentials agent, so `sandbox-local`'s data layer waits for nothing). Where a target declares layers, its `apply_order` is derived from them, so
`up.nu` applies exactly what it did, and `render.nu` fails when a rendered file is in no layer or in two.
`nu infra/layers.nu split <target> <dest>` copies a render into `<dest>/<layer>/` with a `layers.json`:
the shape a GitOps tool reconciles one layer at a time (see the Flux plan in `remaining-work.md`).
Declare a new file in the layer it belongs to; the chain is linear, each layer after the one before it.

**`overrides.dhall`** is a flat record merged over each custom chart's
`values.dhall`. It holds only what differs from the defaults for this target:
image references, pull secrets, storage classes, hostnames, URLs.

**`config.nuon`** is the catalog's override layer for this target:

```nuon
{
  cluster: { registry: "registry.example.com" }     # any field of cluster.nuon (POLAR_NODE_REGISTRY overrides the registry)
  workloads: {
    "jira-observer": { backoff_limit: 5 }           # any field of a workload entry
    "oci-registry-resolver": { enabled: false }     # leave a catalog workload out of this target
  }
}
```

**A target's way out of the cluster** is `cluster.egress`, every key optional: `proxy` (an `http://` or `https://`
URL with a host and a port), `no_proxy` (hosts, `.suffixes`, addresses and CIDRs reached directly, beside
`localhost`, `127.0.0.1`, `::1`, `.svc` and `.<domain>`) and `ca_file` (PEM CA certificates, a path inside the
target's directory). Only the agents whose code declares the egress section are given it (jira-observer,
oci-registry-resolver and git-repo-observer): the variables in their document, and the CA as a ConfigMap of their
own, mounted. A target that sets none renders as before. `infra/egress.nu` judges it; the design, what each agent
does with it and what is not done are in `docs/architecture/egress.md`.

**A target leaves a workload out** with `enabled: false`. The catalog is every workload there is, and every
command resolves the ones a target runs, so a disabled workload is not resolved, checked, rendered, listed,
given an image build, or part of the cert-issuer floor for that target. Resolving it by name says it is not enabled
there. Two things are checked: `enabled` is `true` or `false` and nothing else, and a workload the target disables
is not still in its `apply_order` (its manifests are not rendered, so `up.nu` would skip them silently). The catalog
entry is untouched, so another target runs the workload. (A key under `workloads` that names no workload is still
ignored without a word.)

---

## The schema directory

`schema/` is the Dhall vocabulary:

- **`agent.dhall`, `agent-spec.dhall`**: the agent template and the record type it takes: a Deployment of one that restarts in
  place, or, for the `run` kind (the load harness), a Job (`docs/architecture/deployments.md`, step 4).
- **`resources.dhall`, `resources-spec.dhall`**: the objects that live alongside a workload (ClusterRole, binding, token Secret).
- **`kubernetes.dhall`**: Dhall bindings for Kubernetes types.
- **`functions.dhall`**: shared helpers (`makeDeployment`, `makePolarInitContainer`, the proxy cert helpers, ...).
- **`agents.dhall`, `cassini.dhall`, `storage-class.dhall`, `identity-strategy.dhall`, `ca-provisioning.dhall`**: type definitions.
- **`constants.dhall`**: static values: namespace names, service names, TLS paths, ports, labels. Its header says it holds no constructed resources and no environment-dependent values, but it still contains a few (`certEmptyDirVolume`, `saTokenVolume`, `commonClientEnv`); they are on the list to move out.

## The render cache

A re-run of `up.nu` renders the whole target again, and nearly all of that time is Dhall evaluating, and agents being asked
what they take. A rendered manifest is a pure function of its inputs, so one rendered before from the same inputs is read
back instead of rendered again. A resume that changed nothing takes a fraction of the time; a change to one workload
re-renders that workload and nothing else.

### What is cached, and under what key

Three things, all in `cache.nu`'s store under `~/.cache/polar/` (`POLAR_CACHE_DIR` moves it; `XDG_CACHE_HOME` is honoured):

- **the catalog's manifests** (`render/`): `render-catalog.nu` keeps the output of each `dhall-to-yaml` call (two per
  workload);
- **the hand-written charts' manifests** (`chart/`): `dhall-render.nu` keeps the output of each of their 19 `dhall-to-yaml`
  calls. The key is the text rendered, a digest of every Dhall file under `infra/` (charts import each other, the schema
  and the target's overrides by path, so none of them is in the text), the value of every environment variable a Dhall file
  imports (`POLAR_NODE_REGISTRY`, which `render.nu` hands them), the tool's version and the flags;
- **an agent's scaffold** (`scaffold/`): what its image prints for `--scaffold`, under the image's reference and the digest the
  registry serves for it (`digest.nu`, a manifest HEAD request, about ten milliseconds). A rebuilt image is another key; with no
  digest to key on nothing is kept. Before this, every resolve of a workload started its container.

The catalog's key is a hash of:

- the **typed spec**: the workload's resolved record as `json-to-dhall` turns it into Dhall, with the template it is
  applied to. A different value anywhere in the workload (an environment variable, a pull policy, a shutdown budget) is
  different text, so a different key;
- **every file in `infra/schema/`**, hashed together. This is deliberately more than the templates import: any edit
  to the schema, even a comment, is a different key for every workload, and nothing is left to judge by hand;
- the **version of `dhall-to-yaml`**.

The remote Dhall imports (the Prelude, the Kubernetes bindings) are pinned by `sha256:` hash in the schema files, so
they cannot differ under the same key. The parts of the key are length-prefixed, so two lists of inputs cannot run
together into one.

### Why it cannot serve a stale manifest

The cache stores only successes; a failed render reaches the caller and nothing is written. A file is written under a
name of its own and moved into place, so a reader never sees half of one. `infra/tests/render-determinism.nu --with-cache` renders a
target with the cache off, cold and warm and fails unless the output is byte-identical, and it covers all three caches (checked
2026-10-19, on `local-minimal`). The chart cache was also checked against the commit before it: the same render, in a throwaway
worktree, byte for byte. What was checked on the 16 workloads of `local-minimal` for the catalog's cache, on 2026-10-14:

- rendered with the cache off, cold (empty) and warm, the 32 output files are **byte-identical**;
- with one workload's pull policy changed, **one** new cache entry was made and **one** output file differed; the
  other fifteen were read back;
- after a schema file was edited, **all 32** entries were rendered again (the output was unchanged, as it must be for
  a comment).

`infra/tests/cache.nu` covers the store itself (a stored result is used, a failure is not stored, the key changes with
every part, the directory digest changes when a file is edited, added, removed or renamed, `POLAR_CACHE=off` stores
nothing). One property is not tested: that the move into place is atomic, which only matters if a process dies
mid-write.

### What it saves

**Before the chart and scaffold caches.** Measured with `infra/measure-render-cache.nu` on 2026-10-14, on a machine with 32 logical CPUs,
with the cluster running and the machine otherwise idle. "Cold" is a fresh, empty cache directory for every run; "off" is
`POLAR_CACHE=off`. Dhall's own import cache (`~/.cache/dhall`) was left as it was in every case, so it is not part
of these numbers.

| case | runs | mean | min to max | against cache off |
| --- | --- | --- | --- | --- |
| catalog render, cache off | 3 | 55.2 s | 54.9 to 55.3 s | |
| catalog render, cold cache | 3 | 54.9 s | 54.8 to 55.2 s | 0.3 s less: a miss costs nothing measurable |
| **catalog render, warm cache** | 3 | **8.2 s** | 8.2 to 8.2 s | **47.0 s less (85%), 6.7 times faster** |
| catalog render, warm, one workload changed | 3 | 11.2 s | 11.2 to 11.3 s | 44.0 s less (80%), 4.9 times faster |
| whole render (`render.nu`), cache off | 2 | 67.6 s | 67.3 to 67.8 s | |
| **whole render (`render.nu`), warm cache** | 3 | **20.8 s** | 20.7 to 20.8 s | **46.8 s less (69%), 3.3 times faster** |
| `up.nu` resume that changes nothing, cache off | 2 | 74.7 s | 74.2 to 75.1 s | |
| **`up.nu` resume that changes nothing, warm cache** | 3 | **28.0 s** | 27.9 to 28.0 s | **46.7 s less (63%), 2.7 times faster** |

The cache holds 32 entries (16 workloads, two each) and takes 258 kB.

Per workload that is about 3.4 s uncached, about 0.5 s cached, and about 3.0 s more for each workload whose input
changed. The saving is the same in absolute terms at each level, about 47 s, because it is the same work being
skipped; it is a smaller share of the larger totals.

**Since.** Not measured with the harness (so no minimum and maximum), on the same machine, rendering `sandbox-local`, whose catalog has 15
workloads. A warm render took 59 to 62 s with only the catalog cached. The scaffold cache took a catalog file from about 2.1 s to 0.35 s. The
chart cache then brought a warm render to **12 s**, and one with the chart cache cold to 23 s. A render with no cache at all is still over
a minute, because every Dhall render has to run once; a tester's first render is that one.

### Where a warm resume still spent its time, and what was done

The 2026-10-14 breakdown of a 28.0 s resume was about 8.2 s of catalog (resolving each workload twice, and reading the cache), about 12.6 s of
the eight custom charts, which were not cached, and about 7.2 s of everything else `up.nu` does. It pointed at the charts and at resolving, and
a note here said to measure before building a cache of the scaffold. Measured, a resolve that had to ask the image for its scaffold took
2.45 s against 0.63 s from the cache, and a render did it a few dozen times. The charts and the scaffolds are now cached; resolving itself is not. What is left of the render includes
`resolve.nu` run twice per workload in separate processes (0.69 s a call, of which 0.22 s is loading the script), `json-to-dhall` (about
0.3 s a file), and pinning the images against the registry; how much each of those costs now has not been measured.

### Using it

```fish
POLAR_CACHE=off nu infra/render.nu local-minimal      # render everything, store nothing
POLAR_CACHE_DIR=/tmp/c nu infra/render.nu local-minimal   # use another directory
rm -rf ~/.cache/polar                                 # forget it all; nothing depends on it
nu infra/measure-render-cache.nu results.md           # reproduce the numbers above (about 16 minutes)
nu infra/measure-render-cache.nu results.md --user U --email E    # and the up.nu cases, which need a cluster
```

Leave the machine alone while the measurement runs, or the timings move.

---

## Secrets

Secrets are encrypted per target and are not rendered by the chart pipeline.

- `targets/<t>/secrets.sops.yaml` holds the values, encrypted with sops (age, with SSH keys as the identity). It is encrypted to every key
  listed in the target's `.sops.yaml`. To give someone access, add their public key to that file's `age:` value and run
  `sops --config targets/<t>/.sops.yaml updatekeys targets/<t>/secrets.sops.yaml`, then commit the re-encrypted file. Two things about
  `.sops.yaml` are easy to get wrong: the keys must be **comma-separated** (`key1,` newline `key2,`, since sops reads a space-separated list
  as one recipient and silently encrypts to the first key only), and it is gitignored and only needed to *encrypt*: a file knows its own
  recipients, so decrypting needs only your private key (`SOPS_AGE_SSH_PRIVATE_KEY_FILE`). A fresh checkout can decrypt a file it was added to.
  Someone with no access can write a file of their own for a local target: `neo4j_auth` (`neo4j/<password>`) and `graph_password` (the same
  password) are the two the cluster cannot run without (the README's "Secrets setup").
- `targets/<t>/secrets.nu` decrypts them, creates the Secrets in the cluster and updates those the files changed, so re-running is safe. `up.nu` runs it in the "secrets and infrastructure" step.
  Each Secret is one of two kinds (`infra/secrets-lib.nu`, tests in `infra/tests/secrets.nu`):
  - **From the SOPS files** (`kubectl_sops_secret`): created when missing; when it exists, compared with what the files give and
    left alone (`unchanged`) or its data replaced, its type, labels and annotations kept. An update names the workloads to restart,
    since a pod reads a Secret when it starts: `secret/jira-secret in polar: updated from the sops files (restart the pods that read it:
    kubectl -n polar rollout restart deployment/jira-observer)`. When the files have no value (`token=placeholder`, `oci-registry-auth={}`)
    the placeholder is made only if the Secret is missing, and never replaces a value the cluster holds, which may have been set by hand.
    Neo4j takes its password from `neo4j-secret` on its first start only, so a changed `neo4j_auth` updates the Secret and not the
    database's password (`up.nu`'s Neo4j check says when they differ).
  - **Made here** (`kubectl_generated_secret`, the random `cassini-shutdown-token`): created once, never compared, never made again.
  A value stored base64-encoded would reach the workload still encoded, since Kubernetes encodes Secret values itself: the step warns
  when one looks so (it decodes to 16 or more printable characters with no whitespace), naming the key and the file, never the value.
- Manifests only *reference* secrets by name (`secretKeyRef`); they never contain secret values. A catalog workload's env entry does the same: `{ name: "JIRA_TOKEN", secret: "jira-secret", key: "token" }`.
- Never `git add` certificates or keys.

(The `infra/secrets/` directory tree that the first version of this pipeline
described is empty and unused; secrets are per target now.)

---

## Recipes

### Add an agent to the catalog

Prerequisite: the agent's image builds from the flake. If it is not on
`polar-agent-builder` yet, migrate it first. The builder declares what the code
needs (`.with_graph_write()`, `.requires(...)`), and your `needs` must provide
at least that or the agent refuses to start (see
[Capabilities and the Rust agent builder](#capabilities-and-the-rust-agent-builder)).

1. **Add the image row** to `infra/images.nuon` (one row; nothing else needs to know):
   ```nuon
   {image: "foo-observer", tag: "latest", kind: "nix", attr: "polarPkgs.fooAgent.observerImage"}
   ```
2. **Add the workload** to a file under `infra/config/workloads/` (all files there are read):
   ```nuon
   {
     name: "foo-observer"
     kind: "agent"
     image: "foo-observer"          # a row of images.nuon; its tag is used
     needs: ["graph-write"]         # on top of what the kind gives: cassini
     env: [
       { name: "FOO_URL",   value: "https://foo.example.com" }
       { name: "FOO_TOKEN", secret: "foo-secret", key: "token" }
     ]
   }
   ```
   Any field `kinds.nuon` defines can be overridden on the entry (`backoff_limit`,
   `probe`, `namespace`, `service_account`, ...).
3. **Secrets**: a `secret:` reference must exist in the cluster; add it to the
   target's `secrets.sops.yaml` and `secrets.nu`.
4. **Target-specific values** (a URL that differs per environment) go in
   `targets/<t>/config.nuon` under `workloads.foo-observer`, not in the entry.
5. **Check and look**:
   ```fish
   nu infra/resolve.nu check
   nu infra/resolve.nu explain foo-observer
   ```
6. **Apply order**: add `foo-observer.yaml` to the right layer of `apply_layers` in the
   target's `target.nu` (or to `apply_order` in a target without layers), and
   `foo-observer-resources.yaml` *before* it if the workload has resources. This list is
   still hand-maintained, and `render.nu` refuses a file that is in none.
7. **Render, compare, bring up** (see [Verifying a change](#verifying-a-change)).

The ServiceAccount is automatic: the workload runs as `service_account` (default:
its own name), and `render.nu` emits the ServiceAccount object for it.

### Add a capability

A capability is something several workloads can need: a service connection, a
permission, a credential.

1. Add an entry to `infra/config/capabilities.nuon` that **answers every facet** in
   `facets.nuon`. Use `{}` where it has nothing to add. The file's header documents
   each facet's shape.
2. Text values can use `{<service>.dns}`, `{<service>.port}`, `{<service>.addr}`
   and `{workload}`.
3. If it connects to a service that is not in `services.nuon`, add the service first.
4. Use it: `needs: ["your-capability"]`.
5. `nu infra/resolve.nu check` fails if a facet is unanswered, an unknown facet is
   answered, a dependency names a service that does not exist, or a placeholder
   cannot be resolved. You cannot forget a case.

Name it for what the code *requires*, and add the same name to the Rust
`Capability` enum in `polar-agent-builder` (`src/agents/polar-agent-builder/src/capability.rs`). A unit test
(`names_match_the_deployment_catalog`) compares the two name sets in both
directions, so a rename on one side fails the other side's tests.

### Capabilities and the Rust agent builder

One vocabulary, two enforcement points:

| Where | When | What it does |
|---|---|---|
| `resolve.nu check` | render time | an unknown `needs` entry, or a capability that does not answer every facet, fails |
| `AgentBuilder::build()` | agent startup | the agent's declared requirements must be a subset of `POLAR_CAPABILITIES`, else it exits non-zero (its pod restarts in place) |

The same shape applies to time. The agent's code declares the least it needs to drain
(`.with_shutdown_budget(..)`, a requirement, not a preference) and the catalog says how
much the deployment provides (`shutdown.budget_secs`):

| Where | When | What it does |
|---|---|---|
| `resolve.nu check`, running the agent's `--check` | render time | the configuration document each workload will be given is judged by the agent itself (`<agent> --check --config FILE`, in its built image under `up.nu`), the same function the agent runs when it starts: every variable the agent reads must parse, and what must hold between them must hold (room to rejuvenate before the certificate expires, the probe's stale limit above twice the tick, ...). Each problem names the workload, the variable and what it is for |
| `resolve.nu check`, running `infra/rules.nu` | render time | what only the deployment knows: the pod's termination grace period must exceed the shutdown budget |
| `AgentBuilder::build()` | agent startup | the whole environment is checked first (the same function), and `POLAR_SHUTDOWN_BUDGET_SECS` must be at least what the code declared, else it exits non-zero naming both numbers. The deployment may give more, never less |

So raising a number is a catalog change. Lowering what the code needs is a change to the
code, which someone has to review.

**Whoever owns a fact owns the rule about it.** What the agent's code needs of its
configuration is the code's, so those rules are Rust and are not repeated in Nushell:
`resolve.nu check` runs the agent's own `--check --config FILE` on the document it rendered,
which is the function the agent runs when it starts. Change a rule or a constant in the Rust and
the render check changes with it, with no second copy to update. What only the deployment knows
(the grace period against the budget) is here, in `infra/rules.nu`.

Each variable is declared once, with `polar-env`: name, type, default, what it is for, and
whether it is secret. The check, the agent's own reads and the documentation all come from
that declaration. `<agent> --scaffold` lists every variable the agent reads, by section.

`resolve.nu check` asks each agent from its image, as a pod on the cluster (`infra/agent-pods.nu`;
it needs KUBECONFIG and the images in the registry). An agent that cannot be run, or does not answer,
is an error, never a skipped check. (There used to be
a separate program, `polar-config-check`, that judged an environment; it is retired, and a catalog
workload is an agent that describes itself.)

Run the catalog's tests with `nu infra/tests/rules.nu` and `nu infra/tests/resolve.nu`; the agent's own checks are tested by `cargo test -p polar-agent-builder`.

In Rust an agent declares what its own code needs:

```rust
AgentBuilder::new(myself.clone(), TCP_CLIENT_NAME)
    .with_graph_write()            // requires Capability::GraphWrite; health also watches the graph
    .with_encryption()             // requires Capability::Encryption; spawns the CryptoActor
    .requires(Capability::KubeApiRead)   // a requirement the builder itself does nothing for
```

Every builder agent implicitly requires `cassini`. The deployment states what it
provides by setting `POLAR_CAPABILITIES` (derived from `needs`, never written by
hand). If the variable is unset, for example when running outside the cluster,
the builder logs a warning and starts anyway; it only refuses on a real mismatch.

`encryption` is **opt-in**: an agent that never encrypts a payload (the observers)
does not declare it. Its one footprint is in init: `POLAR_INIT_ESCROW_BOOTSTRAP=true`,
so the custody daemon's custodian fetches the workload's escrow keypair, which the
daemon keeps and only the agent's crypto actor uses (through the daemon, since
2026-10-28; `docs/architecture/key-custody.md`). It needs the daemon: set without a
certificate of the daemon's, polar-init stops. No other workload runs the bootstrap: the credentials agent serves it and the
broker carries it, and neither needs encryption (decided 2026-10-22,
`docs/architecture/policy-enforcement.md`, "Decisions").

Health configuration is no longer passed from Rust: the builder derives
`expects_graph` from the capabilities and reads the dependency endpoints from
`POLAR_HEALTH_DEP_ENDPOINTS`, which the catalog derives from the same `needs`.

### Add a service

One entry in `infra/config/services.nuon`:

```nuon
foo: { dns_name: "foo-svc", namespace: "polar", port: 9000 }
```

`namespace` is a key of `cluster.nuon`'s `namespaces`, not a literal name. The
address is `<dns_name>.<namespace>.svc.<cluster.domain>:<port>`, so changing the
cluster domain in one place changes every address. The entry does not deploy
anything; it tells other workloads where to find the service. Deploying it is a
custom chart until services are in the catalog.

### Add an image

One row in `infra/images.nuon`. Three kinds:

| kind | for | columns |
|---|---|---|
| `nix` | built from this repo's flake (`.#packages.<platform>.<attr>`) | `attr` |
| `mirror` | a third-party image copied into the local registry | `upstream` |
| `external` | built outside this repo, an existing docker-archive | `path` (`~` allowed), `env` (variable that overrides it) |

The rendered manifests decide which images a target needs; `images.nu` joins their
`image:` fields against this table. A manifest image with no row, or a tag that
disagrees with the row, fails before anything is built.

### Change a value for one target

Use `targets/<t>/config.nuon` (see [How a target works](#how-a-target-works)).
Scalars override; `needs` and `env` entries are appended.

### Add a custom chart (the escape hatch)

Use this when a workload does not fit a catalog kind: a StatefulSet, a Deployment
with a PVC, anything with more structure than an agent's Deployment. This is what the
existing services are today.

1. Pick the layer by dependency and create `infra/layers/<layer>/<name>/` with a
   Dhall file per concern, a `values.dhall`, and a `main.nu` (merge the incoming
   overrides with `values.dhall`, run `dhall-to-yaml` per file, write the output).
2. Add `main.nu` to `charts` in each target that needs it, and its output files to
   `apply_order`.
3. If the chart needs cluster facts, they come through `render.nu`'s context. Today
   that is a `match` on the chart name; replacing it with data is on the list.
4. Per-deployment values go in `overrides.dhall`.

Keep a custom chart's Dhall simple. When two charts start repeating the same
skeleton, promote the skeleton to a kind.

### Add a kind

A kind is a template plus defaults for a family of workloads that share a shape
(today: `agent`).

1. Defaults in `kinds.nuon`.
2. A template `schema/<kind>.dhall` taking a typed record, and its type in
   `schema/<kind>-spec.dhall`. Use only Text, Natural, Bool, Optional and List
   (Dhall cannot compare Text, and the JSON encoding of unions is fragile); read
   "which variant is this" off which `Optional` field is set.
3. Register it in `TEMPLATES` in `render-catalog.nu`.
4. The resolver builds the agent-shaped record today; a kind with a different shape
   needs the resolver to produce its own record.

### Add a facet

Rare. A facet is a new operation every capability must answer.

1. List it in `facets.nuon` with a one-line description.
2. Give every existing capability an answer (`check` lists the missing ones).
3. Teach `resolve.nu` to collect it and a template to use it.

### Add a target

1. Copy `infra/targets/local-minimal/`.
2. `target.nu`: the charts it deploys, `apply_order`, and the values still passed
   through it.
3. `overrides.dhall`: values for the custom charts.
4. `config.nuon`: the `cluster` facts that differ (`registry`, `domain`) and
   per-workload overrides.
5. Secrets: `.sops.yaml` with your keys, `secrets.sops.yaml`, `secrets.nu`.
6. `nu infra/resolve.nu check --target <t>`, then `nu infra/render.nu <t>`.

You do not modify any chart, schema file or `render.nu` to add a target.

---

## Verifying a change

Rendered manifests are the contract. For a change that should not alter what is
deployed (a refactor, a migration), prove it. For one that should, check that only
the intended differences appear.

```fish
# 1. Keep a render from before your change (untracked; add manifests.old/ to .git/info/exclude once)
nu infra/render.nu local-minimal
rm -rf manifests.old; cp -r manifests manifests.old

# 2. Make your change, then render again
nu infra/render.nu local-minimal

# 3. Compare by meaning, not by text
nu infra/compare-manifests.nu manifests.old manifests kube      # names after the dirs filter by Kind/namespace/name
```

`compare-manifests.nu` keys every document by Kind, namespace and name, so a
document that moved between files still matches. Lists of named things (env,
volumes, mounts, containers) are compared in name order. It prints `same`, or one
line per difference, and exits non-zero if there are any.

For a quick check with no render, `nu infra/resolve.nu diff <workload> --manifests
manifests.old` compares what the resolver says with a rendered manifest, and
`nu infra/resolve.nu resources <workload>` prints the objects it will emit
alongside the workload.

Then bring it up on a cluster and look at the pods, not just the apply output.

---

## Reading errors

| You see | It means |
|---|---|
| `unknown need 'x'` | `x` is not in `capabilities.nuon` |
| `capability 'x' does not answer facet(s): ...` | a facet in `facets.nuon` is missing from the capability; add `{}` or the real answer |
| `capability 'x' answers unknown facet(s)` | a typo, or a facet not listed in `facets.nuon` |
| `dependency names service 'x', which is not in services.nuon` | add the service, or fix the name |
| `unresolved placeholder in '...'` | a `{svc.dns}` names a service `services.nuon` does not have |
| `image 'x' has no row in infra/images.nuon` | add the row |
| `identity a.b is shared by: ...` | two workloads share a ServiceAccount; give one its own `service_account` |
| `the resolved record does not match agent-spec.dhall` | resolver and template disagree about a field; change both together |
| `no template for kind 'x'` | register the kind in `TEMPLATES` in `render-catalog.nu` |
| `json-to-dhall` / `dhall-to-yaml` not on PATH | the `dhall-json` package is missing from the shell (it comes from nix) |

---

## Rules of the road

- **Put each change in the right tier.** A new fact is nuon. A new shape is Dhall. A new step is nushell.
- **Variation is data.** No per-agent booleans in templates.
- **Capabilities answer every facet.** `check` enforces it; keep it true.
- **Compare before and after.** Rendered output is the contract.
- **Keep Dhall simple.**
- **Never `git add` certificates or keys.**
- **Do not change product code to suit the dev environment.**

nushell gotchas that have bitten this pipeline:

- `env` is a reserved variable name.
- Closures cannot capture a `mut` variable; return a record or list and collect.
- Annotate mutable lists: `mut xs: list<any> = []`. Newer nushell infers the element type from the first value and rejects a later append of a different shape.
- Do not shadow builtins (`table`, `select`).
- `do -i ... | complete` does not catch a missing command; use `try`.
- `input` needs a TTY.

---

## The old pod is gone before the new one starts

*Since 2026-10-28 every agent is a Deployment of one that restarts in place (`docs/architecture/deployments.md`, step 4):* an
ending, planned or not, restarts the same pod, so there is nothing to replace. A pod that is really replaced (deleted,
evicted, lost with its node, or a changed Deployment rolled out) is replaced by `strategy: Recreate`, which ends the old pod
before it makes the new one. A rolling update would start the new one first: two instances of one identity at once, with the
same Cassini identity (and the credentials agent's ledger on a `ReadWriteOnce` volume, which still lets two pods on one node
mount it). `render.nu` refuses a Deployment with `polar-init` that lacks `Recreate`, one replica or the restart rule
(`in_place_problems` in `rules.nu`).

A Job, the `run` kind's (the load harness) and a custom chart's, sets `podReplacementPolicy: Failed` (`JobPodReplacementPolicy`
in `schema/constants.dhall`) for the same reason: the Kubernetes default, `TerminatingOrFailed`, creates the replacement pod
the moment the old one starts terminating. `render.nu` refuses a Job without it (`job_replacement_problems`).

**The risk it brings: a pod stuck terminating.** If a pod's node dies or becomes unreachable, the
kubelet cannot confirm the pod stopped, and the pod stays `Terminating`. `Recreate` (and a Job's `Failed`) then
create no replacement until the pod is gone, where a rolling update (or the default) would have started one. For a
single-instance agent that is the correct failure (the alternative is a second instance running
beside one that may still be alive), but it is an outage until someone acts.

- A healthy node with a slow pod is not this case: the kubelet kills the container when
  `terminationGracePeriodSeconds` ends, the pod goes `Failed`, and the replacement starts. That
  period is derived from the shutdown budget, so the overlap-free wait is bounded.
- For a dead node there is no field on the workload or the pod that force-deletes the pod; the manifest can
  only tune how soon it is marked for deletion (`tolerationSeconds` on the `unreachable` and
  `not-ready` tolerations). The fix is an operator action once the node is known to be down: taint
  the node `node.kubernetes.io/out-of-service` (NoExecute), which tells the control plane to force
  delete the pods on it, or delete the Node object. `kubectl delete pod --force --grace-period=0`
  also works, but only do it when the node is confirmed down, since it removes the pod object
  without any evidence the process stopped. Check the Kubernetes documentation for your version
  before relying on the exact taint form.
- On the single-node dev cluster (u7s) a dead node means no cluster, and this does not arise; on a multi-node one (u7c) it can.

## Known issues and limits

- **Access to the outside is checked once, at the end of `up.nu`, and only reported.** A pod runs `git ls-remote` against one public repository (`POLAR_EGRESS_PROBE_URL` changes it) and `up.nu` says whether the name resolved and the host answered. It is a note because a cluster used offline works. The probe runs in the `polar-scheduler-observer` pod, the one image with a git CLI, so if that pod is not running the check says it was skipped. On a cluster whose CoreDNS forwards to a resolver that pods cannot reach, pods resolve nothing outside the cluster, and this check says so.
- **Service-account token ordering.** On a fresh cluster, a `kubernetes.io/service-account-token` Secret is populated by the token controller only after its ServiceAccount exists, so the first apply may need repeating. `serviceaccounts.yaml` precedes `kube-observer-resources.yaml` in `apply_order`, which should avoid it, but this has only been exercised on an existing cluster.
- **The catalog has two kinds, `agent` and `service`.** All the agents are in it; the custom charts are ace, cert-issuer, jaeger, keycloak, neo4j and postgres.
- **`apply_order` (and the layers it comes from) is hand-maintained** in each `target.nu`; `local-minimal` and `sandbox-local` have layers, `local` does not. It should be derived from the catalog (resources before the Job).
- **`render.nu` passes cluster facts to custom charts through a `match` on the chart name,** and `target.nu` carries values (`cert_issuer_url`, `polar_init_image`, ...) that the catalog derives elsewhere. That is data written as control flow, and a duplicated fact.
- **Only the kube agents declare capabilities in Rust.** Other agents still hard-code health arguments and Cassini/Neo4j endpoints; they move to the builder one pair at a time.
- **`POLAR_CAPABILITIES` unset is a warning, not an error,** so an agent run outside the cluster still starts. A deployment that forgets to set it is therefore not caught by the builder; the catalog sets it for every workload it renders.
- **The cluster domain drives catalog addresses,** but custom charts still hard-code `svc.cluster.local`, and `polar-init --cluster-domain` is not passed.
- **`src/deploy/`** is the old system. Confirm it is dead and remove it, or mark it.
- **Resolving the workloads is not cached.** A render runs `resolve` twice per workload in separate processes (0.69 s a call, 0.22 s
  of it loading the script). Only the scaffold inside it, the hand-written charts and the catalog's Dhall are cached; see "The render cache".
- **A first render is slow.** With nothing cached every Dhall render has to run (over a minute for `sandbox-local`); only repeats are fast.
- **Pulling `Always` is throttled by the kubelet** (5 requests a second, burst 10) on a cluster that has not lifted `registryPullQPS`: a
  burst of pods is retried with a backoff, which slows a bring-up but does not break it. The `u7s` node lifts it.
- **Third-party images under a version tag (Jaeger, Keycloak, Postgres, the Flux controllers, local-path-provisioner) are still
  `IfNotPresent`. An image loaded into a node by hand, and not in the registry, does not work with `Always` for ours: push it
  (`just image <name>`; `just cluster-load` is now its alias).
- **The gitops target has run on one kind of cluster** (the podman dev node), most recently (2026-10-19) on a fresh one with the registry
  and the deployment git server both in the cluster. The server does not yet reject an unsigned push (`flux.nu install` checks the
  signature; the server does not).

Open items are tracked in `remaining-work.md`.
