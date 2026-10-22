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

# The authority model: one place to reason about who may do what

Written 2026-10-22. **Proposed, not settled.** It is to be covered with the maintainer, one question at a time, before
any of it is built. It replaces stage 5b's slice 5 (the credentials agent's check of a policy on arrival, open point B in
`policy-enforcement.md`), which would have been a hand-written version of the same thing.

## Why

Open point B was decided on 2026-10-21: before the credentials agent accepts a new policy, it evaluates the policy
against a fixed set of requests, and if any would be denied it refuses the whole policy. A policy that would cut the
channel "through which policy is corrected, or certificates renewed" is never accepted.

Working out that fixed set showed that the facts it needs are in many places, and some are in none:

- the workloads, their groups and their declared topics are in the catalog, but the render splits them: cert-issuer is
  given the bootstrap services' (`infra/rules.nu:182`, `floor_entries`), the credentials agent the rest
  (`infra/rules.nu:166`, `issuance_topics`, which drops `bootstrap: true`);
- what a group implies is code (`polar-agent-builder/src/topics.rs:212`, `implied_by`);
- a workload's identity has two forms, converted wherever someone needs it;
- membership at runtime is the signed groups document, which replaces the certificate's groups for policy;
- who depends on whom (renewal goes through the observer, correction through the scheduler) is written nowhere.

Each new rule would add another projection of the catalog, or another hand-written probe. The maintainer's view
(2026-10-22): the data is not structured, or in one place, so that the obvious inferences can be made at the moment the
authority decides whether to accept a policy; the credentials agent, or a policy agent split from it, should be the
arbiter of whether a policy is accepted, with a stronger mechanism than probes.

This extends a principle already decided: the credentials agent is the **ultimate decision authority** for policy and
permissions, and pushes decisions out to the leaves (`authority-distribution.md`, "The principle", 2026-10-12).

## The paths authority travels

What has to stay open for the system to be governable. Read from the code, 2026-10-22.

- **Hearing authority.** The credentials agent publishes `polar-authority-updates` and `polar-authority-snapshot-replies`
  and hears `polar-authority-snapshot-requests`; every Cassini client hears the first two and publishes the third, all
  implied by `cassini-client` (`topics.rs:234`). The broker hears them in process (`cassini/broker/src/authority.rs`).
- **Correcting policy.** An administrator signs a `policy` or `groups` document and commits it to the schedules
  repository on the Polar git server. The scheduler observer pulls it (on the git server's announcement on Cassini, or
  on an interval) and publishes it on `scheduler.in`. The scheduler processor publishes it on
  `agent.credentials-agent.configuration` (its family, `polar-scheduler/processor/src/agent.rs:52`; the credentials
  agent is among the served workloads, `infra/topics.nu:32`). The credentials agent hears its configuration topic
  (implied), accepts the document, and publishes it as authority. A credentials agent that starts asks for what is held
  for it on `polar-configuration-replay`, and the scheduler observer replays it. The admin action `PolicyUpdate` is
  refused as not yet supported (`credentials-agent/consume/src/admin_action.rs:103`), so the observer's
  `/admin-action` is **not** on this path today.
- **Issuing and renewing certificates.** polar-init calls the credentials agent's observer over HTTP
  (`/workload/session`, `/workload/issue`); the observer publishes `credentials-workload-issuance-requests` and hears
  the replies (`credentials-agent/observe/src/agent.rs:40`); the consumer answers. Renewal is a new pod running
  polar-init again. An agent that needs encryption also escrows a key from init, over Cassini.
- **Encryption.** Members of `encryption` publish the crypto request topics and hear the reply topics (`topics.rs:248`).
- **Accounts and administrator actions.** Through the observer: `/account`, `/admin-action`.

The bootstrap services (cert-issuer, Cassini, the credentials agent and its observer) are marked `bootstrap: true` in
the catalog; nothing records why each is critical, or which of the paths above it carries.

## What is checked today

Every rule is code; none is data evaluated generically. Grouped by when it is checked. Paths are under `src/agents`
unless they start with `infra`.

