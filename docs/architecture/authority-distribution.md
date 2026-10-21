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

# Authority distribution: policy and permissions pushed to the leaves

Status, 2026-10-12: design, decided; no question is open. Built so far: the three-valued decision in
`polar-policy`, groups and principals in `polar-permissions`, the `c:update-groups` capability, and in
`credentials_agent_common` the `AuthorityUpdate` message with its signing and verification (`authority_update`) and
the state a holder keeps (`authority_state`). The builder now hosts the actors and keeps them current (below). Since then the credentials agent publishes and answers snapshot requests (`authority_publisher.rs`) and its `policy` and `groups` sections feed it (`authority_sections.rs`); the aging and the revision notice below are not built. It supersedes the placement questions in `credentials-policy.md` and the "policy for other
receivers" note in `git-controller.md`.

## The principle (2026-10-12)

The credentials agent is the **ultimate decision authority** for policy and permissions, and it is designed to
**push decisions out** to be consumed at the endpoints. It is not the runtime extension of the agents that need
them. Every agent evaluates, locally, whatever concerns it: that is the point of a distributed system, and a
decision that does not change should never cost a round trip to the credentials agent. The early diagrams
(`sequences/general/policy_update_enforcement.mmd`) already had a policy update event delivered to the leaves.

So: policy, permissions and configuration are delegated to the leaf nodes. Each leaf hosts the evaluator. When an
authoritative update is accepted, an **event is raised** to every consumer of it, so the effect is immediate.

## Decided

1. **Groups are principal data, not rules** (option A). They live with `PermissionsActor`, the sole authoritative
   source of what a principal holds, as another signed input of its own. The policy document carries rules only.
   `polar-policy`'s invariant stands: a principal's attributes come only from `PermissionsActor`, and no caller
   supplies them. Who may assign people to groups can therefore be a different authority from who may write rules.
2. **Every receiver hosts the evaluator** (option i). The builder gains a `PermissionsActor` and a `PolicyActor`,
   next to the `ConfigurationActor` and `CommandActor` it already has. Policy rules and principal data are
   delivered to every agent, and each agent decides for itself whether a signer may change it or run a command on it.
3. **An event signal for updates.** When the credentials agent accepts a policy or permissions update, it raises an
   event for the consumers, handled everywhere the update needs to be received.
4. **The credentials agent signs what it publishes, with its escrow key.** Every agent can already get its public
   key from the credentials agent (the escrow key query, which the crypto actor fetches and caches), and it is
   sufficient for the credentials agent to sign its own unique work. Leaves pin that key and verify the signature
   on each `AuthorityUpdate`, so an update is believed because of who signed it and not because of who relayed it,
   and it can be stored and replayed. This covers ledger-originated changes (a revocation), which have no
   author's document to forward. **One key, not two** (2026-10-12): this key is already the identity whose
   loss is catastrophic and whose custody and recovery are designed accordingly; a second key would add a second
   thing to persist and recover for no gain, since the argument for separating them (a different rotation) does
   not apply to keys that are not rotated, and a leaked private key ends the model whichever uses it. The
   signature is RSASSA-PSS with SHA-256 over the payload prefixed with a fixed domain line
   (`polar-authority-update-v1`), so that nothing signed for another purpose can be replayed as an update and the
   decryption (OAEP) and signing (PSS) uses are kept apart in the message. The update carries the fingerprint of the
   key that signed it, so a key that ever changed would be noticed.
5. **Cedar overrides the certificate; the certificate is the fallback.** Once a policy is loaded, a request goes to
   the policy first. A `permit` admits a signer whatever the certificate says, and a `forbid` refuses one
   whatever it says. If the policy does not speak to the request (no rule mentions it), the capabilities in the
   certificate decide, as they did before there was a policy. This is the point of runtime configuration: it
   overrides the static defaults. **Built**: `polar_policy::Decision` is now `Allow`, `Deny` or `NotDetermined`,
   separating "a rule forbids this" from "no rule applies", and a rule that cannot be evaluated is a `Deny`, never
   a way through.
6. **`c:update-groups`** is a capability of its own, authoring group membership: not `c:grant` (which gives an
   identity permissions) and not `c:update-policy` (which writes the rules). **Built.**
7. **Policy and groups are authored by capability alone.** The `policy` and `groups` documents are checked
   against `c:update-policy` and `c:update-groups` and nothing else, always. Cedar never governs its own
   authorship, which avoids a mistaken `forbid` locking out every author and a rule admitting someone to write
   rules. A policy author is demoted by revoking their certificate, which the update signal makes immediate at
   the leaves. (2026-10-12.)
