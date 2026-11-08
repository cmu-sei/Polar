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

Status: DESIGN CLEARED by the maintainer 2026-11-01, every question below answered as recommended ("All as
recommended"); not built, and to be built after custody part B (the credentials agent's own key in its daemon). Arose from the multi-instance threat model (`authority-model.md`, "Multi-instance
workloads").

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

## Design (draft for the maintainer's review, 2026-11-01)

Every choice here is a PROPOSAL (**P**) until cleared; the questions are at the end. Paths are relative to
`src/agents/`. Nothing is built.

**Preconditions.** Part B (`key-custody.md`, "Part B"): the credentials agent's key in its daemon, split into a signing
key and an HPKE P-256 decryption key ("The key type"); today it is an RSA file the workload reads
(`credentials-agent/consume/src/escrow.rs:50`, `:104`), so no rotation of it could keep plaintext out of the workload.
The reason field (being built in parallel). Pulled health (`remaining-work.md`, "HEALTH IN THE GRAPH, PULLED"; not built).

### The shape: rotation re-wraps 32 bytes, never a payload

An `EscrowedSecret` is AES-256-GCM over the payload, its 32-byte key wrapped to the holder (`polar-escrow/src/lib.rs:164`:
`ciphertext`, `nonce`, `encrypted_key`, `encrypted_to_pem`; the binding context is the payload's AAD). **P1:** rotating
an entry = opening its wrapped key under the old key and sealing the same 32 bytes to the new key, both in the daemon.
The payload ciphertext, nonce and binding stay byte for byte; the daemon never sees a payload (as `Unwrap` today,
`polar-custody/src/custody.rs:255`), and no rotation shows the workload the 32 bytes (only `Unwrap` does, as today).

### Trigger: a compromise, recorded with its cause

- *Where the reason is.* The ledger already records it: `CredentialsEvent::Revoke { serial, reason, .. }`
  (`credentials-agent/common/src/events.rs:227`), `RevocationReason::Compromised` (`:59`) against the ordinary ends
  (`Superseded`, synthetic, `:67`; `Decommissioned` and the rest), folded in `state.rs:573`. The published set is
  serials only (`AuthorityBody::Revoked { serials }`, `authority_update.rs:91`); the parallel build adds the reason
  there and at the broker. **P2:** rotation needs neither: it starts in the ledger, the only writer, so the published
  reason is for the leaves and the graph, not a trigger.
- *Who declares.* An administrator: the existing signed `AdminAction::Revoke { serial, reason: Compromised }`
  (`admin_action.rs:56`), for any serial the ledger knows, live or ended (a compromise found late still counts); no new
  command (**P3**). The attestation agent (not built; `runtime-attestation.md`, slices A and B): **P4:** any
  `NotGenuine` verdict on a running pod whose daemon holds an escrow key is, in the credentials agent, a `Revoke` with
  `Compromised` of that pod's serials, recorded with the verdict's correlation id (a pod not genuine cannot vouch for
  its daemon).
- *What starts.* **P5:** right after appending a `Compromised` revoke of a serial of workload X's pod (X has an escrow
  key, `agent-escrow-keypair:X`), the ledger appends `RotationStarted { rotation, holder: X, key: <fp of the old key>,
  cause: <hash of the revoke's log entry> }`. Replay finding a `Compromised` revoke with no start after it appends the
  start (idempotent). A rotation already open for that key absorbs further compromises. A compromise of the credentials
  agent's own pod starts its key's rotation and, since every escrowed key was sealed to it, one for every X (**P6**).
  Key ids are SHA-256 of the SPKI DER, as `authority_update::fingerprint` (`authority_update.rs:207`).

### The declared store (a typed builder API)

**P7:** the domain hands the builder a `SealedStore`, as it asks for `.with_encryption()` (`polar-agent-builder/src/
lib.rs:323`); the builder prints its name in the scaffold (`sealed_stores`), computes the capability `sealed-store`
(implying publish `polar-rotation-reports`), and the catalog carries it, so the credentials agent knows which stores to
wait for. Nothing outside reads the domain: the actors see what the trait returns.

```rust
#[async_trait]
pub trait SealedStore: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Up to `limit` entries whose wrap is sealed to `key`, ordered by id, after `after`. Never a payload.
    async fn sealed_to(&self, key: &KeyId, after: Option<EntryId>, limit: usize) -> Result<Vec<Sealed>, StoreError>;
    /// Replace entry `id`'s wrap `from` with `to`, only if `from` is still its wrap; durable before it returns.
    async fn write_back(&self, id: EntryId, from: WrapId, to: Wrapped) -> Result<WriteBack, StoreError>;
    /// How many entries are sealed to each key: the pass's progress, for health and the report.
    async fn census(&self) -> Result<BTreeMap<KeyId, u64>, StoreError>;
}
pub struct Sealed { pub id: EntryId, pub wrap: Wrapped }
pub struct Wrapped { pub key: KeyId, pub bytes: Vec<u8> }   // HPKE encapsulation + the sealed 32 bytes
pub enum WriteBack { Written, Moved }                        // Moved: not `from` any more; someone resealed it
```

A store is the WORKLOAD's and durable (a PVC); a per-instance store dies with its pod, which a rotation restarts, and
is not declared (**P8**).

### The daemon's `Rewrap`

**P9:** one request, the workload's alone: `Request::Rewrap { wrapped, from: KeyId }` beside `Unwrap`
(`polar-custody/src/protocol.rs:70`), answered `Reply::Rewrapped(Wrapped)`.
- *Holds:* `retiring: Option<(KeyId, EscrowKey)>` beside `escrow` (`custody.rs:81`). It opens `wrapped` under the
  retiring key named `from`, seals the 32 bytes to the daemon's CURRENT key (`escrow`, or the credentials agent's
  decryption slot), zeroizes them, and returns the wrap. It never seals to a key the caller names, so it is never an
  export.
