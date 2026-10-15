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
nix-usernetes cluster for development, a sandbox AKS environment, an emerging
`ace` target, and more to come. Each shares a large amount of structural intent
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

# Only render (never touches a cluster); output goes to manifests/ (gitignored)
nu infra/render.nu local-minimal

# Render without the cache, to see the true cost or to rule it out (see "The render cache")
POLAR_CACHE=off nu infra/render.nu local-minimal

# What does the catalog say about a workload, and where did each value come from?
nu infra/resolve.nu explain kube-consumer

# Is the whole catalog consistent? (no cluster, no render)
nu infra/resolve.nu check

# Images
nu infra/images.nu list                     # every image the repo knows how to produce
just images-build kube-observer             # stage 1: build only (needs nix; no registry, no cluster)
just images-push kube-observer              # stage 2: build and push to the registry (no cluster)
just images-mirror                          # stage 3: copy third-party images into the registry
just image kube-observer                    # every stage, then drop the node's stale cached copy
just images-all                             # every stage for everything in the table
```

The `build`, `push` and `mirror` recipes take no names to mean every image. A
failing image does not stop the stage: the rest are attempted and the stage ends
with a summary and a non-zero exit. The node cache step of `image` and
`images-all` is skipped when no u7s node exists.

`up.nu` prints its plan first and skips what the target does not need. Its steps:
preflight, render, images (`--dev`), namespaces, cluster CA, secrets and
infrastructure, waiting for core services, port-forwards, admin flow, browser
certificate and Neo4j password. It stops for a human only where a human is
required, and looks first, read-only, so it stops only for what is missing: the browser step when the
Librewolf profile does not hold this cluster's CA and the user's current certificate (closing the browser
and importing it), and the Neo4j password step only when Neo4j is still on its factory password. With the
`nix-neo4j` image that applies `NEO4J_AUTH` on its first start (the one this repository deploys), Neo4j starts
with the password the agents use and there is no password step at all.

---

## Directory structure

```
infra/
  up.nu                  brings a cluster up from a target (applies)
  render.nu              renders a target (never applies)
  resolve.nu             the resolver: explain, json, resources, diff, check
  render-catalog.nu      renders every catalog workload (through the render cache)
  cache.nu               the render cache: where it lives, how a key is made, read or compute
  compare-manifests.nu   compares two renders by meaning
  images.nu  images.nuon which images exist, and building/pushing them
  validate.nu            environment validation helpers
  apply.nu               the decisions up.nu makes while applying: which Jobs to restart and why
  retry.nu               try again for a while, and stop at once on a failure that cannot be fixed
  forwards.nu            wait for a port-forward to answer; restart it if it does not
  admin.nu               what to do with ~/.config/polar-admin that belongs to another cluster
  browser.nu             step 10: does the browser hold this cluster's CA and the user's certificate, and has
                         Neo4j's password been changed (read-only)
  resolve.nu scaffolds   writes every agent's `--scaffold` to a folder (`~/.config/polar-admin/scaffolds`), which
                         `polar-sign template` and `polar-sign sign` read
  egress.nu              the check at the end of up.nu: can a pod reach a host outside the cluster, and if not, why
                         (a note, never a failure)
  config/                the catalog
    cluster.nuon         domain, namespaces, registry, token audience
    services.nuon        in-cluster services other things connect to
    kinds.nuon           defaults shared by every workload of a kind
    capabilities.nuon    what each `needs` entry contributes
    facets.nuon          the operations every capability must answer
    workloads/*.nuon     one entry per catalog workload
  schema/                Dhall vocabulary
    agent.dhall  agent-spec.dhall          the Job template and its input type
    resources.dhall  resources-spec.dhall  RBAC and token Secrets alongside a workload
    functions.dhall  constants.dhall ...   shared helpers and static constants
  layers/                custom charts, by dependency
    1-platform/          must exist before everything else
      infra/             namespaces, storage
      services/          cert-manager, flux, networking (targets opt in)
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
  `services/` holds platform services (cert-manager, Flux, Istio networking). Not
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
        ▼   schema/agent.dhall  the one Job template for agents
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

Today's capabilities are `cassini`, `graph-write`, `encryption`,
`kube-api-read`, and others that describe the shape of the pod rather than what
the code needs: `persistent-data` (a claim named for the workload, mounted at
`/var/lib/<workload>`), `pinned-ca` (polar-init and the container trust the Polar CA
from a mounted file) and `escrow-exempt` (polar-init skips the escrow
bootstrap, because this workload serves it or is what the request travels through), `tracing` (every workload sends its traces to Jaeger, from its kind) and `oci-registry-auth` (registry credentials mounted as a Docker config). An agent on a document says what its
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

**`overrides.dhall`** is a flat record merged over each custom chart's
`values.dhall`. It holds only what differs from the defaults for this target:
image references, pull secrets, storage classes, hostnames, URLs.

**`config.nuon`** is the catalog's override layer for this target:

```nuon
{
  cluster: { registry: "registry.example.com" }     # any field of cluster.nuon
  workloads: {
    "jira-observer": { backoff_limit: 5 }           # any field of a workload entry
    "oci-registry-resolver": { enabled: false }     # leave a catalog workload out of this target
  }
}
```

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

- **`agent.dhall`, `agent-spec.dhall`**: the agent Job template and the record type it takes.
- **`resources.dhall`, `resources-spec.dhall`**: the objects that live alongside a workload (ClusterRole, binding, token Secret).
- **`kubernetes.dhall`**: Dhall bindings for Kubernetes types.
- **`functions.dhall`**: shared helpers (`makeDeployment`, `makePolarInitContainer`, the proxy cert helpers, ...).
- **`agents.dhall`, `cassini.dhall`, `storage-class.dhall`, `identity-strategy.dhall`, `ca-provisioning.dhall`**: type definitions.
- **`constants.dhall`**: static values: namespace names, service names, TLS paths, ports, labels. Its header says it holds no constructed resources and no environment-dependent values, but it still contains a few (`certEmptyDirVolume`, `saTokenVolume`, `commonClientEnv`); they are on the list to move out.

## The render cache

A re-run of `up.nu` renders the whole catalog again, and nearly all of that time is Dhall evaluating two templates per
workload (the pod, and the objects beside it). A rendered manifest is a pure function of its inputs, so one rendered
before from the same inputs is read back instead of rendered again. A resume that changed nothing takes a fraction of
the time; a change to one workload re-renders that workload and nothing else.

### What is cached, and under what key

`render-catalog.nu` keeps the output of each `dhall-to-yaml` call (two per workload) in `cache.nu`'s store, under
`~/.cache/polar/render/` (`POLAR_CACHE_DIR` moves it; `XDG_CACHE_HOME` is honoured). The key is a hash of:

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
name of its own and moved into place, so a reader never sees half of one. What was checked on the 16 workloads of
`local-minimal`:

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

Measured with `infra/measure-render-cache.nu` on 2026-10-14, on a machine with 32 logical CPUs, with the cluster
running and the machine otherwise idle. "Cold" is a fresh, empty cache directory for every run; "off" is
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

### Where a warm resume still spends its time

By subtraction, not measured separately: of the 28.0 s,

- about **8.2 s** is the catalog render that is left (resolving each workload twice, about a quarter of a second
  each, and reading the cache);
- about **12.6 s** is the eight custom charts, which are not cached;
- about **7.2 s** is everything else `up.nu` does (the applies and their comparisons with the cluster, the checks,
  the waits).

So the next saving is in the charts and in resolving, about 21 s between them, and neither is cached yet. A cache
of the per-image scaffold would address a slice of the resolving only; measure before building it.

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

- `targets/<t>/secrets.sops.yaml` holds the values, encrypted with sops (age). Add your key to the target's `.sops.yaml` and run `sops updatekeys` to give someone access.
- `targets/<t>/secrets.nu` decrypts them and creates the Secrets and ConfigMaps in the cluster. It creates what is missing and skips what already exists, so re-running is safe. `up.nu` runs it in the "secrets and infrastructure" step.
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
6. **Apply order**: add `foo-observer.yaml` to `apply_order` in the target's
   `target.nu`, and `foo-observer-resources.yaml` *before* it if the workload has
   resources. This list is still hand-maintained.
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
`Capability` enum in `polar-agent-builder` (`src/capability.rs`). A unit test
(`names_match_the_deployment_catalog`) compares the two name sets in both
directions, so a rename on one side fails the other side's tests.

### Capabilities and the Rust agent builder

One vocabulary, two enforcement points:

| Where | When | What it does |
|---|---|---|
| `resolve.nu check` | render time | an unknown `needs` entry, or a capability that does not answer every facet, fails |
| `AgentBuilder::build()` | agent startup | the agent's declared requirements must be a subset of `POLAR_CAPABILITIES`, else it exits non-zero (a Job restarts) |

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

`resolve.nu check` asks the agent from its image when `POLAR_AGENT_SOURCE=images` (what `up.nu`
sets), and otherwise runs it with `cargo run`, which rebuilds it when the code has changed. An
agent that cannot be run, or does not answer, is an error, never a skipped check. (There used to be
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
does not declare it. It currently contributes nothing to the manifest (all facets
are `{}`), because polar-init's escrow bootstrap for polar-crypto runs for every
agent. When the builder's mTLS integration through the CryptoActor lands, decide
whether encryption becomes a baseline or stays derived from a capability.

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
with a PVC, anything with more structure than an agent Job. This is what the
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

## Jobs wait for the old pod to be gone

Every Job sets `podReplacementPolicy: Failed` (`JobPodReplacementPolicy` in
`schema/constants.dhall`, used by `agent.dhall` and every custom chart). The Kubernetes default,
`TerminatingOrFailed`, creates the replacement pod the moment the old one starts terminating, so a
pod that is deleted or evicted overlaps with its replacement for the whole drain, up to its
`terminationGracePeriodSeconds`: two instances of one identity at once, with the same volumes and the same
Cassini identity (the credentials agent's ledger is on a `ReadWriteOnce` volume, which still lets two pods
on one node mount it). Their kernel keys do not collide: each pod loads its own key under its own number. With `Failed` the controller waits until the old pod is fully gone.

This costs nothing in the normal case. Every agent ends a pod by exiting non-zero, whether it chose to
(rejuvenation, a health verdict) or was told to (SIGTERM runs the drain and then exits 1), so a pod
always finishes `Failed` and the Job replaces it as before. `render.nu` refuses a render in which a
Job does not carry the policy (`job_replacement_problems` in `rules.nu`).

**The risk it brings: a pod stuck terminating.** If a pod's node dies or becomes unreachable, the
kubelet cannot confirm the pod stopped, and the pod stays `Terminating`. With `Failed` the Job then
creates no replacement until the pod is gone, where the default would have started one. For a
single-instance agent that is the correct failure (the alternative is a second instance running
beside one that may still be alive), but it is an outage until someone acts.

- A healthy node with a slow pod is not this case: the kubelet kills the container when
  `terminationGracePeriodSeconds` ends, the pod goes `Failed`, and the replacement starts. That
  period is derived from the shutdown budget, so the overlap-free wait is bounded.
- For a dead node there is no field on the Job or the pod that force-deletes the pod; the manifest can
  only tune how soon it is marked for deletion (`tolerationSeconds` on the `unreachable` and
  `not-ready` tolerations). The fix is an operator action once the node is known to be down: taint
  the node `node.kubernetes.io/out-of-service` (NoExecute), which tells the control plane to force
  delete the pods on it, or delete the Node object. `kubectl delete pod --force --grace-period=0`
  also works, but only do it when the node is confirmed down, since it removes the pod object
  without any evidence the process stopped. Check the Kubernetes documentation for your version
  before relying on the exact taint form.
- The dev cluster has one node, so a dead node means no cluster and this does not arise there.

## Known issues and limits

- **Access to the outside is checked once, at the end of `up.nu`, and only reported.** A pod runs `git ls-remote` against one public repository (`POLAR_EGRESS_PROBE_URL` changes it) and `up.nu` says whether the name resolved and the host answered. It is a note because a cluster used offline works. The probe runs in the `polar-scheduler-observer` pod, the one image with a git CLI, so if that pod is not running the check says it was skipped. In nix-usernetes, CoreDNS forwards names it does not own to the host's `systemd-resolved` through a second listener on `127.0.0.1:5300` (`services.resolved.settings.Resolve.DNSStubListenerExtra` on the host); without it pods resolve nothing outside the cluster, and this check says so.
- **Service-account token ordering.** On a fresh cluster, a `kubernetes.io/service-account-token` Secret is populated by the token controller only after its ServiceAccount exists, so the first apply may need repeating. `serviceaccounts.yaml` precedes `kube-observer-resources.yaml` in `apply_order`, which should avoid it, but this has only been exercised on an existing cluster.
- **Only `agent` is a catalog kind, and only the kube agents are in the catalog.** Everything else is a custom chart.
- **`apply_order` is hand-maintained** in each `target.nu`. It should be derived from the catalog (resources before the Job).
- **`render.nu` passes cluster facts to custom charts through a `match` on the chart name,** and `target.nu` carries values (`cert_issuer_url`, `polar_init_image`, ...) that the catalog derives elsewhere. That is data written as control flow, and a duplicated fact.
- **Only the kube agents declare capabilities in Rust.** Other agents still hard-code health arguments and Cassini/Neo4j endpoints; they move to the builder one pair at a time.
- **`POLAR_CAPABILITIES` unset is a warning, not an error,** so an agent run outside the cluster still starts. A deployment that forgets to set it is therefore not caught by the builder; the catalog sets it for every workload it renders.
- **The cluster domain drives catalog addresses,** but custom charts still hard-code `svc.cluster.local`, and `polar-init --cluster-domain` is not passed.
- **`src/deploy/`** is the old system. Confirm it is dead and remove it, or mark it.
- **Only the catalog's manifests are cached.** The custom charts (cert-issuer, Neo4j, ACE, ...) are rendered in full
  on every run; see "The render cache" for what that costs.
- **Resolving the workloads is not cached.** A warm render still runs `resolve` for each workload twice (its
  pod and its resources, each about a quarter of a second), which is most of what remains of a warm render.

Open items are tracked in `remaining-work.md`.
