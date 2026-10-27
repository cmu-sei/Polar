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

# Runtime attestation

Status, 2026-10-26: the shape, decided with the maintainer. Nothing here is built. What Polar already had on the topic
(the original Attestation Service, cert-issuer's deferred "v2" items, the image attestation ladder) is in
`core-restarts.md`, question 1, "Runtime attestation: what Polar has on it".

## Why

The floor signs the core's own identities on a token alone, and the decided fix for the credentials agent's restart
(`core-restarts.md`, question 1: a handover ticket for rejuvenation, a narrow window for a crash) only decides *when*
that entry is open. Inside the window, any pod running as the service account could get there before the real one
(the token names the account and a pod UID, and nothing says which UID is the real successor). The same gap, smaller,
is in every certificate the credentials agent vouches for: it vouches on a verified token, not on the pod.

The maintainer (2026-10-26) wants both:
- **an instance check at issuance:** a certificate goes only to the instance the deployment made, running what the
  deployment names;
- **runtime attestation of the process:** the instance that was attested is still that, while it runs.

## Decisions (the maintainer, 2026-10-26)

- **One agent: the attestation agent.** It takes over the pod observer's role in `pod-reaper.md`, since both need the
  same rights over pods (get, list and watch, in Polar's namespaces): one agent rather than two holding the same
  permission. It sees and reports; it never acts. The Reaper stays a separate agent that only deletes, and stays for
  clusters older than the in-place restarts the Deployments below rely on.
- **Kubernetes stays inside it.** Everything Kubernetes-specific is a platform backend of the attestation agent. The
  rest of Polar speaks a platform-neutral protocol to it over Cassini ("is this instance genuine?", "this instance has
  lapsed"). A bare-metal backend (TPM, SPIRE) would replace the backend, not the protocol.
- **It is not in the core.** It is among the first things to start after the core.
- **A node sensor, read-only, on every node** (below): exec sessions and the process's own locks are verified from
  outside the process, not taken on its word.
- **The sensor is built with the attestation agent, and deployed apart from it.** One domain, as `kubernetes/` keeps
  `observe` and `consume`: `attestation/agent` (the attestor, one per cluster, holding the Kubernetes credential),
  `attestation/sensor` (one per node, a DaemonSet, host PID visibility only, no Kubernetes credential) and
  `attestation/common` (the protocol both speak over Cassini). Two identities, each with its own certificate. Why not
  one workload: the sensor must be one per node, since it sees only its own node's processes, and merging would either
  leave the attestor blind to other nodes or put the cluster credential on every node; and one compromise would then
  hold both a read view of every pod in the cluster and every process on a node. Each identity holds only what it needs:
  the Reaper's principle, applied here to two kinds of seeing rather than to seeing and acting. Only the attestation
  agent gives verdicts; the sensor gives evidence.
- **The trust base is written down, and the regress stops there.** The Kubernetes control plane (its signed tokens and
  its API), cert-issuer's CA key, the signed deployment (`provenance.json` and the image lock), the attestation agent,
  and the node sensor (nothing else sees its node). Everything else is attested against these. "Who attests the attestor" is not asked again; the maintainer:
  it is "like mixing your own silicon and baking your own semiconductors to solve the hardware supply chain issue".
  Two attestation agent instances attesting each other's successor is noted as optional hardening, not planned.

## The instance check at issuance

The request carries the pod UID its token is bound to (the instance binding, `instance_binding_claim`,
`cert-issuer/server/src/config.rs:93`). The attestation agent answers whether:
- the pod exists and has that UID;
- its owner is the workload the catalog says this identity runs as (its Job, or its Deployment's ReplicaSet);
- it runs as the expected service account;
- each container's `imageID` is the digest the signed deployment pins (the Kubernetes consumer's "attestation ladder":
  the kubelet's `imageID`, never the spec's tag, `kubernetes/consume/src/lib.rs`).

Who asks:
- **the credentials agent,** before it vouches for any workload;
- **cert-issuer,** for a token-only request in one of the core's windows (the ticket, the crash window, Cassini's
  route). The window decides when the entry is open; the attestation decides who gets through it.

**Bootstrap is attested after the fact.** The core's first certificates come from the floor before anything can attest
them. Every entry in cert-issuer's record carries its pod UID, so once the attestation agent is up it attests the
running core pods against the record, and reports any that fail. The same pattern as the record itself: the body guard
learns as soon as it can.

## Runtime attestation of the process

A builder actor in every agent, beside the health actor, answers the attestation agent's challenges on a schedule. The
rule that decides what counts: **what a process says about itself is worthless against an attacker running code inside
it**, who can read its memory, sign with its key and answer any challenge. So evidence comes from what the attacker does
not control, or the process submits to restrictions that are verified from outside. Self-measurement (hashing
`/proc/self/exe` from inside) is not evidence, and is dropped.

| What the attacker does | What stops it or shows it | Seen by |
|---|---|---|
| An impostor pod with a stolen token | The instance check: owner, service account, `imageID` against the signed deployment | the API |
| Copies the certificate key out, to use elsewhere | The private key lives in the kernel keyring, usable but not readable (as the escrow key already does, `CryptoActor`, `credentials-agent-architecture.md`); Cassini binds each session to the attested pod's address | the kernel, Cassini |
| Replaces the binary, or drops a new one | `readOnlyRootFilesystem` and digest pinning, verified in the pod spec | the API |
| `kubectl debug` | An ephemeral container in the pod's status | the API |
| `kubectl exec`, or a spawned shell | A process in the container's cgroup that is not the agent | the node sensor |
| Code injected into the live process | The builder locks the process before it parses any untrusted input, irreversibly: seccomp (no `execve`, `ptrace`, `memfd_create`, no new executable memory) and Landlock (no file access beyond what it opened); `Seccomp: 2` and `NoNewPrivs: 1` verified in `/proc/<pid>/status` | the node sensor |
| Uses the agent's rights for its own ends | Every refused publish and subscribe is a signal; Cedar and the topic gate already cap what it can do | Cassini |
| Stops answering, or answers late | A fresh challenge each round; a missed or stale answer lapses | the attestation agent |

**What remains:** an attacker who injects code into memory, spawns nothing, touches nothing outside the lock, and
behaves exactly as the agent does. Nothing above tells it from the agent. It cannot persist (read-only root, no writable
executable mount), cannot leave (the key cannot be extracted, the session is bound to the pod's address), and dies at the
next rejuvenation, within the hour. Only a hardware root closes that (a TPM quote over IMA, or confidential containers),
as a later platform backend; it is not available on a rootless node.

**A verdict is the attestation agent's; acting on it is the credentials agent's.** A lapsed or failed attestation is
published; the credentials agent, as body guard, stops renewing (the certificate is dead within the hour), and Cedar and
the broker may act sooner.

## The node sensor

`attestation/sensor`: one per node, a DaemonSet, read-only, with host PID visibility and no Kubernetes credential. For
each container of a Polar pod on its node it reports the processes in the container's cgroup, and for the agent's
process its `Seccomp` and `NoNewPrivs` state, to the attestation agent over Cassini. It acts on nothing and gives no
verdict. Its own pod is attested through the API like any other; what it reports of its own node is taken as part of
the trust base. It is Linux-specific, not Kubernetes-specific; on another platform its backend changes with the
attestation agent's.

## Deployments, next (the maintainer, 2026-10-26)

The maintainer plans to make the agents Deployments again instead of Jobs, now that Kubernetes 1.36 and later can run a
pod's init containers again when its workload container restarts, in place. A pod restarted in place keeps its UID
(`polar-init`, `898120b9`). So a rejuvenating credentials agent's successor is the same pod: the handover ticket can name
its UID, and the race in `core-restarts.md`, question 1, is left only for a pod that is really replaced (deleted,
evicted, or lost with its node). With in-place restarts the Reaper is mostly obsolete; it stays for older clusters. To be
taken up after this document.

**Checked (2026-10-27).** The feature is KEP-5532, "restart all containers on container exits": a container restart
rule whose action is `RestartAllContainers` restarts the whole pod in place, init containers (which must be reentrant)
and sidecars included, keeping its sandbox, UID, network namespace, IP and every volume, `emptyDir` included; restart
counts rise, and the pod carries an `AllContainersRestarting` condition while it restarts. Alpha in 1.35 (gate
`RestartAllContainersOnContainerExits`, on the API server and the kubelet), beta and on by default in 1.36, on top of
container restart rules (KEP-5307, beta in 1.35). On sandbox-local (v1.37.1) the API server accepted the action although
`kubectl explain` still describes `Restart` as the only one, and a pod (`restartPolicy: Always`, its container
`restartPolicy: Always` with a rule `RestartAllContainers` on exit code 1) whose workload exited every 8 s ran its init
container again each time (four runs, appended to one `emptyDir` file), under one UID and one IP. What it means here,
to be settled before the move:
- *The renewal rule meets the same UID.* The credentials agent refuses a second certificate of a type to a pod
  instance while the first is valid (`AlreadyHasValidCertificate`), and so does the held-request voucher; a pod restarted
  in place is the same instance, asking again before its certificate expires, which is why it rejuvenates. That rule is
  what makes rejuvenation need a new pod today, so changing it is a decision, not a detail.
- *Kept bundles.* The `emptyDir` survives, so `polar-init` finds the last bundle (`keeps_existing_bundle`) and would keep
  it while it is valid, which at rejuvenation it still is.
- *Rollouts.* A Deployment's rolling update starts the new pod before the old one ends; a credentials agent (one ledger)
  wants `Recreate`, and the ledger lock is the backstop either way.
- *Init containers must be reentrant*: `polar-init` and the escrow bootstrap, run again in the same pod.

The plan for the move, with these settled one at a time: `deployments.md`.

## Open, to be settled when it is built

- The two workloads' names in the catalog, their capabilities (`Capability` in `polar-agent-builder`), the
  attestation agent's Role (which `pod-reaper.md` already describes for the pod observer), and what the sensor's pod
  spec needs beyond `hostPID` (a read-only view of the host's cgroup tree; whether reading another user's
  `/proc/<pid>/status` needs any capability on the node's kernel).
- The protocol's topics and messages, and how often an agent is challenged.
- How cert-issuer hears the attestation agent while Cassini is down (Cassini's own restart): a direct mTLS path, with
  `core-restarts.md`, question 2.
- The mTLS key in the kernel keyring: today `polar-init` writes `key.pem` to the shared `emptyDir`; the escrow key's
  handle (`POLAR_ESCROW_KEY_HANDLE`) is the precedent.
- Cassini's binding of a session to the attested pod's address.
- The seccomp and Landlock profiles the builder applies, and which agents need more (the nushell agents run other
  programs).
