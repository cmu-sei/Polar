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

# Agents as Deployments that restart in place

Status, 2026-10-27: cleared by the maintainer, its five questions decided as written below. 2026-10-28: build steps 1 to
3 done (the custody daemon, `key-custody.md`; the renewal rule, below; the Dhall schema at 1.35, below); step 4 next.

## Why, and what Kubernetes gives

Every agent is a Job (`restartPolicy: Never`, `infra/schema/agent.dhall:226`) and ends by exiting 1, so that its Job
makes a new pod (`health-and-shutdown.md`). Each ending leaves a Failed pod (`pod-reaper.md`, "The problem"), a new pod
has a new UID, and a changed Job is replaced by force, which runs the old pod and the new one at once (the ledger fork of
2026-10-26, `remaining-work.md`). The maintainer's plan (`runtime-attestation.md`, "Deployments, next"): Deployments whose
pods restart in place.

KEP-5532 does it (`runtime-attestation.md`, "Deployments, next", "Checked"): a container restart rule with the action
`RestartAllContainers` restarts the whole pod, init containers again, keeping its UID, IP and volumes. Beta and on by
default since 1.36 (released 2026-04-22); tried on sandbox-local (v1.37.1) on 2026-10-27.

## What has to be settled first

Each with a recommendation; to be taken one at a time.