8. **Cadence.** The credentials agent republishes the current policy and principal data every half hour, or when
   something that must be known changes, whichever comes first, and the half-hour timer restarts whenever an update
   is pushed. Changes set the cadence, and agents rejuvenate often enough that a quiet cluster repairs itself
   at registration.

## Aging, and being told to catch up (2026-10-13)

**Status: decided, not built.** What an agent holds is a cache of the credentials agent's authority, and the cache
has a lifetime.

- **The cache is ephemeral state of the running pod, and that is right.** A pod cannot start without being able to
  initialize its state (its certificates come from the credentials agent), so a cluster that has lost the authority is
  on a ticking clock, and the end of the clock is every pod dropping back to its init container to wait. That is the
  intended way to tell an administrator that the cluster is dying, in place of an agent running in a degraded state
  for an unspecified time. There is no persistence of the cache.
- **Valid until aged out or evicted.** An update is valid until the agent has gone longer than a maximum age
  without hearing from the authority, or until it is told to catch up and cannot.
- **What counts as hearing from it:** any verified message from the credentials agent: an update, a snapshot (the
  half-hourly republish is the heartbeat), or the notice below. The clock restarts on each, not on each change.
- **Defaults, chosen to be sensible and tunable** (`POLAR_AUTHORITY_MAX_AGE_SECS`, declared in the builder's
  environment section):
  - republish stays at 30 minutes (decision 8);
  - an agent that has not heard for 45 minutes (one and a half periods) asks for a snapshot, as it does now when it
    has no key;
  - the cache expires at **90 minutes** (three periods: two republishes missed, plus margin).
