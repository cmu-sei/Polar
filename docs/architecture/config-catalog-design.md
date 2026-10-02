# Configuration catalog: design record

Status: stage 1 built and verified (resolver + kube slice); stage 2 (agent.dhall, resources.dhall, render wiring, compare script) written, awaiting the golden diff on the user's machine (Dhall untested by the author).
Branch: `feat/ace-integration`. Written 2026-10-02 so the design does not live only in a chat.

## 1. Goal

Centralize configuration. Make what an agent *is* (its facts) plainly visible in
one place, separate from the logic that turns facts into manifests. Variation
between agents should be **data**, not code branches.

Three tiers, named for newcomers:

| Tier | What | Where | Rule |
|---|---|---|---|
| Configuration | entity data: nouns, facts | nuon (`infra/config/**`, `infra/targets/*/config.nuon`) | no logic |
| Structure | templates: what a Job/Deployment looks like | Dhall (`infra/schema/**`) | pure functions of typed records; structure only, no facts |
| Automation | orchestration: verbs | nushell (`up.nu`, `render.nu`, `images.nu`) | effectful; thin |

Between data and structure sits a **pure resolver** (`infra/resolve.nu`): config
in, fully resolved records out, no side effects, testable, with `explain`.

`constants.dhall` already states this taxonomy in its header (env-dependent values
via render.nu, target-specific in overrides.dhall, constructed resources in the
chart, secrets elsewhere) and then breaks it: it holds constructed resources
(`certEmptyDirVolume`, `saTokenVolume`, `commonClientEnv`) next to static names.

## 2. Layering and precedence

`kind defaults` < `workload entry` < `target override` (a gitignored local overlay
was discussed, not built).

Capabilities (`needs`) *union* into list-valued fields (env, volumes, mounts, certs,
dependency checks) in declared order. `explain` shows provenance per value, like
`git config --show-origin`: `[kind]`, `[entry]`, `[cassini]`, `[derived]`.

Target-specific values (e.g. `JIRA_URL`, `JIRA_EMAIL`, today in `overrides.dhall`)
come from the target layer, not the workload entry.

## 3. The capability vocabulary (the shared contract)

An agent declares `needs: [...]`. `infra/config/capabilities.nuon` maps each name to
what it contributes, organised by **facet** (`infra/config/facets.nuon`):

| Facet | What it answers |
|---|---|
| `pod` | env, volumes and mounts the agent container gets |
| `identity` | client certs polar-init obtains (each adds its volume, mounts, a health-checked cert path) |
| `dependency` | the service whose cert must be valid first, and for how long (init `--check-dep-cert`, `POLAR_HEALTH_DEP_ENDPOINTS`) |
| `resources` | Kubernetes objects alongside the workload (cluster roles, token Secrets) |

Text values may use `{<service>.dns|port|addr}` and `{workload}`.

**Exhaustiveness is enforced by the resolver.** Every capability must answer every
facet in `facets.nuon` (`{}` where it has nothing to add) and may answer none that is
not listed. `nu infra/resolve.nu check` fails on a missing facet, an unknown facet,
a dependency naming a service not in `services.nuon`, or an unresolvable placeholder.
This is the visitor idea's real value (adding a capability or a facet cannot leave a
cell of the capability x operation grid forgotten) delivered at the earliest stage of
the pipeline, so the Dhall stays simple. A Dhall `Need` union with handler records
was considered; it would enforce the same thing in the type system but depends on the
untested json-to-dhall union encoding and complicates the Dhall. It can be layered on
later without changing the facet shape.

History: an earlier version of this design dropped the visitor ("footprints are data")
because only one operation, the pod fragments, was in view. The kube RBAC chart was
then left behind as a custom chart, which showed the second operation (`resources`)
and the gap. Current vocabulary: `cassini`, `neo4j`, `kube-api-read` (ClusterRole +
binding + token Secret + KUBE_TOKEN env + token mount). Planned: `graph-write`,
`encryption`, `logger`/tracing. Names are meant to match the Rust builder's capability
methods.