1. **The renewal rule, in its two places.** The credentials agent refuses a second certificate of a type to a pod while
   the first is valid: in issuance (`credentials-agent/consume/src/issuance.rs`, `AlreadyHasValidCertificate`) and in
   the held-request voucher (`credentials-agent/consume/src/held_voucher.rs`, `rule`, "already holds"). A pod restarted
   in place keeps its UID and asks again, before its certificate expires (that is why it rejuvenates), so both would
   refuse it. *Recommendation:* both allow a new certificate to a pod that holds one, with a bound (below), and the
   ledger supersedes the old one, as it already does for a new pod (`state.rs:265`: a new certificate of a type revokes
   the identity's earlier ones, whatever their pod).
   **Decided (the maintainer, 2026-10-27): yes, and supersede.**
2. **Telling a restart from a second ask.** The credentials agent cannot see Kubernetes, so it cannot see a restart.
   What makes a second ask safe today is that only `polar-init` can ask: the workload never holds the service-account
   token. *Options:* (a) trust that, with a bound so no pod is issued more than one certificate of a type in, say, five
   minutes (a loop or a flood is refused; a real restart is not that frequent); (b) require the attestation agent to say
   the pod restarted (its restart count, or the `AllContainersRestarting` condition), which waits for it to be built;
   (c) allow a renewal only near the certificate's end, which refuses a crash restarted early in its certificate's life,
   and with the custody daemon (`key-custody.md`) such a pod has no key to go on with. *Recommendation:* (a) now, (b)
   when the attestation agent exists.
   **Decided (the maintainer, 2026-10-27): (a), trust that only `polar-init` asks, with the five-minute bound; (b) when
   the attestation agent exists.**
3. **What superseding does to the running certificate.** The old certificate is revoked in the ledger and its serial
   published (`polar-authority-updates`). With the custody daemon its key dies with the restart; without it, the old
   `key.pem` stays in the shared volume until `polar-init` overwrites it, moments later. *Recommendation:* the daemon
   first (item 4), so nothing of the old key outlives the restart.
   **Decided (the maintainer, 2026-10-27), with item 4: the daemon first.**
4. **The order: the custody daemon, then the move.** Without the daemon, `polar-init` finds its old bundle valid
   (`keeps_existing_bundle`, `polar-init/src/main.rs`) and keeps it, so a rejuvenating pod restarts into the certificate
   it is rejuvenating away from, and ends again at once. With the daemon the key is new at every start, so no old bundle
   matches it and `polar-init` asks. *Recommendation:* build the daemon first; `keeps_existing_bundle` then goes (the
   case it was made for, a pod restarted with its certificate still good, is a fresh key now). The other way round
   would mean teaching it to tell rejuvenation from a crash.
   *Corrected 2026-10-28, read against the code:* it does not go. A certificate whose key is the daemon's never keeps
   an old bundle already (`issue_custody_cert_from_cert_issuer`, `polar-init/src/main.rs`; through the observer the
   custodian fetches it); a key-file certificate still does, and still needs to: Neo4j's StatefulSet, the `ace` job and
   the unrendered agent charts, none of which rejuvenates, and Neo4j is the case it was written for (2026-10-25).
   **Decided (the maintainer, 2026-10-27): the custody daemon first, then the move.**
5. **The ticket (`core-restarts.md`, step 5).** A credentials agent restarted in place is its own successor, under the
   same UID, so the outgoing one can name it: a one-use ticket for its identity and its UID, published before it exits.
   *Recommendation:* step 5 is built on this, after the move; the crash window (step 4) stays for a pod really replaced.
   **Decided (the maintainer, 2026-10-27): yes, after the move, and the credentials agent is subject to runtime
   attestation too:** the ticket says which pod may be its successor, and the attestation agent checks that pod and its
   process, as it does every other (`runtime-attestation.md`); neither stands in for the other.

## How the move is done (no decisions in it, only care)

- **Each agent a Deployment of one, `Recreate`.** A rolling update starts the new pod before the old one ends, two
  agents of one identity at once; `Recreate` keeps the Job's one-at-a-time. The ledger lock stays the backstop.
- **The restart rule.** The workload container's `restartPolicy: Always` and one rule, `RestartAllContainers` on every
  exit, so a rejuvenation, a crash and a shutdown verdict all run `polar-init` again. Native sidecars (the custody daemon)
  restart with them.
- **The Dhall types.** The schema imported dhall-kubernetes 1.31 (`infra/schema/kubernetes.dhall`), whose containers
  have no restart rules; 1.35 has them (beta in 1.35; the action is text, so `RestartAllContainers` fits). Moving the
  schema to 1.35 was a step of its own, every chart and the catalog rendered before and after: done 2026-10-28 ("Step
  3", below).
- **Init containers again in one pod.** `polar-init` writes its bundle atomically, file by file (`output.rs`,
  `write_bundle`), so a second run replaces the first. (The escrow key is the custody daemon's since custody step 9, and
  the daemon restarts empty with the pod: there is no keyring entry to displace.) Checked once more when built.
- **What goes.** The forced replacement of a changed Job (`infra/deploy-objects.nu:138`), since a Deployment changes in
  place; the digest check that restarts a Job by hand (`remaining-work.md`, 2026-10-14); Failed pods, so the Reaper
  stays only for clusters older than 1.36.
- **What does not change.** The health machine and the shutdown protocol: an agent still ends by exiting, and the pod
  restarts around it.
- **To read when built.** Whether in-place restarts back off like a crash-looping container (none was seen in the
  test, restarts every 8 s), and how a pod restarting in place shows in `up.nu`'s waits and the health checks.

## The renewal rule, the plan (2026-10-27, its question decided)

What is there, read for this:
- *Issuance* (`credentials-agent/consume/src/issuance.rs`):
  - step 4 asks the ledger `CheckRenewal { identity, instance_binding, cert_type }`, which is
    `has_valid_certificate` (`state.rs`), and refuses `AlreadyHasValidCertificate`;
  - step 7 appends `IssuanceCompleted` after cert-issuer has signed;
  - the check and the record are two steps around a network call, so two requests at once from one pod can both
    pass. That race is there today.
- *The voucher* (`held_voucher.rs`, `rule`): "already holds one" comes from the same `CheckRenewal`. A vouched
  certificate is signed by cert-issuer, so the ledger learns of it only from cert-issuer's record. The record is read
  every 30 s (`record_asker::ASK_EVERY`), so the check can lag that much.
- *Superseding is already there.* `record_workload_certificate` (`state.rs`) marks every earlier certificate of the
  identity and type `Superseded`, whatever its pod. `valid_certificates` is keyed by identity, pod and type, so it
  holds each pod's newest.
- *Superseded is not published.* `revoked_serials` takes explicit `Revoke`s only, and a test pins it
  (`a_certificate_replaced_by_a_reissue_is_not_an_explicit_revocation`). No reason is recorded. The architecture
  document gives superseding's purpose as one current certificate per identity for the lookup
  (`credentials-agent-architecture.md`, "A reissue for the same identity supersedes"). Item 3 above assumed the
  serial is published.
- *Two units in one field.* `IssuanceCompleted`'s timestamp is in milliseconds (`polar::time::now_millis`). A reported
  certificate's `signed_at` is in seconds (`cert-issuer/server/src/record.rs`). Both become
  `CertificateRecord.timestamp`.

The plan:
1. *One rule, in the ledger, for both callers.*
   - `LedgerMessage::Admit { identity, instance_binding, cert_type, now }` replaces `CheckRenewal`. A request is
     admitted unless that pod was issued, or admitted, a certificate of that type within the bound (five minutes).
   - Admitting records the time. The ledger actor answers one message at a time, so two requests at once cannot both
     pass, and the race closes.
   - The last issuance comes from the record, which is persisted and replayed. An admission not yet followed by a
     record is held in memory; losing it at a restart only lifts a bound.
2. *Issuance:* `Admit` in place of `CheckRenewal`.
   - Too soon is a new reason, `TooSoon`, which `polar-crypto` counts as temporary (`is_temporary`). `polar-init`
     waits it out rather than exiting, as it does everything that is not final.
   - `AlreadyHasValidCertificate` is no longer given; it stays in the enum for the wire.
3. *The voucher:* `rule` takes "too soon" in place of "already holds one", from the same `Admit`. On that path the
   refusal is final, as every held answer is (`core-restarts.md`, "3 in detail"): `polar-init` exits, and the pod
   asks again when it restarts.
4. *One unit:* `record_reported` stores `signed_at` in milliseconds, so the bound compares like with like. Replay
   gives the same, since the state is folded from the events.
5. *A superseded certificate is published as revoked* (decided below). Superseding puts the serial in
   `revoked_serials`, which makes the publisher send the set (`ledger.rs`, `revocation_changes`), and the test that
   pinned the opposite changes with it.
6. *One client certificate per pod, in the `local` target too.* The hand charts there (`layers/3-workloads/agents`:
   gitlab's consumer, build-processor, openapi's processor) still ask for a second client certificate for Neo4j
   (`neo4jClientCertPath`), for the same identity on the same pod. The second supersedes the first, and published,
   that would revoke the Cassini certificate in use. The catalog already settled this: one client certificate,
   presented to Cassini and Neo4j alike (`infra/config/capabilities.nuon`, `graph-write`). The hand charts follow it,
   their graph variables pointing at `/etc/tls/certs`. The `ace` job keeps its one.
7. *Tests first:*
   - a property: for any sequence of asks (pod, type, time) and issuances, each ask is admitted exactly when that pod
     had no issuance or admission of that type in the five minutes before. A log replayed gives the same answers;
   - examples:
     - a restarted pod admitted after the bound, its old certificate superseded;
     - one within the bound refused `TooSoon`, and `polar-init` waiting it out;
     - two at once, one admitted;
     - the voucher refusing too soon;
     - a reported certificate's time in milliseconds;
     - a superseded serial among the revoked, and published.
   - The tests that pin `AlreadyHasValidCertificate` (`issuance.rs`, two) and superseded-not-revoked (`state.rs`)
     change with the rule (items 1 and 3, decided).

*Decided (the maintainer, 2026-10-27): a superseded certificate is published as revoked.* The question, as it was put: Item 3 assumed it is, and today it is not. Publishing it
puts the old serial on `polar-authority-updates`. Agents then refuse the old certificate at once, the daemon of an old
pod still running destroys its key (`key-custody.md`, step 5), and the broker ends its sessions. The cost: the revoked
set grows by one serial for every restart, and the set is never pruned, so it keeps growing. Pruning a serial once
its certificate has expired is a follow-up (`remaining-work.md`).

*Built (2026-10-27).*
- *The rule:* `credentials-agent/consume/src/renewal.rs` (`Admissions`, `RENEWAL_BOUND_MILLIS`), applied by the ledger
  actor (`LedgerMessage::Admit` and `Release`).
- *Issuance:* `issuance.rs` asks it last, just before signing, refuses `TooSoon`, and gives the admission back when
  signing fails.
- *The voucher:* `held_voucher.rs` asks it only of an offer the other rules allow.
- *The state* (`state.rs`): `last_issued`; superseding puts the serial in `revoked_serials`; a pod reissued in place
  no longer leaves its old serial naming the new record; reported times are in milliseconds.
- *polar-crypto* counts `TooSoon` as temporary. The observer answers it `429 Too Many Requests`
  (`credentials-agent/observe/src/handler.rs`); its client reads the reply whatever the status.
- *The `local` target's three hand charts* present the one client certificate to Neo4j. `makeGraphEnv` takes the
  certificate's directory, and the `ace` job passes its own. The render diff is those three charts alone, and
  `sandbox-local` renders the same.
- *Tests:*
  - the property over asks, issuances, givings-back and restarts, its gaps landing on the bound now and then;
  - one per type and per pod;
  - the ledger's record holding the bound after a restart;
  - two asks at once;
  - a pod issued again after the bound with its old certificate revoked, and one within it refused `TooSoon` without
    reaching cert-issuer;
  - a failed signing given back;
  - the voucher refusing within the bound and vouching after it;
  - superseded serials revoked;
  - an in-place reissue not revoked by its old serial;
  - reported times in milliseconds.
- *Mutations caught:* the admission unrecorded, the ledger's record ignored, the bound inclusive (the property catches
  it too), superseded not revoked, the old serial still indexed, seconds kept, no giving back, the voucher ignoring
  the rule.
- *Not run:* `infra/tests/resolve.nu` and `render-determinism.nu` need a cluster (they validate against an apiserver).

## Build order, each tests first

1. The custody daemon (`key-custody.md`), which item 4 puts first.
2. The renewal rule, in issuance and in the voucher, with the bound (items 1 and 2). Built after the daemon's `polar-init` step and before
   any pod is given a daemon (the maintainer, 2026-10-27; `key-custody.md`, "Step 3, the plan"): with the daemon, a pod
   whose init container runs again has a new key and must be issued again.
3. The Dhall schema at 1.35.
4. The `agent` and `service` kinds as Deployments with the restart rule and `Recreate`; the hand charts after them.
5. On a fresh sandbox-local: every agent restarts in place at rejuvenation, keeps its UID, is issued a new certificate,
   and leaves no Failed pod; the credentials agent's successor comes through its window; nothing runs twice.
6. The ticket (`core-restarts.md`, step 5).

## Step 3, the Dhall schema at 1.35: the plan (2026-10-28, for review)

Why: step 4's restart rule is a container field, `restartPolicyRules`, which dhall-kubernetes 1.31 does not have and
1.35 does (`Container.restartPolicyRules : Optional (List { action : Text, exitCodes : Optional { operator : Text,
values : Optional (List Natural) } })`, read from the pinned package itself). The step changes the types and nothing
rendered.

What a dry run found (2026-10-28, in a scratch worktree at `cd3d2a86`, every target's catalog and charts rendered with
1.31 and with 1.35):
1. *Four pins, not one.* `infra/schema/kubernetes.dhall` names the package (URL and hash), and `agents.dhall`,
   `constants.dhall` and `functions.dhall` import `./kubernetes.dhall` pinned by the same hash. Dhall resolves a
   hash-pinned import from its cache by the hash, so with only the first changed those three still load 1.31, and every
   render fails ("You can only append ❰List❱s with matching element types": a 1.35 `Volume` beside a 1.31 one). All
   four move to `sha256:2086df21…`. `agents.dhall`'s own pin (`8458543a…`, in `constants.dhall` and
   `functions.dhall`) does not change: nothing it exports depends on the Kubernetes types.
