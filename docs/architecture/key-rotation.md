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

# Key rotation

Status: DIRECTION DECIDED by the maintainer 2026-11-01; the design is to be written (below, "To design") and cleared
before anything is built. Arose from the multi-instance threat model (`authority-model.md`, "Multi-instance workloads").

## Why

Revocation is immediate: the moment Polar says a certificate is revoked, its daemon destroys the keys and the broker ends
its sessions (`key-custody.md`). Rotation is something else: it is about DATA RETENTION, taking what is encrypted to one
key and encrypting it to another, whatever that key's revocation status. The maintainer: done by hand or offline, it would
live "alongside a live, distributed zero trust architecture"; it belongs inside it, and it must be observable that rotation
is happening.

## Decided (the maintainer, 2026-11-01)

- **Revocation records why**: a compromise (of a key, a daemon) against an ordinary end (a pod ending, a certificate
  replaced). Only a compromise starts a rotation.
- **Who declares a compromise**: either the attestation agent's verdict or an administrator's signed command.
- **Rotation is a builder facility, not an agent**: a `RotationActor` acting over an agent's OWN data, a service the
  builder provides to every agent "like running backups probably should be" (a `BackupActor` is its sibling, over the same
  declared stores). The domain declares its encrypted stores through a typed API (how to list entries, what each is
  sealed to, how to write one back); nothing looks into it (AGENTS.md, "Design Principles").
- **It is observable**: progress (to rotate, done, failed) and its start and end reach the graph as infrastructure
  introspection (pulled health), and the end is an event.

## What is at rest, and to which key (read 2026-11-01)

- The credentials agent's store and event log: every escrowed secret, sealed to the CREDENTIALS AGENT's own escrow key
  (`credentials-agent-architecture.md`, "Secret escrow": "no secret is ever at rest anywhere in this system ... except
  encrypted to a specific holder's own key"). A relay to a consumer is transient: open, re-seal to the consumer, send, wipe.
- Signed documents carrying secrets: git-repo-observer's `credentials` runtime section, its entries sealed to the
  OBSERVER's escrow key, in polar-schedules (`git/observe/src/credentials.rs`); on receipt the observer opens each through
  its custody daemon and escrows it into the credentials agent.

So: rotating the credentials agent's key re-encrypts its whole store (the first real user of a `RotationActor`); rotating
a workload's key re-encrypts almost nothing, since a workload holds nothing at rest of its own. A signed document is NOT
rewritten by a machine (its author signed it): after its first receipt the agent gets those secrets back from the
credentials agent, which holds them under its own key and seals them to the agent's NEW key; the document stays the record
of what was given.

## To design (for the maintainer's review before building)

- The typed API by which a domain declares an encrypted store, and the actor's resumable, idempotent pass over it
  (plaintext only inside the custody daemon's unwrap-and-reseal; never in the workload).
- The old key during a rotation: held by the daemon for unwrapping only, and destroyed everywhere once every store
  declared against it reports done; the credentials agent tracks completion.
- How revocation carries its reason (the ledger's revocation event, the signed authority update's revoked set, the
  broker's view), and how a compromise starts a rotation.
- The credentials agent's own key: its store as the first user; the order against in-flight relays.
- Re-delivery of document-carried secrets after a workload's key rotates (the git credentials rule above).
- The `BackupActor` over the same declared stores (scope only).