Derived, never hand-written per agent:
- `POLAR_HEALTH_CERTS` = comma-joined client cert paths of the needs
- `POLAR_HEALTH_DEP_ENDPOINTS` = comma-joined `host:port:ttl` of the needs' dependencies
- init `--check-dep-cert host:port:ttl` and `--cert type:dir:alg[:sans]`
- a workload's ServiceAccount is uniform: render.nu emits one for every workload
  identity no manifest declares (the catalog does not need to).

## 4. Two builders, one vocabulary

- **Agent builder (Rust, `polar-agent-builder`)**: runtime wiring inside the process.
  `AgentBuilder` composes `InfrastructureSupervisor` (HealthCheckActor, RouterActor,
  TcpClientActor, optional CryptoActor; respawn; PrepareShutdown/ProceedWithShutdown
  handshake), `Subscription`, `HealthHandle`, `CryptoHandle`, type-state
  `Outbound -> Encrypted/Plaintext`. Only kube-observer and kube-consumer use it today.
- **Manifest builder (resolver + generic `agent.dhall`)**: deployment wiring at render time.

Agreed properties:
1. Names are checked in both directions: a test compares the Rust builder's
   capability names against the vocabulary, so a rename on one side fails on the other.
2. Mismatches fail loudly on both sides: the manifest builder rejects a need with no
   footprint at render time; the agent builder rejects a capability the deployment
   did not provision at startup with a **non-zero exit** (fits the no-graceful-exit rule).
3. Proposed (user onboard): the deployment declares the capabilities it provides
   (e.g. one env var) and the builder cross-checks `requires ⊆ provides`. It could
   also derive dependency endpoints from the same facts, removing cluster-domain
   literals from Rust.
4. The vocabulary starts with what exists and grows with the builder; catalog entries
   use capability names from the start so a later Rust rename does not touch the
   deployment side.

Facts learned about health config: `HealthCheckActor::pre_start` reads env first and
falls back to `HealthCheckArgs` (`POLAR_HEALTH_DEP_ENDPOINTS`, `_CERTS`,
`_REJUVENATION_SECS`, `_EXPIRY_SECS`, `_TICK_SECS`, `_GRAPH_DEADLINE_SECS`); it
validates rejuvenation > expiry. Builder migration does **not** change the manifest
contract (kube-observer is migrated and its Dhall still sets the env var). Every
agent's Dhall (all 19 files) sets `dep_cert_endpoints`, except kube-consumer.

## 5. Decisions

- Derive BOTH init `--check-dep-cert` and `POLAR_HEALTH_DEP_ENDPOINTS` from `needs`
  for every agent. Intent (user): don't start an agent whose dependency certs are
  about to expire.
- Pair per agent: migrate the Rust agent to the builder, add its catalog entry,
  golden-diff. Order: kube pair (local-minimal) -> jira -> credentials-agent.
- **Rendered manifests are the contract.** Render with the old pipeline, render from
  the catalog, diff. List order is non-semantic (multiset compare).
- Escape hatch stays: anything that does not fit the generic agent template remains a
  custom chart. That need is what created the render pipeline originally; credentials-agent
  (lives under `2-services`) is the first real test of it.
- Companion resources are a facet, not a custom chart: `kube/` (rbac.dhall, main.nu, values.dhall, observer/consumer.dhall) is replaced by the `kube-api-read` capability, `schema/resources.dhall` and `<name>-resources.yaml`.
- A single generic `agent.dhall` replaces per-agent `observer/consumer/processor.dhall`.
  Risk is a god-function; mitigation is data-not-flags (no `hasGraph : Bool`-style
  fields) plus the escape hatch.
- The resolved record is flat, uniform, null-friendly: Dhall has no Text equality, and
  `json-to-dhall` with union payloads is untested.

## 6. Drift found (the argument for the catalog)

- Image names in three places: `values.dhall` (`polar-kube-observer:latest`, stale,
  masked), `overrides.dhall` (`registry ++ "kube-observer:latest"`), `images.nuon`.