2. *No download.* The same package at the same hash is already pinned by `src/deploy/types/kubernetes.dhall`, so it is
   in the dev shell's built cache (`src/flake/dhall-cache.nix` collects every pin in the repository) and in the local
   one; `infra` moves to the URL and hash already there.
3. *One type change reaches the charts:* `StatefulSetSpec.serviceName` is `Optional Text` in 1.35 (`Text` in 1.31).
   Neo4j's (`layers/2-services/neo4j/statefulset.dhall`) and Postgres's (`layers/2-services/postgres/statefulset.dhall`)
   StatefulSets give it `Some`.
4. *With those, nothing rendered changes:* `local-minimal`, `local` and `sandbox-local`, catalog and charts, byte for
   byte the same; the agent charts no target renders (`layers/3-workloads/agents`) type-check on both.

The plan:
- *Test first* (`infra/tests/charts.nu`): a container with a `RestartAllContainers` rule renders from the schema's
  types, its rule in the YAML. It fails on 1.31 (no such field) and passes on 1.35, and it is what step 4 builds on.
- *The change:* the four pins, and the two `serviceName`s.
- *Checked:* the renders before and after, byte for byte, for the three targets (the gitignored `cluster-inputs.nuon`
  copied into the worktree, which the first comparison of 2026-10-28 lacked); `tests/charts.nu`, `rules.nu`,
  `resolve.nu`; the unrendered agent charts type-checked. Mutation: one of the three local pins left at 1.31, which the
  charts' render fails on (as the dry run did).
