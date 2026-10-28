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

# Placement: Polar's workloads by node

Status, 2026-10-28: the plan, cleared by the maintainer, and built ("Built", below); not yet on the cluster.

*Where it is going (the maintainer, 2026-10-28):* Polar ships two kinds of exemplar target in `infra/targets`.
`sandbox-local`, with Flux and the node pools, shows how a deployment on a cluster with placement marks should work.
`local-minimal`, without Flux, is for development on a single-node cluster, where the pools mean little. So placement
is declared in `sandbox-local` and nowhere else.

## Why

The maintainer wants Polar's workloads separated by node, on a cluster that carries placement marks, and Polar's
deployment to honor them. The low-trust and high-trust halves already exist in the design: an observer is what the
outside reaches, and the consumer owns the state (`credentials-agent-architecture.md`, "Consumer (high-trust)" and
"Observer (low-trust)"). Separate nodes put them on
separate hosts' kernels and volumes, so a compromised observer is not on the host that holds a writer's key or the
graph's volume.

## What the cluster gives (nix-usernetes `b14d71b`, `822d2b0`)

`u7c`, from `~/Documents/projects/nix-usernetes/cluster.dhall`: `cp` runs etcd and the control plane and is no
Kubernetes node (no pod runs there), and four workers, each one pool, labelled `polar.dev/pool=<pool>`:

| Node | Pool | Taint |
|---|---|---|
| `core` | Polar's core | `polar.dev/pool=core:NoSchedule` |
| `observers` | the low-trust side | `polar.dev/pool=observers:NoSchedule` |
| `writers` | the high-trust side, what writes the graph | `polar.dev/pool=writers:NoSchedule` |
| `services` | what Polar relies on and is not Polar proper, and CoreDNS | none |

A pod goes to a dedicated pool only with both a `nodeSelector` on the label and a toleration of the taint (a selector
alone stays Pending, "untolerated taint"); a pod that asks for no pool lands on `services`. Workers register already
carrying their label and taint. `u7s`, the single node, has no pool labels, so a selector would leave a pod Pending
there: placement is the target's to declare.

## The mapping

| Pool | Workloads | Why |
|---|---|---|
| core | credentials-agent, cert-issuer, cassini | the maintainer, by name |
| core | git-repo-controller | decides what is observed, holds no graph password (`git-controller.md`): off the writers' host, the power to decide and the graph password stay apart. *Decided, the maintainer, 2026-10-28* |
| core | polar-git-server | serves signed authority (the schedules, the policy). *Decided, the maintainer, 2026-10-28* |
| observers | git-repo-observer, jira-observer, kube-observer, polar-scheduler-observer, credentials-agent-observer | the low-trust side; the credentials agent's observer is its low-trust gateway |
| writers | kube-consumer, git-repo-processor, jira-processor, polar-scheduler | the graph writers (polar-scheduler writes the ledger to the graph, `git-controller.md`) |
| writers | Neo4j (hand chart) | beside its only writers, off the observers' host. *Decided, the maintainer, 2026-10-28* |
| services | ace (hand chart) | not Polar proper, faces people, holds no Polar authority. *Decided, the maintainer, 2026-10-28* |
| services | Flux, Jaeger, Postgres, Keycloak, the in-cluster registry, harness-producer, harness-sink | not Polar proper: the open pool, by its rule; nothing is rendered for them |

## The design

1. *The pool is the workload's, in the catalog.* Each entry in `infra/config/workloads/*.nuon` that belongs to a
   dedicated pool says `pool: "<pool>"`; one that says none is the open pool's. What a workload is decides its pool,
   not the target.
2. *The pools are the target's.* A target whose cluster has them declares the dedicated ones:
   `cluster: { placement: { pools: ["core" "observers" "writers"] } }` in `targets/sandbox-local/config.nuon`. The
   default (`config/cluster.nuon`) declares none, so `local`, `local-minimal` and `u7s` render as today.
3. *Rendered only when declared.* `resolve.nu` gives the pod record `placement: { pool }` when the target declares the
   workload's pool, else nothing; `schema/agent.dhall` renders the `nodeSelector` and the toleration from it. The key
   is `polar.dev/pool`, nix-usernetes' (`cluster.dhall`, `poolKey`), one constant in `resolve.nu`.
