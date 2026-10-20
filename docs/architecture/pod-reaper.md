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

# Reaping Failed pods

Status, 2026-10-20: a design for review. Nothing here is built.

## The problem

Every agent ends a pod by exiting 1, so that its Job (`restartPolicy: Never`, `backoffLimit` 1000000) replaces it
(`health-and-shutdown.md`). Rejuvenation does this about hourly per agent, and every ending leaves a pod in phase Failed
next to its replacement. An agent that cannot start (the Jira observer with no Jira to reach) leaves one every few
minutes. Nothing removes them: `ttlSecondsAfterFinished` acts only when a Job finishes, which these never do, and the
cluster's own pod garbage collection is a controller-manager setting outside Polar's reach. The only cleanup is
`just cluster-prune-failed`, by hand; a weekend of failed Jira observer pods once filled a node until it tainted itself
with disk pressure.

## The shape

Two agents, so that the right to see and the right to act are held by different identities, and each holds the least it
needs.

- **The pod observer** takes an inventory of the Failed pods of Polar's Jobs in Polar's namespaces, on a schedule the
  scheduler agent can change. For each it reports what anyone who cares about the failure needs: the pod and its UID, its
  Job, when it failed, each container's exit code and reason, and the end of its log, which is gone once the pod is. It
  answers a request for the current set, so a Reaper that restarts asks instead of waiting for the next round.
- **The Reaper** decides retention and deletes. It keeps the newest one or two Failed pods of each Job, for long enough that
  their logs and graph edges can be taken (a setting of its signed configuration), and deletes the rest. It acts only on
  what the pod observer reported, and reports what it deleted and why.
- **The graph** gets a node for each failure (the pod, its Job, its exit code and reason, the log's end) when the
  observer reports it, and an edge from it to the reaping when the Reaper reports that. A failure stays walkable after its
  pod is gone.

Nothing outside Polar's namespaces is seen or touched: the boundary is enforced by Kubernetes (below), not only by the
agents' code.

## Controls, at each layer

| Layer | Pod observer | Reaper |
|---|---|---|
| Kubernetes RBAC | a namespaced Role in each Polar namespace: `pods` get, list, watch; `pods/log` get | a namespaced Role in each Polar namespace: `pods` delete, nothing else (not even get) |
| Kubernetes admission | | a ValidatingAdmissionPolicy that refuses the Reaper's identity any delete of a pod that is not Failed, not owned by a Job, or not labelled as a Polar workload |
| The Polar certificate | a capability that says it may report pods for reaping | a capability that says it may reap |
| Cedar | | `PolicyLookup` before each delete: `Action::"reap"` on `Pod::"<namespace>/<name>"`, with the pod's phase, owner and age as facts |
| Cassini | | acts only on a report whose `published_by` is the pod observer's identity, and whose publisher holds the observer's capability |

The ValidatingAdmissionPolicy (built into Kubernetes since 1.30) is what makes "only Failed pods" a fact of the cluster and
not of the Reaper's code: RBAC can say `delete` on `pods`, but cannot look at a pod's phase, and admission can (a delete's
`oldObject` is the pod being deleted). Each delete also carries `preconditions.uid` from the report, so a pod that was
replaced under the same name is not the one deleted.

## What exists, and what each control needs

- **Kubernetes RBAC.** The catalog's capabilities produce a ClusterRole and ClusterRoleBinding only
  (`infra/schema/resources.dhall`), and the only one is `kube-api-read`, `*/*` get, list and watch, cluster-wide. Both
  agents need namespaced Roles with narrow rules, so `resources.dhall` gains a Role and a RoleBinding per namespace, and the
  catalog gains two capabilities (with their `Capability` variants in `polar-agent-builder`, which a test keeps in step).
- **Admission.** None exists in the repository. The policy and its binding would be rendered with the Reaper's chart, in a
  layer before the Reaper, and checked by `render.nu`.
- **The certificate.** A workload certificate carries its identity and, from a ledger grant, `p:` permissions; `c:`
  capabilities are a closed set of administrative powers, minted only on `/internal/issue` and in practice only for
  accounts (`cert-issuer-isolation.md`). There is no workload capability yet, and grants are single-use, so a pod that
  rejuvenates hourly would lose one. This is the open design question below.
- **Cedar.** `polar-policy` evaluates the cluster's one rule set, which the credentials agent publishes; any builder agent
  can ask it (`PolicyLookup`). There is no `Pod` vocabulary yet, and the facts (phase, owner, age) are the asker's to give.
- **Cassini.** The broker does not decide who may publish or subscribe to a topic (`remaining-work.md` lists it as a gap to
  be addressed). It stamps the verified sender's identity on each message (`published_by`), which receivers can check, as
  the authority updates already are (`AuthorityCore::from_the_credentials_agent`).

## Open questions

1. **The capability in the certificate.** Two ways to give a workload a capability that rejuvenation does not lose:
   (a) from the catalog: a workload's capabilities are declared in its entry, rendered into cert-issuer's per-workload
   floor as server names are today, and written into each certificate it is issued; or (b) from the ledger: a standing grant
   (not single-use) on the credentials agent, signed by an administrator with `c:grant`, so that giving an agent an
   infrastructure privilege is an act someone signs and the ledger records. (b) fits "an infrastructure privilege is
   granted, not configured"; (a) needs no administrator at bring-up.
2. **The Kubernetes credential.** Today an agent's Kubernetes token is a Secret the catalog renders, mounted into its pod.
   Should the Reaper's instead be held by the credentials agent and released only to a certificate holding the reap
   capability, under Cedar? That ties the Kubernetes privilege to the Polar one, at the cost of the Reaper depending on the
   credentials agent to act.
3. **Retention.** Keep the newest N Failed pods of each Job (N = 2?) for at least T (an hour?), and delete the rest, and any
   Failed pod older than T' (a day?) whatever N says?
4. **What is kept of a log.** The end of each container's log goes into the failure's graph node: how much (200 lines? 64
   KiB?), and is a log a thing the graph should hold, or should it go to a store of its own with an edge to it?
5. **Names.** `pod-observer` and `reaper`, as their workloads, identities and crates?