**At render** (`infra/resolve.nu`, `infra/rules.nu`, `infra/topics.nu`, `infra/render.nu`):
- two workloads may not share `<sa>.<ns>`, and the default ServiceAccount is refused (`resolve.nu:631`, `render.nu:312`);
- the issuer is cert-issuer or the credentials agent; a bootstrap identity uses cert-issuer; a non-bootstrap workload
  on cert-issuer is a warning, or an error where `issuance.floor_enforced` is set (`rules.nu:135`);
- a workload on the credentials agent needs the observer (`resolve.nu:494`; polar-init checks the same,
  `polar-init/src/environment.rs:113`);
- a bootstrap identity is one certificate type (`rules.nu:193`);
- cert-issuer's floor: a bootstrap identity's entry carries its declared topics and groups; a credentials-agent
  workload with a server certificate gets a `vouchedOnly` entry (`rules.nu:182`);
- the credentials agent's issuance floor, by token identity, for non-bootstrap workloads (`rules.nu:166`);
- a one-sided topic is a warning per target (`topics.nu:266`) and a failure catalog-wide
  (`infra/tests/catalog-topics.nu:92`); more than 64 carried topics is a warning (`topics.nu:289`);
- the escrow bootstrap follows from `encryption` (`infra/config/capabilities.nuon`).

**At start:**
- an agent's declarations: unique runtime sections, topics with a purpose, none declared twice or declared when implied
  (`polar-agent-builder/src/agent.rs:538`, `topics.rs:453`);
- an agent's name is the workload of its certificate's identity (`polar-agent-builder/src/own_name.rs:33`);
- cert-issuer's floor and the credentials agent's issuance floor parse strictly (`cert-issuer/server/src/floor.rs:202`,
  `credentials-agent/consume/src/issuance_floor.rs`); cert-issuer with no floor at all is open to any valid token, and
  says so (`cert-issuer/server/src/main.rs:157`).

**At issuance:**
- the credentials agent: a request's shape, the token's validation, one valid certificate per identity, binding and
  type, a grant where one is needed, and the topic and group names taken from the issuance floor by the token's
  identity (`credentials-agent/consume/src/issuance.rs:72`–`:301`);
- cert-issuer: a token-only caller is a non-`vouchedOnly` floor entry of that type with only its names; a vouched
  caller adds only `p:` and `g:` names; no `c:` name on `/issue`; `/internal/issue` only for the credentials agent's
  identity (`cert-issuer/server/src/handler.rs:140`, `floor.rs:107`);
- administrator actions: a known, unrevoked author, a good signature, the required `c:` from the ledger, no replay
  (`credentials-agent/consume/src/admin_action.rs:57`).

**On a signed document's arrival** (every agent's configuration actor, the credentials agent's included):
- the signature, the chain to the pinned CA, the type and target, and a version newer than the one held
  (`polar-agent-builder/src/signed.rs:215`, `configuration.rs:314`);
- **who may change policy is never governed by policy**: a `policy` document needs `c:update-policy` and a `groups`
  document `c:update-groups`, read from the signer's certificate (`credentials-agent/consume/src/authority_sections.rs:41`,
  `polar-agent-builder/src/runtime.rs:35`); other sections are admitted by policy, then the certificate's `c:` names;
- a policy parses, is at most 256 KiB, and passes the strict schema with warnings counted as errors
  (`polar-policy/src/lib.rs:90`); polar-sign runs the same check before signing;
- a groups document's shape, sizes, real `c:` and `p:` names in roles, and no cycle
  (`authority_sections.rs:92`, `cert-issuer/common/src/hierarchy.rs:123`). Its identity keys are not checked against
  anything.

**As authority is distributed:**
- the credentials agent republishes only policy and groups, signed, numbered by its ledger
  (`credentials-agent/consume/src/authority_publisher.rs:181`);
- a leaf believes only the configured publisher, pins its key from the first snapshot, verifies every update, refuses
  a stale version, asks for a snapshot on a gap, and keeps the old policy when a new one fails to load
  (`polar-agent-builder/src/authority.rs:109`).