- *The retiring key only rewraps:* `Unwrap` opens under the current key alone (unchanged), so the retiring key never
  hands the workload 32 bytes directly (**P10**).
- *Refusals:* `NotPermitted` (the installer, or no workload role); `NoKey` (no retiring key `from`, or no current key);
  `BadRequest` (does not open under it); `Revoked` (as every request); new `Retired` (that key was destroyed: the
  rotation completed; the entry is lost and asking again changes nothing).
- *Logs* (never bytes, never a wrap): the first rewrap of a rotation (`from` and current key ids), a count every 1,000
  and at the end, each refused open at `warn` with the key id, and the retiring key's destruction with the authority
  sequence that retired it.
- *Where the retiring key comes from:* for X, the custodian's one escrow retrieval returns the current key and, while a
  rotation is open and X declares a store, the retiring one, within the spent window (`key-custody.md`, "Part A's
  bootstrap", item 3); for the credentials agent, a second file on its daemon's volume (part B). `Custody::revoke`
  (`custody.rs:135`) destroys it with the rest; the credentials agent's files outlive the pod, as part B has it.

### The `RotationActor`: a resumable, idempotent pass

**P11:** a builder actor under the InfrastructureSupervisor beside the CryptoActor and StateActor
(`polar-agent-builder/src/state.rs:364`), started for an agent with `sealed-store`. It learns of a rotation from the
signed authority (below), already held by every builder agent.
- *The pass:* for each store, `sealed_to(old, after, 64)`, `Rewrap` each, `write_back`; `Moved` is skipped; again until
  the page is empty. Then `census()` must show zero under the old key; then it publishes `RotationReport { rotation,
  store, remaining: 0 }` (broker-stamped).
- *Progress is the store itself, not self-state* (**P12**): an entry is done exactly when its wrap is sealed to the new
  key, so `sealed_to(old)` is what is left. Soft state is an unsigned hint that may be lost (`soft-state.md`,
  "Revised"); the store cannot disagree with itself.
- *A crash mid-pass:* the restarted actor lists again; finished entries are not listed; a rewrap whose write did not
  land is done again (HPKE is randomized, a new wrap, harmless); a write racing another writer is `Moved`. No step
  needs to know where the last one stopped.
- *Plaintext:* the actor holds ids and wraps, ciphertext under a key it does not have; the 32 bytes exist only inside
  `Rewrap`.

### The old key's life, and completion

- *During:* held only as `retiring`, in the daemons that need it; it rewraps and nothing else.
- *Who tracks:* the credentials agent. It records each report from the store's own workload (as the catalog names it)
  as `RotationStoreDone { rotation, workload, store }`; its own store it checks in process. When every declared store
  holding the key is done: `RotationCompleted { rotation }`.
- *Signed and observable* (**P13**): a new state-based authority kind, `AuthorityBody::Keys { retiring: [KeyId],
  retired: [KeyId] }`, versioned and caught up by snapshot exactly as `Revoked` (`authority-distribution.md`,
  "Snapshots", "Pruning the revoked set"), verified against the pinned key by every leaf and daemon
  (`polar-custody/daemon/src/watch.rs:95`, `Watch::hear`). Destroying keys is security, so it is authority, not a
  runtime section. `retiring` tells the credentials agent's daemon to make its successor and the RotationActors to run;
  `retired` makes every daemon destroy that key (zeroed, the credentials agent's file deleted), logged.
- *Everywhere:* daemons on `retired`; the escrowed copy of an old `agent-escrow-keypair:X` by shredding its wrap (next).

### The credentials agent's key, the first user

*Its store is the ledger.* Every secret is a `SecretStored` event carrying the whole `EscrowedSecret`
(`events.rs:287`), folded into `stored_secrets` (`state.rs:179`, `:620`); the log is hash-chained over each event's
exact bytes (`lib/src/event_log.rs:121`, verified at `:171`, `:183`) and appended with fsync (`:273`).

*History is not rewritten* (**P14**): a rotated entry is a new event, `SecretResealed { name, rotation, from: WrapId,
to: WrapId }`, appended through `append_and_project` (`ledger.rs:571`); its fold replaces the current wrap. Every
`SecretStored` stays as written, the chain verifies as it does today, and the trail says what moved, when, and which
rotation (whose cause names the revoke) moved it.

*But an old wrap in the chain outlives its key's destruction:* whoever holds the compromised key and later reads the
log (a backup, a read-only bug) opens every secret it ever held. **P15:** wraps leave the chained event. `SecretStored`
carries the payload ciphertext and a `WrapId` (key id and SHA-256 of the wrap); the wraps sit beside the log on the same
volume, one file per hash, written and fsynced before the event that names them. At completion the wraps under the
retired key are deleted and `WrapsShredded { rotation, key, hashes }` appended. The chain commits to every wrap and
still verifies; a missing wrap of a CURRENT entry stops replay as loudly as a broken chain. Deleted, not scrubbed: the
medium is the trust base's. The doc comment "nothing ... is ever irrecoverable" (`events.rs:284`) then reads "until a
rotation shreds it". Shipped with a fresh install, as HPKE was ("No migration", `key-custody.md`).

*In-flight relays.* The relays open a stored secret and seal it to the requester (`retrieve_secret.rs:177`, `:186`;
`decrypt_on_behalf_of.rs`; `agent_escrow_keypair_bootstrap.rs`). **P16:** one ledger operation, `open_current(name)`:
an entry still under the old key is rewrapped first (rotate-on-read, its `SecretResealed` appended), then unwrapped
under the new key. The ledger actor is the single writer, so a relay and the pass never both win (`Moved`). A store
sealed to the retiring key (a sender that cached it; `store_secret.rs:184` refuses it `WrongEscrowKey` today, and the
CryptoActor does not retry a refusal, `polar-crypto/src/lib.rs:991`) is accepted and rewrapped before it is appended
(**P17**); the escrow-key query answers the new key from `RotationKeyReady` on.

*Order of steps* (**P18**):
1. The `Compromised` revoke, then `RotationStarted`; the revoked pod's daemon destroys what it holds in memory and
   exits (decided item 1, `key-custody.md`); its files stay.
2. The new pod's credentials agent replays, sees the rotation open, publishes `Keys { retiring: [old] }`.
3. Its daemon, hearing it, makes the successor (`next` file, fsynced, then renamed: current to retiring, next to
   current; a crash between is recognized at start); the workload reads the new public key and appends
   `RotationKeyReady { rotation, successor }`.
4. The pass over the ledger, relays rotating on read; `RotationStarted` for every X (P6), each run as below.
5. Census zero, `RotationCompleted`, wraps shredded, `Keys { retired: [old] }`; the daemon deletes the retiring file.

*The signing half is not this:* the same daemon compromise exposes it, and a leaf never replaces its pinned key
(`authority-distribution.md`, "Pinning the key to the CA", item 3). Its successor is a re-pin, every pod restarting into
init (question 6).

### A workload's key, and document-carried secrets

*Order* (**P19**): (1) `RotationStarted` for X; X's escrow certificate is revoked `Compromised`, so the lookup stops
naming the exposed key. (2) Every instance of X is revoked as an ordinary end (`Superseded`), since a custodian's
authority is spent and a running daemon cannot fetch again; each pod restarts. (3) The first new custodian finds the
name open for rotation, makes a P-256 key, has it issued, and stores it under `agent-escrow-keypair:X`. Two "once"
rules admit one replacement per open rotation, serialized per identity as the multi-instance first issuance is: one
service certificate per identity (`agent_escrow_issuance.rs:111`; today a revoked one still counts, since the `Revoke`
fold, `state.rs:573`, never touches `agent_escrow_certificates`, `:869`) and the name never replaced
(`store_secret.rs:173`, `NameTaken`). Then `RotationKeyReady`. (4) X's RotationActor passes over X's declared stores
(none today), reports. (5) Completion: `retired`, and the old copy's wrap under the credentials agent's key shredded.

*Re-delivery* (**P20**). git-repo-observer re-applies its signed `credentials` section on every start
(`git/observe/src/supervisor.rs:99`, `credentials::apply`, `credentials.rs:246`), each entry opened by
`RegisterSecret` (`polar-crypto/src/lib.rs:206`). An entry sealed to a key other than the daemon's current one is not
opened: the CryptoActor asks `LoadSecret` (`:218`, `load_secret`, `:1008`) for `<identity>/<name>`, which the
credentials agent seals to the NEW key on retrieval. It is accepted only if the escrowed record says it was opened
under the same binding context as the entry (`credentials.rs:223`, agent, section, version, name): the escrow form
(`polar-crypto/src/registry.rs:238`) gains that context. An entry never received before the rotation is reported by
name ("sealed to a retired key: seal it again", `Applied.failed`); the author seals it to the new escrow certificate
(`polar-sign seal`) and signs a new version. No machine rewrites the document.

### Observability

- *Pulled health* (**P21**): `HealthState` (`polar-health/src/lib.rs:166`) gains `rotation: Option<{ rotation, key,
  stores: [{ name, to_rotate, done, failed }] }>`, from the RotationActor's census, so the state agent writes it into
  the graph with the rest; introspection from an infrastructure actor, counts and key ids only. Until pulled health is
  built, the health file carries it.
- *Events* (the ledger, and its graph projection, `projection.rs`, which projects nothing of a `SecretStored`, `:308`):
  `RotationStarted` (cause), `RotationKeyReady`, `RotationStoreDone`, `RotationCompleted`, `WrapsShredded`; and the
  daemon's log lines above.

### The `BackupActor` (scope only)

Over the same `SealedStore`s, a sibling builder actor that exports entries as they are sealed (payload ciphertext and
wrap, never opened) and restores through `write_back`. Not designed here: where a backup goes, how often, who may
restore. One rule follows from rotation: a backup made before a completion holds wraps under the retired key, and
restoring it gives entries that answer `Retired`; a backup never brings a retired key back.

### Threat check, against the custodian's guarantees (`key-custody.md`, "What it protects")

| Threat | With rotation |
|---|---|
| A compromised workload copies a key | still not possible: `Rewrap` returns a wrap sealed to the daemon's own current key, never a key or a caller-named recipient |
| It uses a key while it runs | `Rewrap(old)` then `Unwrap(new)` equals `Unwrap(old)`: the oracle its identity had over the old key, no wider; it ends at `retired`; a retiring key is fetched only where a store needs it |
| The RotationActor or health | ids, key ids, counts and ciphertext; never the 32 bytes or a payload |
| Another pod, same UID; a killed pod; root on the node | unchanged; the credentials agent's retiring file is on its daemon's volume, deleted at completion |
| Revocation during a rotation | destroys current and retiring keys alike (`Custody::revoke`) |
| A forged `Keys` update | refused by the pinned key; only the credentials agent retires a key |
| A compromised credentials agent | can start a rotation or report completion early: loss of what is not yet rewrapped, a denial of service as its ledger already allows, never disclosure |
| A compromised custodian | its authority is still spent: a rotation reaches new keys by restarting pods, never by fetching again |

### Questions for the maintainer (each with the recommendation)

1. Wraps out of the hash-chained event, in shreddable files the chain commits to (P15), or old wraps kept in the log for good? Recommended: out of the chain.
2. A workload key's rotation restarts every instance (ordinary-end revocation) rather than give a running custodian new authority (P19)? Recommended: restart.
3. The retiring key answers `Rewrap` only, never `Unwrap` (P10)? Recommended: yes.
4. A rotation's start and the old key's destruction carried as a new signed authority kind, `Keys { retiring, retired }` (P13)? Recommended: yes.
5. Any attestation `NotGenuine` of a pod whose daemon holds an escrow key counts as `Compromised` (P4)? Recommended: yes.
6. The credentials agent's signing half after a daemon compromise: a CA-certified successor and every pod re-pinning by restart, designed on its own? Recommended: yes, separately, before the first real use.
7. A compromise of the credentials agent's key starts a rotation of every escrowed workload key (P6)? Recommended: yes.
8. Stores arriving sealed to the retiring credentials-agent key accepted and rewrapped on arrival (P17)? Recommended: yes.
9. Re-delivery only for the same binding context, the escrow form recording it (P20)? Recommended: yes.
10. Declared stores are the workload's and durable; per-instance stores are not declared (P8)? Recommended: yes.
11. No new admin command: a signed `Revoke` with `Compromised`, of any known serial, is the declaration (P3)? Recommended: yes.
12. Build after part B, the credentials agent's key in its daemon and split (preconditions)? Recommended: yes.