4. *A pool the target lacks is refused* when the target declares placement: a workload asking for `writers` on a target
   that declares only `core` names itself and the pool, rather than landing on the open pool unseen (as the render
   refuses escrow without custody). A target that declares no placement places nothing and refuses nothing.
5. *Neo4j, the one hand chart placed.* Its pool, `writers`, is in its `values.dhall`; `render.nu` hands the chart the
   target's declared pools, and `statefulset.dhall` renders the selector and toleration when `writers` is among them.
   The other hand charts are the open pool's and do not change.

## Tests first

- `infra/tests/rules.nu`: the placement rule (declared, not declared, a pool the target lacks).
- `infra/tests/resolve.nu`: under `sandbox-local`, every enabled workload's pod record has the pool of the mapping
  above and nothing for the open pool's; under `local-minimal`, none has any; a pool the target lacks refused.
- `infra/tests/charts.nu`: Neo4j's StatefulSet with the selector and toleration under `sandbox-local`, without under
  `local`.
- Render determinism: `local` and `local-minimal` render byte for byte as before; `sandbox-local`'s diff is the
  selectors and tolerations alone.
- Mutations: the toleration left out (the pod would stay Pending); the selector rendered on a target that declares no
  placement; a pool the target lacks let through.

## Built (2026-10-28)

- *The rule* (`infra/rules.nu`): `POOL_KEY`, `placement_of` and `placement_problems`, beside the render's other rules,
  so the resolver and Neo4j's chart share them (the plan had the key in `resolve.nu`).
- *The catalog:* `pool:` on the fourteen entries of the mapping; none on the harness, the secret harness or
  oci-registry-resolver, which are the open pool's (the last two are not in `sandbox-local`; a target that runs them on
  pools would decide theirs).
- *The resolver* (`infra/resolve.nu`): the record's `placement`, refused when the target lacks the pool, and refused
  when a target sets a workload's `pool` itself; `resolve.nu placement` gives the target's declared pools, for
  `render.nu` to hand Neo4j's chart.
- *The templates:* `functions.placementOf` (`infra/schema/functions.dhall`) makes the selector and the toleration from
  an `Optional Placement`; `agent.dhall`, `ca.dhall` (cert-issuer) and Neo4j's `statefulset.dhall` use it.
- *The target:* `sandbox-local` declares `placement: { pools: ["core" "observers" "writers"] }`.
- *Tests:* the rule (`tests/rules.nu`); under `sandbox-local` every workload in the mapping's pool and the open pool's
  in none, under `local-minimal` none, a pool the target lacks refused, a target setting a pool refused, and the
  rendered pod's selector and toleration in both templates (`tests/resolve.nu`, 60 of 61 pass: the one that fails, "workloads
  still asking cert-issuer are listed as pending", fails the same way before this change); Neo4j's StatefulSet placed,
  not placed, and refused (`tests/charts.nu`). Mutations caught: the toleration left out; placement on a target that
  declares none; a pool the target lacks let through.
- *Renders before and after:* `local` the same byte for byte; `local-minimal` the same; `sandbox-local` differs by the
  fourteen pods' and Neo4j's selectors and tolerations alone.

## On the cluster

u7c is reset onto the new layout (`just cluster-reset && just cluster-up && just cluster-init && just
cluster-kubeconfig` in nix-usernetes), which wipes it: done only when the maintainer says. Then `up.nu sandbox-local
--dev`, and checked: every pod on its pool's node (`kubectl get pods -o wide`), nothing Pending, every PVC bound, and
the agents registered and the custodians fetched as before. The result goes to the nix-usernetes session.

## Open, and assumed

- *local-path's helper pods on a tainted node* (untested by nix-usernetes): the provisioner creates a volume's
  directory with a helper pod on the volume's node. Three volumes land on tainted nodes: the credentials agent's ledger
  and the git server's repositories (core) and Neo4j's (writers). If one of their PVCs hangs, the helper pod is the
  first place to look, and its tolerations are the provisioner's configuration, nix-usernetes'.
- *Assumed, to check on the cluster:* the in-cluster registry's NodePort (30500), which every node's kubelet pulls
  from at `localhost:30500`, answers on every worker, as it did on one node and on `cp` and `core`.
- *Not in this plan:* placement for the other targets (no cluster of theirs has pools), and anything finer than a
  node (per-pod user namespaces, `hostUsers: false`, are a separate item).