- `certIssuerUrl` in three places: `constants.dhall`, `target.nu`, neo4j block of `overrides.dhall`.
- Identity lifetime overrides disagree: `target.nu` has graph and ace, `overrides.dhall` only graph.
- Neo4j bolt address typed in `target.nu` though `Constants.neo4jDNSName` derives it.
- `svc.cluster.local` literal in many files (cluster-domain backlog; `polar-init --cluster-domain`
  exists but is never passed by Dhall).
- `apply_order` is 30 hand-typed filenames; derivable from chart list + dependencies.
- `render.nu` `match $chart_name`: nine of twelve arms pass the same four values
  (`neo4jBoltAddr`, `certIssuerUrl`, `certIssuerAudience`, `polarInitImage`), i.e. a
  table written as control flow.
- Each chart `main.nu` (~35 lines x ~12) builds Dhall by string concatenation.
- `validate.nu` only validates the environment; the real validation passes are the
  post-render checks in `render.nu` (unique ServiceAccount identities, cert-issuer
  identity vs URL, CA provisioning). Keep them as a safety net after catalog checks.
- Four inconsistent pods: kube-consumer lacks `POLAR_HEALTH_DEP_ENDPOINTS`; only
  kube-consumer has `--check-dep-cert`; only kube-observer leaves
  `automountServiceAccountToken` unset (deliberate: `sa_token_secret`).

## 7. Expected golden-diff deltas

kube slice (verified in stage 1; these are the only differences):
- kube-consumer: + `POLAR_HEALTH_DEP_ENDPOINTS` = cassini + neo4j, ttl 300 (no-op vs the Rust fallback)
- kube-observer: + init `--check-dep-cert` for cassini

jira slice (expected, not yet built): jira-observer and jira-processor each gain
`--check-dep-cert` args.

## 8. Stages

Stage 1 (done): `infra/config/{cluster,services,capabilities,kinds}.nuon`,
`config/workloads/kube.nuon`, `targets/local-minimal/config.nuon`, `infra/resolve.nu`
(`list`, `check`, `explain`, `json`, `diff`). Verified against golden manifests.

Stage 2: generic `schema/agent.dhall` reusing `functions.makePolarInitContainer`
(10 args) and the Proxy* helpers; render wiring (resolve -> JSON -> `json-to-dhall` ->
`dhall-to-yaml`) coexisting with legacy charts; `kube/main.nu` reduced to RBAC only;
golden-diff harness. Dhall is untested by the author (no toolchain in the sandbox).

Then: jira (after its Rust migration), credentials-agent (tests the escape hatch).

## 9. Open items

- Capability rename in the Rust builder before migrating the remaining agents (migrate once, to the final vocabulary).
- Confirm `HealthCheckArgs` hard-coding was convenience, not a decision.
- Pass `--cluster-domain` to polar-init from `cluster.domain`.
- Derive init env (currently `Constants.commonClientEnv`) from the cassini capability.
- Static check: rejuvenation > expiry.
- Remove dead Rust endpoint fallbacks after the env is always set.
- `images.nuon`: rows for scheduler/git/openapi/build-processor/resolver, then delete those four Justfile recipes.
- Justfile leftovers: dev-PKI recipes and comment block, `Agent containers` recipe, stale comments, orphan provisioner comment.
- Fresh-cluster `up.nu` end-to-end run.
- After the working cluster: builder rollout to all agents; audit that no agent exits gracefully (always non-zero so Jobs restart).

## 10. Working conventions

- Never `git add` certs or keys. `ring` was removed on purpose.
- Do NOT change product code to suit the dev environment.
- Commits: conventional, single-quoted `git commit -m` with no apostrophes, with the two trailers.
- Nushell: `env` is a reserved variable name; don't shadow builtins; closures can't capture `mut`
  variables (return records and collect); `do -i ... | complete` does not catch missing commands (use `try`);
  `input` needs a TTY. Tested on 0.106.1: `merge deep`, `reject -o`, `get -o`, `window 2 --stride 2`, `glob`, `uniq-by`.