- *Not here:* rendering any restart rule (step 4); the cluster, since nothing rendered changes, so no redeploy;
  `src/deploy`, a tree `infra` does not render from (found reading for this, not touched).

*Cleared by the maintainer and built (2026-10-28).*
- *The test* (`infra/tests/charts.nu`, "the schema's containers take a restart rule"): a container with a
  `RestartAllContainers` rule, from the schema's own types, renders with its rule. It failed on 1.31 ("Expression
  doesn't match annotation", `+ restartPolicyRules`) and passes on 1.35.
- *The change:* `infra/schema/kubernetes.dhall` at 1.35 (`sha256:2086df21…`), with a comment that the three files
  importing it move with it; `agents.dhall`, `constants.dhall` and `functions.dhall` pinned to the same hash; Neo4j's
  and Postgres's `serviceName = Some …`.
- *Checked:* `local-minimal`, `local` and `sandbox-local`, catalog and charts, rendered before (HEAD, its
  `cluster-inputs.nuon` copied in) and after: byte for byte the same. The unrendered agent charts type-check.
  `tests/charts.nu` (10 of 10), `rules.nu` (29 of 29), `resolve.nu` (60 of 61: the one failing, "workloads still
  asking cert-issuer are listed as pending", fails the same way without this change). Mutation caught: `functions.dhall`'s pin left at 1.31, six of
  the chart tests failing on it.

## Step 4, the kinds as Deployments that restart in place: the plan (2026-10-28, for review)

### What was read, and tried on the cluster

- *What renders a workload:* `infra/schema/agent.dhall` (the `agent` and `service` kinds) and `ca.dhall` (`ca`,
  cert-issuer) render a `Job` (`restartPolicy: Never`, `backoffLimit`, `podReplacementPolicy: Failed`,
  `ttlSecondsAfterFinished`, from `config/kinds.nuon`). Their pods are labelled `name: <workload>`.
- *What assumes a Job,* besides the templates:
  - `infra/up.nu` waits on `job-name=<workload>` (cassini, the credentials agent and its observer, the git server, the
    scheduler and its observer: lines 314-318, 645-648, 741), and on a changed manifest restarts its Jobs itself
    (`job_verdict`, 179-230);
  - `infra/apply.nu` (the targets without Flux) deletes and applies a changed Job again, its pod template being
    immutable, and waits on `job-name=cassini`;
  - `infra/deploy-objects.nu` forces every layer that holds a Job (`has_job`), and since Flux counts a Job healthy only
    once complete, which an agent never is, checks an agent through the Endpoints of a Service that selects its pods
    (`health_check`);
  - `render.nu` checks every Job's `podReplacementPolicy` (`rules.nu`, `job_replacement_problems`).
- *What must stay a Job:* the load harness's producer and sink, which `infra/load-harness.nu` copies into a run's Jobs,
  each running to completion and logging one result; and `egress.nu`'s probe, a one-off.
- *Tried on u7c (2026-10-28, a scratch namespace, since removed):* a Deployment of one, `Recreate`, its pod
  `restartPolicy: Always`, a native sidecar, an init container, and a main container with `restartPolicy: Always` and
  one rule, `RestartAllContainers` on `exitCodes: { operator: NotIn, values: [] }` (every exit code: the API server
  takes it, and refuses a rule with no `exitCodes`):
  - exit 1: the whole pod again, the sidecar started anew and the init container run again, in 3 s;
  - exit 0: the same;
  - *killed by its liveness probe: only the main container again,* some 31 s on, no sidecar restart and no init
    container: a probe's kill is not an exit the rule acts on;
  - one pod, one UID, throughout.

### The change

1. *The `agent`, `service` and `ca` kinds render a Deployment:* `replicas: 1`, `strategy: Recreate` (decided above: no
   two pods of one identity), the selector the pods' `name` label, the pod `restartPolicy: Always`; the workload
   container `restartPolicy: Always` with the one rule, `RestartAllContainers` on every exit. The custody daemon, a
   native sidecar, restarts with the pod, empty, so the restarted pod's keys are new and `polar-init` buys their
   certificates again (the renewal rule's five-minute bound admits a rejuvenation). The Job fields leave these kinds.
2. *A kind for what runs to completion* (question 1): the load harness keeps its Jobs.
3. *The scripts:* the `job-name=` waits become `name=` waits (the label the pods already carry); `apply.nu` applies a
   Deployment in place, a changed one rolling out by `Recreate`; `deploy-objects.nu` forces only layers that still hold a
   Job, and checks a Deployment as Flux does natively (available replicas) rather than through a Service's Endpoints;
   `up.nu`'s Job restarts apply only to Jobs.
4. *A rule beside `job_replacement_problems`:* every Deployment the catalog renders has one replica, `Recreate`, and the
   restart rule on its workload container (`rules.nu`, checked by `render.nu`), so none can lose them unseen.
5. *The hand charts after them:* the `ace` job (`layers/2-services/ace/job.dhall`) as a Deployment, the same way. Neo4j
   and Postgres stay StatefulSets: Neo4j's server certificate lives a year (its identity's lifetime override), so it
   does not rejuvenate. The agent charts no target renders are not changed.
6. *What goes:* Failed pods at rejuvenation (an ending restarts the pod in place); the forced replacement of a changed
   Job; the Endpoints health check for agents. The Reaper is left for what is still replaced (a deleted, evicted or lost
   pod), as `pod-reaper.md` says.

### For the maintainer

1. *The load harness's Jobs.* A new kind, `run` (extending `agent`, keeping the Job fields and template), for the
   producer and the sink; or a field on an entry (`runs_to_completion: true`) choosing the Job within a kind.
   *Recommendation:* the kind. A kind already says a workload's shape (`agent`, `service`, `ca`), and what it renders as
   is shape.
2. *A pod killed by its liveness probe restarts only its workload container* (tried, above): no `polar-init`, the same
   certificates, the daemon keeping its keys. Today the same kill fails the pod, and the Job makes a new one. The probe
   is the fallback for a supervisor that no longer answers (the health actor decides the rest). *Recommendation:*
   accept it. The restarted process starts from the same valid certificates and daemon, and its health actor
   rejuvenates it, the whole pod then, when they near their end; a probe kill is not a renewal, and nothing is issued
   for it.
3. *Clusters older than 1.36,* where `RestartAllContainers` is not on by default: keep rendering Jobs for a target that
   says its cluster is older, or require 1.36 or later for every target. *Recommendation:* require 1.36 or later. Every
   cluster Polar runs on is 1.37, two paths would each need testing, and the Reaper's note on older clusters
   (`pod-reaper.md`) then reads as not supported.

### Tests first

- `infra/tests/rules.nu`: the Deployment rule (one replica, `Recreate`, the restart rule; each missing refused).
- `infra/tests/resolve.nu`: every catalog workload of `sandbox-local` and `local-minimal` renders a Deployment with
  them, the load harness a Job (by question 1's answer); the rendered pod keeps its placement and its sidecar.
- `infra/tests/charts.nu`: the `ace` Deployment.
- The scripts' pure parts (`deploy-objects.nu`'s health check and `has_job`, `apply.nu`'s verdict) in
  `infra/tests/*.nu`, for a Deployment and for a Job.
- Mutations: the rule's `exitCodes` narrowed to exit 1 (exit 0 would then restart the container alone); `RollingUpdate`
  in place of `Recreate`; the harness rendered as a Deployment.

### On the cluster

`up.nu sandbox-local --dev` over the running `u7c` (Flux applies the Deployments; the old Jobs go with their layers'
`prune`), then, on a fresh `u7c` too, since a first bring-up and an upgrade differ:
- every agent a Deployment's one pod, on its pool's node, and its custodian fetching as before;
- *an ending restarts in place:* an agent made to end (its health verdict, by `cassini-shutdown`'s path or by waiting
  out a rejuvenation) restarts with the same UID, its daemon empty and refilled, `polar-init` run again, a new
  certificate issued and the old serial revoked, and no Failed pod;
- the credentials agent restarted in place comes back through its successor window (`core-restarts.md`, step 4), its
  ledger opened once;
- a changed Deployment rolled out by `Recreate`, never two pods of one identity at once;
- nothing waits forever in `up.nu`, and the health checks read a restarting pod as they should (the item "To read when
  built", above).