**At each decision** (the agent's gate, the broker):
- Cedar first, then the floor; an error is Deny; no policy is the floor (`polar-policy/src/lib.rs:463`,
  `polar-agent-builder/src/topic_gate.rs:54`);
- the principal's groups: the groups document's entry for the identity, when there is one, **replaces** the
  certificate's groups; capabilities always come from the certificate (`polar-permissions/src/lib.rs:192`);
- the broker's floor is the certificate's declared topics and the topics its `g:` groups imply
  (`polar-agent-builder/src/floor.rs:47`), so the broker's floor follows the certificate's groups while policy follows
  the groups document;
- a refusal never contains "session mismatch", which makes a client register again.

**Identity forms.** The token form `system:serviceaccount:<ns>:<sa>` keys cert-issuer's floor, the issuance floor and
grants. The certificate form `<sa>.<ns>.serviceaccount.<domain>` keys the ledger, policy, the groups document, the
broker's `published_by` and the authority publisher. `normalize_identity` converts the first to the second
(`cert-issuer/common/src/identity.rs:95`), `workload_of` reads the workload from the second (`:167`), and the render
writes both by string templates (`rules.nu:169`, `:190`). The credentials agent's own identity is written in three
places: the default `POLAR_AUTHORITY_PUBLISHER`, cert-issuer's internal callers, and the `.cluster.local` suffix.

### What nothing checks

- That a new policy, or a new groups document, leaves the paths above open. A `forbid` can take any topic from any
  workload, the authority topics included.
- That a groups document agrees with the catalog: an entry for a workload replaces its catalog groups in policy.
- That a request reaching the credentials agent came from the workload it should: on its request topics only the
  broker's decision limits who publishes.
- Why a bootstrap service is critical, and which path it carries.

## The proposal

Three parts. The standard names help: the authority is the policy administration point (it decides which policy is
in force); every agent and the broker are decision points and enforcement points (they evaluate the accepted policy
locally). Only the first changes.

### 1. One authority model, as data

The render emits one document of base facts, applied with the catalog, so applying the catalog is still the sign-off
(`policy-enforcement.md`, "Decisions"). The credentials agent reads it at start. Its facts:

| Fact | Means | From |
|---|---|---|
| `workload(W)`, `identity(W, token, certificate)` | the workload and both its identity forms, written once | catalog |
| `bootstrap(W)`, `issuer(W, I)` | how its certificates are issued | catalog |
| `cert_group(W, G)` | the groups its certificate carries | its scaffold, through the catalog |
| `declares(W, T, A)` | a topic it declares (families expanded) | its scaffold |
| `implies(G, W, T, A)` | a topic group `G` implies for `W` | its scaffold's computed set, with the rule as origin |
| `carries(P, W, T, A)` | `W`'s use of `T` is a step of path `P` (renewal, correction, hearing, encryption) | its scaffold (see below) |

The runtime documents add facts as they are accepted: `member(I, G)` and the group and role hierarchy from the groups
document, and the policy itself, which Cedar evaluates.

**Where the path facts come from.** Not a hand-written list in the catalog ("nothing is hand-rolled configuration",
`policy-enforcement.md`, "The model"). The declaring code says it: a `TopicDecl` gains an optional path, so the
observer's publish of `credentials-workload-issuance-requests` is declared as a step of renewal, and the scheduler
processor's family as a step of correction; implied topics carry theirs from their rule (`cassini-client`'s authority
topics are hearing). The scaffold prints them, the render collects them, as it collects topics now.

The issuance floor becomes a query on this model, not a separate variable, and the identity forms are converted in one
place. cert-issuer keeps its own small floor, on purpose (`cert-issuer-isolation.md`).

### 2. Rules over the model

Derived facts, in Datalog. For example:

```
% The groups policy sees: the groups document's, where it names the identity, else the certificate's.
named(I)              :- member(I, _).
group(I, G)           :- member(I, G).
group(I, G)           :- identity(W, _, I), cert_group(W, G), !named(I).
group(I, P)           :- group(I, G), parent(G, P).

% The floor the broker and the agent decide on where policy is silent.
floor(I, T, A)        :- identity(W, _, I), declares(W, T, A).
floor(I, T, A)        :- identity(W, _, I), cert_group(W, G), implies(G, W, T, A).

% What must stay allowed: every step of every path, for whoever carries it.
critical(I, T, A, P)  :- identity(W, _, I), carries(P, W, T, A), floor(I, T, A).
```