- **On expiry the agent fails its health check** with a reason that names the cause ("no authority update heard from
  the credentials agent for 90 minutes"), which takes the existing shutdown pipeline to a restart into init. Until
  then its decisions stand; from expiry they fail closed. The credentials agent is exempt (it is the authority; its
  clock restarts at each of its own publications). An agent that has never verified a snapshot is not healthy: the
  credentials agent always publishes one (the key, even before any policy exists), so this does not wait on a policy
  being written.
- **Forced eviction is a notice, not a new kind of policy.** When a significant event needs every agent at a given
  revision and a plain "here is the new policy" may not be enough, the credentials agent publishes a signed
  **revision notice**: the sequence agents must have reached, and a deadline in seconds. An agent behind it asks
  for a snapshot at once; if it has not reached that sequence by the deadline it fails its health check as above and
  restarts. An agent already at or past it needs do nothing; the notice also counts as hearing from the authority.
  The update that carries the change (a higher-version policy, which can deny everything) is published as usual; the
  notice is what makes catching up mandatory.
- **A mismatch with the current code.** Today `AuthorityState` only compares versions (stale means "not higher"); it
  has no clock, no expiry and no deadline, and the builder creates the authority holder only for Cassini clients.
  Both would change. Tests first: an injected clock (the publisher's tests already use paused time).

## As built, and what "degraded" means today (read from the code, 2026-10-21)

- **No code tells "degraded" from "no policy".** `PolicyLookup` (`polar-agent-builder/src/authority.rs:443-537`) answers
  `Ok(Decision)`, `Err(NoPolicyLoaded)` (the actor runs with no rules) or `Err(Unavailable)` (the local actor is missing
  or did not answer). Nothing notices that the credentials agent itself is unreachable, and a policy once loaded stays
  in force with no age: the aging above (`POLAR_AUTHORITY_MAX_AGE_SECS`, the revision notice) is not built.
- **The fallback, where it exists** (`policy_authority.rs:67-113`, `admit`): no policy actor, `NotDetermined` or
  `NoPolicyLoaded` go to the certificate's capability; `Allow` admits; `Deny` refuses; any other error refuses. It is used
  for `update-configuration`, `update-runtime` and `run-command` in every builder agent, except a runtime section
  declared `SectionAuthority::Capability`, which skips Cedar (the credentials agent's `policy` and `groups`).
- **The authority holder exists only for Cassini clients** (`infrastructure.rs:614-633`). The broker (`ServiceBuilder`,
  `CASSINI_CLIENT=false`), cert-issuer and a standalone polar-git-server have none.
- **Settled 2026-10-21: degraded is bounded.** The certificate decides while the authority is away, for a bounded time,
  then the agent restarts into init (`policy-enforcement.md`, "Decisions"). The aging above stands. Before that:
- **The docs disagreed on degraded operation.** This file says a cluster that has lost the authority ages its caches
  out and restarts its pods into init, "in place of an agent running in a degraded state". `cert-issuer-isolation.md`
  (128-135) describes a degraded state that keeps running on the static floor while the credentials agent is away, as a
  domain member uses its cached credentials. To be settled: see `policy-enforcement.md`.

## Where things are decided

There are two evaluation sites, running the same machinery:

| | Decides | On what | Resource data comes from |
|---|---|---|---|
| **The credentials agent** | who may have a secret it holds; admin actions | its ledger | the ledger (`owner`, labels, a freeze) |
| **Every other agent** (the builder) | may this signer change a section of mine, change my configuration, or run this command on me | what the agent was sent | the agent itself: its own sections, variables and commands are the resources, built from its declarations |

The second needs no external resource data: the receiving agent knows its own `Section::"git-repo-controller/repositories"`.
The first has resource data that the ledger owns, and a change to it (a secret frozen) is a ledger event raised by
an admin action, not an annotation in a document. It matters only where the secrets are, at the credentials agent.

## What is pushed

Three kinds of authoritative data, each replaced whole and versioned:

- **Policy**: the Cedar rules. Authored with `c:update-policy` (exists). A `policy` document.
- **Principal data**: group membership and supplemental permissions. A `permissions` document. The capability to
  author it is open (question 3).
- **The schema**: carried in the code of the agent that evaluates (the builder for the shared parts: the action
  and resource types that every agent has), so an author cannot change what a type is.

### The signal

**The leaf's side is built (2026-10-12), in `polar-agent-builder`.** Every Cassini client agent starts an `AuthorityHandle` (a `PermissionsActor`, a `PolicyActor` over it, and an `AuthorityActor` that keeps both current), subscribes to `polar-authority-updates` and to the snapshot replies, and exposes `resources.authority` with `changes()` (an `AuthorityChanged` per applied update) and the two actors. `AuthorityCore` is the pure decision: only the credentials agent's identity as the broker names it (`POLAR_AUTHORITY_PUBLISHER`, default `credentials-agent.polar.serviceaccount.cluster.local`, read from the graph's `reported_by` on real outcomes) is listened to; the signing key is **pinned from the first snapshot from that identity and never replaced** (a snapshot naming another key is refused and reported); each update is verified then applied; a jump in the sequence asks for a snapshot, never more than once in five seconds; revoked serials feed the signer verification, so a revoked signer's next document is refused. A snapshot (the key and the latest signed update of each kind) has its own request and reply topics because the router allows one handler per topic and the escrow-key replies already belong to the crypto actor. **The Cedar-first decision is built (2026-10-12)** in `policy_authority::admit`, used by the ConfigurationActor (configuration and runtime documents) and the CommandActor: verification is in stages, `open` (signature, kind, shape, target), then the question of authority, then `close` (revocation, and a command's time). For each thing a change touches (a variable `Variable::"<agent>/<NAME>"`, a section `Section::"<agent>/<section>"`, a command `Command::"<agent>/<name>"`; actions `update-configuration`, `update-runtime`, `run-command`) the policy actor is asked with the signer as the principal: `Allow` admits whatever the certificate carries, `Deny` refuses whatever it carries, `NotDetermined` or no policy loaded falls back to the certificate's capability, and a rule that cannot be evaluated or a policy actor that does not answer refuses. A change is admitted only if every thing it touches is. Ids are escaped, never formatted into text to be parsed (`polar-policy` also builds the principal from its type and id, which closes a hole where two names could collapse into one entity). Built since (2026-10-12): the credentials agent publishing and answering, and the `policy` and `groups` sections that feed it.

**Built 2026-10-12, in `credentials_agent_common`:** `authority_update` (the payload kinds `Policy`, `Groups`, `Revoked`; `sign` with a blinded RSASSA-PSS-SHA256 over the domain line and the payload bytes; `verify`, which checks the key fingerprint, then the signature, and reads the bytes only then, strictly) and `authority_state` (latest of each kind, stale by version, a jump in the credentials agent's sequence reported as a missed update, and what arrives as the answer to a catch-up request exempt from that). Each was written test first; dropping the domain line, the fingerprint check, the catch-up exemption or the stale rule each fails a test. The update carries the signed bytes, not a structure to be re-encoded.

On accepting an update the credentials agent records it in its ledger with a **global sequence number** (the ledger
already orders admin actions this way) and publishes it on a topic, `polar-authority-updates`:

```
AuthorityUpdate { kind: "policy" | "permissions", sequence, version, document: <the signed document, untouched> }
```

State-based, not a delta: the whole new policy or the whole new principal data, so a missed event is repaired by
the next, and a restarted agent needs only the latest. Consumers, in every builder agent:

1. check it came from the credentials agent (the broker says who published it);
2. check the document itself as for any signed document: the chain to the pinned CA, and that the signer holds
   the capability for its kind (`c:update-policy`, or the one for permissions). That root is the certificate
   capability, which needs no policy to check, so there is no circle;
3. apply it to the local `PolicyActor` or `PermissionsActor` if its version is higher;
4. raise `AuthorityChanged { kind, version }` on an output port of the infrastructure supervisor, as
   `ConfigChanged` is raised today, so any domain actor that holds something derived from authority (a session, a
   cached decision) can drop it at once.

**Catch-up.** Cassini does not yet acknowledge deliveries, so an update can be missed. Every agent asks the
credentials agent for the current policy and principal data when it registers (as it asks the polar scheduler for
its configuration), and the credentials agent also republishes the current versions on an interval. A consumer that
sees a version higher than its own it did not receive asks.

**Ledger-originated changes.** A revocation or a grant made by a signed admin action changes principal data too
(a revoked certificate stops being a signer). The same signal carries it, so a demotion is immediate at the
leaves. How a revocation reaches the signer-verification seam in `signed.rs` (today `NoRevocation`) is the same
mechanism: the leaf holds the revoked serials as part of the principal data.

## Evaluating at a leaf

A Cedar-backed `Authority` plugs into the trait the verifier already has. For a document or command the request is:

- principal: the signer, `Identity::"<id>"`, whose attributes and groups `PermissionsActor` resolves from the
  certificate plus the supplemental data;
- action: `update-configuration`, `update-runtime`, `run-command`;
- resource: `Section::"<agent>/<section>"`, `Variable::"<agent>/<NAME>"` or `Command::"<agent>/<name>"`, built
  from the agent's own declarations;
- context: the time, and what a rule needs.

**Cedar first, the certificate as fallback.** With no policy loaded (a fresh cluster, the bootstrap case) the
capability decides, as it does today. Once a policy is loaded the request goes to it: `Allow` admits the signer,
`Deny` refuses them, and `NotDetermined` (no rule applies) falls back to the capabilities in the certificate. So the
certificate stays the coarse default and policy overrides it in either direction.

The principal's attributes, including its groups as parents and the capabilities its certificate carries, come only
from `PermissionsActor` (**built**: `ResolvePrincipal`, `UpdateGroups`; a rule can say `principal in Group::"operators"`
or read `principal.capabilities`).

## What the builder gains

- `PermissionsActor` and `PolicyActor` in every builder agent, started by the infrastructure supervisor beside the
  configuration and command actors, with the shared schema.
- A built-in subscription to `polar-authority-updates` and the request for the current state on registration.
- `AuthorityChanged` on the infrastructure supervisor's ports.
- The Cedar `Authority` for configuration, runtime documents and commands, behind the existing trait.
- Declarations feed the resources: an agent's sections, variables and commands are the resource types.

## What the credentials agent gains

- It accepts the `policy` and `permissions` documents (as runtime sections of its own, `credentials-policy.md`),
  validates them, records them in the ledger with a sequence, and raises the signal.
- It serves the current state on request and republishes it on an interval.
- Its own `PolicyActor` and `PermissionsActor` are the same actors the leaves run.

## Build order, tests first

1. The shared evaluation as a pure function: given a principal's data, a policy and a request, the decision, with
   the attack list (a document cannot set `permissions`, a forbid beats a permit, the capability floor with and
   without a policy).
2. `PermissionsActor` takes group membership, and `PolicyActor` takes the shared schema and rejects what does not
   validate. The invariant that nothing but `PermissionsActor` supplies principal data is a test.
3. The `AuthorityUpdate` message, its verification at a leaf, versioning, and `AuthorityChanged`.
4. The builder hosts the actors and the Cedar `Authority`; the verifiers use it.
5. The credentials agent accepts, records and raises; serves and republishes.
6. The seed policy and permissions in the schedules repository, and a cluster test: a demotion reaches a leaf and
   is refused there within a second.

No question is open. Next: the `AuthorityUpdate` message and its verification at a leaf.
