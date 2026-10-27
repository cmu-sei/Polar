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

Status, 2026-10-27: cleared by the maintainer, its five questions decided as written below. Nothing here is built.

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
- **The Dhall types.** The schema imports dhall-kubernetes 1.31 (`infra/schema/kubernetes.dhall`), whose containers
  have no restart rules; dhall-kubernetes goes to 1.35, which has them (beta in 1.35; the action is text, so
  `RestartAllContainers` fits). Moving the schema to 1.35 is a step of its own, every chart and the catalog rendered
  before and after.
- **Init containers again in one pod.** `polar-init` writes its bundle atomically, file by file (`output.rs`,
  `write_bundle`), so a second run replaces the first; the escrow key loaded again displaces the earlier one in the
  keyring. Checked once more when built.
- **What goes.** The forced replacement of a changed Job (`infra/deploy-objects.nu:138`), since a Deployment changes in
  place; the digest check that restarts a Job by hand (`remaining-work.md`, 2026-10-14); Failed pods, so the Reaper
  stays only for clusters older than 1.36.
- **What does not change.** The health machine and the shutdown protocol: an agent still ends by exiting, and the pod
  restarts around it.
- **To read when built.** Whether in-place restarts back off like a crash-looping container (none was seen in the
  test, restarts every 8 s), and how a pod restarting in place shows in `up.nu`'s waits and the health checks.

## Build order, each tests first

1. The custody daemon (`key-custody.md`), which item 4 puts first.
2. The renewal rule, in issuance and in the voucher, with the bound (items 1 and 2).
3. The Dhall schema at 1.35.
4. The `agent` and `service` kinds as Deployments with the restart rule and `Recreate`; the hand charts after them.
5. On a fresh sandbox-local: every agent restarts in place at rejuvenation, keeps its UID, is issued a new certificate,
   and leaves no Failed pod; the credentials agent's successor comes through its window; nothing runs twice.
6. The ticket (`core-restarts.md`, step 5).