A path is the place where recursion earns its keep: "can workload W still be renewed?" is reachability along a chain of
steps, each a publish and a matching subscribe, and so is "can the policy still be corrected?". Cedar cannot express
that, deliberately (`policy-enforcement.md`, "Decisions": Datalog was kept out unless rules must derive facts
recursively; these do). A Datalog engine can also say which facts derived a conclusion, so a refusal carries its own
reason.

### 3. Acceptance as a comparison

The catalog is closed, so the universe of requests is finite: every identity in the model, and every person the groups
document names, against every topic in any floor or named by either policy, for publish and subscribe. When a policy or
a groups document arrives, the credentials agent evaluates the current pair (policy, groups) and the proposed pair over
that universe, with Cedar's own evaluator, and compares:

- a request in the critical set that would go from allowed to denied refuses the document whole, with the reason: the
  request, the path it is a step of, and the rule that denies it. For example: "refused: forbids
  credentials-agent-observer to publish credentials-workload-issuance-requests, a step of renewal for every workload the
  credentials agent issues";
- any other change is accepted, and its effect (who gained, who lost what) is recorded with the outcome.

It runs on either document's arrival, against the pair as it would stand, so a groups document that moves a workload
into a forbidden group is refused as a policy that forbids it would be. There is no second evaluator to drift from
Cedar, and no solver: Cedar has an SMT-based analysis for comparing policies, which a closed universe does not need.

Expected cost: a few thousand evaluations per document, which should take well under a second; to be measured.

## Where it lives

In the credentials agent, as its own crate and actor, behind an interface that would let it be split out later:

- accepting a policy and publishing it as authority are one step there today; split across two agents they would be a
  two-party commit;
- a separate policy agent would not be less trusted, since whatever accepts policy can lock everyone out; it would add
  a bootstrap service, and another path the check must protect, without shrinking what has to be trusted;
- reasons to split it later: the analysis growing heavy enough to slow the credentials agent, or needing its own
  release cadence.

## What stays as it is

- Decisions stay at the leaves and the broker, against whatever the authority accepted. Asking a central decider
  would add a round trip and a single point of failure to every publish.
- Cedar decides requests; the certificate decides where policy is silent; Cedar overrides it both ways.
- Certificates carry direct membership only; the broker's floor reads only the certificate.
- Who may change policy stays outside policy.

## The engine

The rules are safety properties of the system, not an administrator's choices, so they are code, reviewed with the
code. That allows an engine whose rules are compiled in:

- `ascent` or `crepe`: rules written in a macro, compiled and typed, with stratified negation (which the groups rule
  needs);
- `datafrog`: the small engine Polonius used; lower level, rules written by hand as joins.

If administrators ever need to write their own rules, that is a runtime engine, and a separate decision.

## Open questions

To settle one at a time, before building:

1. Where the path facts come from: an optional path on each declaration (proposed), or something else.
2. Which rules refuse and which only report: the paths above refuse; a topic left with no permitted publisher, say,
   might only be reported.
3. Whether the groups document is checked too (proposed: yes, against the pair).
4. The engine: `ascent`, `crepe`, `datafrog`, or a hand-written closure until paths need more.
5. The credentials agent, or a policy agent of its own.
6. What a refusal and an effect report look like, and where they are recorded (the ledger, the configuration outcome
   the scheduler records, or both).
7. Persons: only those the groups document names, or others too.
8. Whether the render's separate projections (the issuance floor, the served workloads) are replaced by the model at
   once, or later.
9. Whether the model also checks what the inventory found unchecked: a groups document's keys against the catalog, and
   who publishes on the credentials agent's request topics.

## Effect on stage 5b

Slice 5 as planned (hand-written probes in `validate_policy`) is not built. Slices 6 (the agent's side) and 7 (the
deploy and the measurement) do not depend on it and go ahead. Until the model is built, a `forbid` on an authority path
can lock a deployment out now that the broker enforces.
