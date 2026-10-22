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

Written 2026-10-22. **Proposed, being settled** with the maintainer, one question at a time, before any of it is built;
what is settled is under "Decisions". It replaces stage 5b's slice 5 (the credentials agent's check of a policy on arrival, open point B in
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

## Decisions

- **Agents declare local causes; the paths are derived (2026-10-22).** Paths are declared in code, like everything else
  ("nothing is hand-rolled configuration"), but no developer declares a path. Each agent states only what its own code
  does: what it publishes and subscribes to (as now), and what causes each publish: a message arriving on a topic, an
  external entry (an HTTP route, its registration, a timer, something outside Polar it watches), or nothing, for what the
  agent publishes of its own accord. An
  entry that waits for an answer says which topic answers it. Nothing names another agent or a path. The authority holds
  the rules and computes the paths from everyone's facts, with Datalog: a path is reachability over the edges that
  publishes, subscriptions and causes make, under the policy being judged. A hop added later (an agent between two
  others) changes the path and the check follows, with no one editing a goal. Chosen over a path declared on each
  topic, which made the author of one agent know the shape of a path through others. The facts are one set and each
  viewpoint is a query over it (the authority's acceptance, an agent's own round trips, an operator's comparison of two
  versions); derived facts may be published with the authority for leaves to read, but they explain and accept,
  never enforce: the broker and the agents still decide by Cedar and the certificate.
- **The vocabulary of causes (2026-10-22).** A publish is caused by one of these, and the list grows when Polar needs it:

  | Cause | Means | Used by |
  |---|---|---|
  | `Topic(name)` | a message arrived on a topic | the consumer answering a request; the scheduler processor forwarding `scheduler.in` |
  | `Http(route)` | a request on a route this agent serves | the observer's six routes; the git server's push |
  | `Registered` | the agent registered with Cassini (again) | the authority ask, the configuration replay |
  | `Timer(name)` | its own interval | polling observers; the scheduler observer's pull |
  | `Outside(source)` | something it watches outside Polar | kube-observer (the Kubernetes API), the Jira, GitLab and OpenAPI observers |
  | none | of its own accord | rare, and the exception that gives a reason, as `waits` does now |

  With it:
  - an invariant has one of two shapes, both from the same edges: a **round trip** (an entry's request comes back to
    where it is answered: renewal, the observer's `/workload/issue`) and a **delivery** (what an entry starts reaches
    every subscriber of a topic: correction, a push to the git server reaching the credentials agent through the git
    server's announcement and the scheduler pair; hearing authority, `polar-authority-updates` reaching every agent);
  - only what policy can cut is an edge. A call outside Cassini (the scheduler observer's fetch from the git server,
    decided by the git server's access list) is recorded as a fact and is no edge, until what decides it is governed
    by policy too;
  - an invariant names its entry by agent and cause, with the constant the code uses (a route is a constant in the shared
    crate), so the invariant cannot drift from the route.
- **Two invariants refuse; renewal is derived as their dependency (2026-10-22).** The authority's code names two:
  **hearing authority** (`polar-authority-updates` reaches every agent, and every agent's snapshot request is answered)
  and **correction** (a push to the git server's schedules repository reaches the credentials agent). A policy or groups
  document under which either stops holding is refused. Everything else is reported, never refused: another round trip
  that stops closing, a topic left with no permitted publisher, an agent cut off from its own work, the `encryption`
  agents' key requests; so an administrator can still cut a workload off on purpose and see what it loses.
  Renewal is not a third named invariant but a dependency the rules derive. No certificate is renewed in place: a pod
  rejuvenates as its certificate nears its end (about hourly in the sandbox) and its new init is issued one, which for
  every workload the credentials agent issues goes polar-init, the observer's `/workload/issue`, its publish on
  `credentials-workload-issuance-requests`, and the consumer's reply. That topic is shared, so a `forbid` on it cuts
  issuance for all twelve such workloads at once (quarantining one is done in issuance, by its grant), among them the
  correction path's git server and scheduler pair: nothing fails at first, and within one certificate lifetime
  correction is gone, though a check of messages alone passed it. So an agent stays up only while its certificates can
  be renewed (a bootstrap service renews through cert-issuer, outside policy), and an invariant holds only through
  agents that stay up: a policy that cuts renewal is refused as breaking correction, with that reason ("correction
  would stop within one certificate lifetime: renewal for polar-scheduler no longer closes"). Chosen over naming
  renewal as a third invariant: the same policies are refused, with a truer reason, and a renewal path that no
  invariant depends on (there is none today) would only be reported.
- **The model lives in the credentials agent, as a crate of its own (2026-10-22).** A crate under
  `src/agents/credentials-agent/`, beside `common`, `consume` and `observe`, holding the facts' types, the rules (one
  `ascent!` block) and the comparison of two pairs, with no actor or Cassini in it, so it is tested alone and could move
  into an agent of its own later. The consumer uses it where it accepts a `policy` or `groups` section. Chosen over a
  separate policy agent: accepting a policy and publishing it as authority are one step in the credentials agent, and
  split across two agents they would be a two-party commit; a policy agent would be no less trusted (whatever accepts
  policy can lock everyone out), and it would add a bootstrap service and a path the invariants must protect. The
  spike's figures (tens of microseconds an evaluation) leave no load reason to split. Reasons to split it later: the
  analysis growing heavy enough to slow the credentials agent, or needing a release cadence of its own. One consequence
  for the build: the check must run before the section is replaced, and the only hook there today is the builder's
  `validate: fn(&serde_json::Value)` (`polar-agent-builder/src/runtime.rs:57`), a pure function of the body; the
  consumer's `pass_on_section` (`credentials-agent/consume/src/supervisor.rs:599`) reads a section only after it has
  been replaced, to publish it. The check needs the catalog's facts and the other document of the pair, so a runtime
  section needs a check that is given them, before the configuration actor replaces it; its shape is for the build.
- **A refusal is an `invalid` outcome and a ledger event; an acceptance carries its effect (2026-10-22).** Two records
  that exist already carry both:
  - the document's outcome, `ConfigurationOutcome` (`polar-scheduler/common/src/lib.rs:164`), which every agent
    announces for each signed document and the scheduler processor records in the graph beside it, where an
    administrator looks for their document's fate;
  - the credentials agent's ledger, which writes `CredentialsEvent::AuthorityPublished` for every authority it
    publishes, numbered in sequence (`credentials-agent/consume/src/ledger.rs:292`): the durable record, with its
    provenance.

  So:
  1. A refused document's outcome has the status `invalid` ("verified, but what it asks is not acceptable (… a
     relation that would break)", `outcome::INVALID`), and its `detail` lines give the reason in a fixed form: the
     invariant broken (hearing authority or correction); what broke it (the edge lost, or the renewal it depends on);
     the request now denied (identity, action, topic) and the ids of the Cedar policies that deny it; and the witness
     path under the current pair, what used to work. The ledger records the refusal too, as a new `AuthorityRefused`
     event naming the document, its signer and the reason: a signed lockout is worth a permanent record.
  2. An accepted document's effect goes into its `AuthorityPublished` event: who gained or lost what (identity, topic,
     access), and which round trips stopped or began to close. Its `applied` outcome carries a summary in `detail`
     ("3 identities lose 5 topics; 1 round trip stops closing"), so the graph shows it beside the document; the whole
     effect stays in the ledger.
  3. Outcomes carry names, never values, as now: the report names identities, topics and policy ids, never the
     policy's text.
- **The comparison covers every account the credentials agent holds (2026-10-22).** Workloads come from the catalog;
  persons from the credentials agent's ledger, which issued every person's certificate, so every account it holds is
  in the universe and the report is complete without guessing. Persons are on no invariant path today (an
  administrator's step in correction is a push to the git server, decided by its access list, which is no edge), but
  they matter to the effect report: a person's identity implies no topics, so its floor is only what its certificate
  declares and a permit is what moves its access; a change of policy is exactly what an administrator wants to see
  for them ("alice loses `credentials-admin-action-requests`"). Chosen over the persons the documents name (the groups
  document's, and those a policy names outright), which would miss everyone a group rule covers whom the groups
  document does not list. The cost is one evaluation a person a topic, small at these sizes.
- **The model replaces the credentials agent's issuance floor when it lands (2026-10-22).** Of the render's three
  projections of the catalog, only the issuance floor goes:
  - cert-issuer's floor stays, as its isolation decided: it is given only the bootstrap entries it signs
    (`cert-issuer-isolation.md`);
  - the served workloads (`POLAR_SERVED_WORKLOADS`) stay: the scheduler reads them, not the credentials agent, and
    expands its topic family from them at start;
  - the issuance floor (`CREDENTIALS_AGENT_ISSUANCE_TOPICS`, `infra/rules.nu:166`, `issuance_topics`) is replaced: what
    it lists (each workload the credentials agent issues, by token identity, with its declared topics and groups) is
    in the model's facts (`identity`, `issuer`, `declares`, `cert_group`), and issuance becomes a query on the model.

  Kept beside the model, the credentials agent would be given the same facts twice by two render functions, and on the
  day they disagree what it writes into a certificate would differ from what it judges policy against; the model also
  converts identity forms in one place, and the issuance floor is keyed by the token form. Applying the catalog stays
  the sign-off of what a certificate carries, since the model is applied with the catalog. The cost is contained: the
  issuance code (`credentials-agent/consume/src/issuance.rs`, `issuance_floor.rs`) moves onto the model's types,
  `issuance_topics` goes from the render, and the issuance tests cover it.
- **A groups document's keys are judged against what exists (2026-10-22).** Today a key is checked only for its
  length and control characters (`credentials-agent/consume/src/authority_sections.rs:92`), so a typo
  (`polar-scheduler.polar.serviceaccount.cluster.locl`) is accepted and applies to no one. The model knows every
  workload identity (the catalog) and every account (the ledger), so:
  - a key that names no identity the authority knows is reported, not refused: it shows in the outcome's summary ("names
    no identity the authority knows: …"), since it may be deliberate (a person put in a group before their account
    exists);
  - a key that is no identity at all, neither a workload's identity nor a username, is `invalid`; the groups section's
    own validation can judge that alone.
- **A groups document is checked the same way as a policy (2026-10-22).** Against the same rules: the two invariants,
  renewal derived beneath them, over the same catalog facts. The rules are judged on a pair, the policy and the groups
  document, and do not ask which of them arrived. When either arrives, the authority:
  1. forms the pair as it would stand: the new document with the other one in force;
  2. asks Cedar about every request in the model under that pair, for the `allowed` facts (Cedar's `principal in
     Group::"..."` reads the groups document's memberships);
  3. runs the rules over those facts and the catalog's (the groups document's hierarchy also enters the Datalog
     directly, as `group` and `parent`, where nested groups get their closure);
  4. refuses the document whole, with the reason, if an invariant that holds now would not; otherwise accepts it and
     records its effect.

  A policy changes the rules Cedar applies, a groups document who is in which group; either can break an invariant
  alone. A `forbid (principal in Group::"retired", action == Action::"publish", resource)` is accepted while no
  workload is in `retired`; a later groups document that puts `polar-scheduler` in it would break correction with no
  policy arriving, and is refused. The refusal names the groups document and the membership, though the `forbid` came
  first; either document may be the fix (undo the membership, or narrow the `forbid`).

## Two kinds of groups

Polar has two, and "the groups document" is only the second (read from the code, 2026-10-22):

1. **A certificate's groups.** Deployment-time. A workload's scaffold implies them (`cassini-client`, `encryption`,
   `self-test`); the render puts them in the catalog, and the certificate carries them as `g:` names. Applying the
   catalog is their sign-off; they change only with a redeploy.
2. **The groups document.** Runtime configuration: the credentials agent's `groups` section
   (`credentials-agent/consume/src/authority_sections.rs:50`): memberships (an identity to the groups it is in), and a
   hierarchy of groups and roles (a group may be in other groups, a role may hold roles, capabilities and permissions).
   It travels as the policy does: a holder of `c:update-groups` signs it, it is committed to the `polar-schedules`
   repository, the scheduler pair forwards it to the credentials agent, and the credentials agent accepts it and
   republishes it, signed, as authority to every agent and the broker.

Neither is seeded: `infra/seeds/signed/` holds only `policy.cedar`, which up.nu's seed step signs and commits. No groups
document exists in the sandbox, so today every principal's groups are its certificate's.

Where the groups document names an identity, its list replaces that identity's certificate groups, for Cedar only
(`polar-permissions/src/lib.rs:192`, `resolve_principal`). The floor, and so the broker's admission where policy is
silent, always reads the certificate's groups.

## The consumer's request topics, audited (2026-10-22)

For open question 10: for each topic the credentials agent's consumer serves (`REQUEST_TOPICS`,
`credentials-agent/consume/src/supervisor.rs:148`), what a request proves by itself, whether the consumer is given the
broker-verified publisher (only four are, `dispatch_request`, `supervisor.rs:619`), and what a client publishing on it
directly, not through the observer or the agent meant to, could do. Read from the code.

| Topic | Meant to publish it | What the request proves | Publisher given | Published directly, it could |
|---|---|---|---|---|
| `credentials-grants` | nothing in the catalog (`.waiting`) | nothing | no | append any ledger event (below) |
| `credentials-revocations` | nothing in the catalog | nothing | no | append any ledger event (below) |
| `credentials-account-provisioning` | nothing in the catalog | nothing | no | append any ledger event (below) |
| `credentials-admin-action-requests` | the observer | a known, unrevoked author's signature, the required `c:`, no replay (`admin_action.rs:57`) | no | no more than through the observer |
| `credentials-account-requests` | the observer | bootstrap: the bootstrap service account's token; native: a username and email matching a provisioned account, "no out-of-band secret" (`account_claim.rs:51`) | no | no more than through the observer |
| `credentials-issuance-requests` | the observer | an IdP token, introspected, and a grant (`issuance.rs:168`) | no | no more than through the observer |
| `credentials-workload-issuance-requests` | the observer | a Kubernetes token cert-issuer validates; one certificate per identity, binding and type | no | no more than through the observer |
| `credentials-identity-normalization-requests` | the observer | a token cert-issuer validates | no | learn the normalized identity of a token it already holds |
| `credentials-agent-escrow-key-requests` | the observer | nothing needed: it answers with a public key | no | read a public key |
| `credentials-escrow-key-requests` | encryption agents' crypto actor | nothing needed: the escrow public key | no | read a public key |
| `credentials-certificate-requests` | encryption agents' crypto actor | nothing needed: another identity's public certificate | no | read a public certificate |
| `credentials-agent-escrow-issuance-requests` | polar-init, for encryption agents | a Kubernetes token cert-issuer validates | no | no more than polar-init |
| `credentials-store-secret-requests` | encryption agents | the publisher, a payload encrypted to the escrow key, and Cedar | yes | only what it may itself |
| `credentials-retrieve-secret-requests` | encryption agents | the publisher, and Cedar | yes | only what it may itself |
| `credentials-retrieve-agent-escrow-keypair-requests` | polar-init | the publisher, which names the keypair (`agent_escrow_keypair_bootstrap.rs:65`) | yes | only its own keypair |
| `credentials-decrypt-on-behalf-of-requests` | encryption agents | the publisher, and Cedar | yes | only what it may itself |

Thirteen of the sixteen prove themselves, or carry the broker's publisher and Cedar. **The first three prove nothing.** A
message on any of them is read as a `CredentialsEvent` and appended to the ledger as it is (`supervisor.rs`, the
fall-through of `deserialize_and_dispatch`; `ledger.rs:319`, `AppendNoReply`): no signature, no publisher, and any kind of
event, the ones documented as "published internally by the consumer itself" (`IssuanceCompleted`, `SecretStored`)
included. So a client that could publish on one could, for example:
- provision an account holding `c:update-policy` (`AccountProvisioned`), then claim it with the username and email it
  chose, and sign policy;
- grant an IdP identity `p:` names of its choosing, which issuance writes into its certificate (`Grant`);
- revoke an administrator's certificate (`Revoke`), which the credentials agent publishes as authority, so every agent
  refuses that administrator's signature.

What stands in the way today is the broker alone: nothing declares publishing on these topics, so since stage 5b the
floor refuses everyone, and only a Cedar `permit` would open one. Before stage 5b was deployed (2026-10-22) the broker
admitted every client's publish. The signed administrator actions already carry all three (`AdminAction::ProvisionAccount`,
`Grant`, `Revoke`; `credentials-agent/common/src/admin_action.rs:40`), through `credentials-admin-action-requests`, so the
raw topics are a parallel path that nothing in the catalog uses.

*Removed 2026-10-22.* They were the first design (2026-08-27 for grants and revocations, 2026-09-25 for provisioning),
replaced by the signed administrator actions on 2026-09-30 (`3ea5e754`, which says "The unsigned GRANTS, REVOCATIONS and
ACCOUNT_PROVISIONING subscriptions are still in place and are to be removed"), and never removed; declaring the topics in
stage 2 then marked them as waiting for a publisher, which hid the leftover. The consumer serves 13 request topics now,
nothing reaches its ledger as it arrives (a message on a topic it has no handler for is dropped), and the ledger's
`AppendNoReply` is gone. The `Grant`, `Revoke` and `AccountProvisioned` events stay: they are the effects derived from a
recorded signed action (`CredentialsEvent::admin_effect`).

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
| `causes(W, In, T)` | `W` publishes on `T` because of `In`: a message on a topic, or an entry | its scaffold (see below) |
| `entry(W, E, Sent, Answer)` | `W` takes entry `E` (a cause other than a topic: "Decisions"), publishes on `Sent`, and answers it from `Answer` | its scaffold |

The runtime documents add facts as they are accepted: `member(I, G)` and the group and role hierarchy from the groups
document, and the policy itself, which Cedar evaluates.

**Where the cause facts come from** ("Decisions"). The declaring code says what causes each publish, beside the topic.
The observer:

```rust
TopicDecl::publish(WORKLOAD_ISSUANCE_REQUESTS, "polar-init's workload issuance")
    .because(Cause::Http("/workload/issue")),
TopicDecl::subscribe(WORKLOAD_ISSUANCE_REPLIES, "the answer to it")
    .answers(Cause::Http("/workload/issue")),
```

The consumer:

```rust
TopicDecl::subscribe(WORKLOAD_ISSUANCE_REQUESTS, "a workload's issuance request"),
TopicDecl::publish(WORKLOAD_ISSUANCE_REPLIES, "the certificate, or why not")
    .because(Cause::Topic(WORKLOAD_ISSUANCE_REQUESTS)),
```

Neither says "renewal" nor names the other. Request and reply may get a shorthand. Implied topics carry their causes from
their rule, so `cassini-client`'s authority round trip is declared once. The scaffold prints the causes, the render
collects them as it collects topics now, and applying the catalog signs them off. The vocabulary of causes (which
entries exist) is an open question.

The issuance floor becomes a query on this model, not a separate variable ("Decisions": when the model lands), and the identity forms are converted in one
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

% A message on T goes from A to B if both may use T (allowed: Cedar's answers, then the floor, under the policy judged).
edge(out(A,T), in(B,T)) :- publishes(A,T), subscribes(B,T), allowed(A,T,pub), allowed(B,T,sub).
% Inside an agent, what arrives leads to what it publishes.
edge(in(A,I), out(A,O))  :- causes(A, I, O).
reach(X,Y) :- edge(X,Y).
reach(X,Z) :- reach(X,Y), edge(Y,Z).
% A round trip: an entry's request comes back to where it is answered.
closes(A,E) :- entry(A,E,Sent,Answer), reach(out(A,Sent), in(A,Answer)).
% An agent stays up only while its certificates can be renewed ("Decisions": renewal is a dependency, not an invariant).
lives(W) :- issuer(W, cert_issuer).
lives(W) :- issuer(W, credentials_agent), closes(observer, http(WORKLOAD_ISSUE)).
% An invariant path holds only through agents that stay up.
held(out(A,T), in(B,T)) :- edge(out(A,T), in(B,T)), lives(A), lives(B).
% A delivery: what an entry starts reaches every subscriber of T (stratified negation: a subscriber it misses).
reached(A,E,Y)   :- causes(A,E,O), reach(out(A,O), Y).
misses(A,E,T,B)  :- causes(A,E,_), subscribes(B,T), !reached(A,E,in(B,T)).
delivers(A,E,T)  :- causes(A,E,_), subscribes(_,T), !missed(A,E,T).
missed(A,E,T)    :- misses(A,E,T,_).
```

"Renewal still works" is then `closes(observer, http("/workload/issue"))` under the proposed policy: no list of topics
is written for it. The invariants are judged over `held` edges, so an agent that would not be renewed carries nothing
for them; `held` is not recursive through `lives`, since renewal's own round trip is judged over `edge`. Reachability is
where recursion earns its keep, and Cedar cannot express it, deliberately (`policy-enforcement.md`, "Decisions": Datalog
was kept out unless rules must derive facts recursively; these do). Cedar still decides each request; its answers are
the `allowed` facts, and Datalog does the closure.

A refusal carries its reason: the round trip that no longer closes, and the edge whose loss broke it. No engine gives
proof trees by itself, so the witness is designed into the rules (see "The engine").

### 3. Acceptance as a comparison

The catalog is closed, so the universe of requests is finite: every identity in the model, and every account the
credentials agent's ledger holds ("Decisions"), against every topic in any floor or named by either policy, for publish
and subscribe. When a policy or
a groups document arrives, the credentials agent evaluates the current pair (policy, groups) and the proposed pair over
that universe, with Cedar's own evaluator, and compares:

- an invariant the authority's code names (hearing authority, correction: "Decisions") that holds under the current pair
  and not under the proposed one refuses the document whole, with the reason: the invariant, the edge or the renewal
  lost, and the rule that denies it. For example: "refused: correction would stop within one certificate lifetime:
  renewal for polar-scheduler no longer closes, since the policy forbids credentials-agent-observer to publish
  credentials-workload-issuance-requests";
- any other change, a round trip that is not an invariant included, is accepted, and its effect (who gained, who lost
  what, which round trips stopped closing) is recorded with the outcome. So an administrator can still cut a workload
  off on purpose, to quarantine it.

It runs on either document's arrival, against the pair as it would stand ("Decisions": a groups document is checked as
a policy is), so a groups document that moves a workload into a forbidden group is refused as a policy that forbids it
would be. There is no second evaluator to drift from
Cedar, and no solver: Cedar has an SMT-based analysis for comparing policies, which a closed universe does not need.

Expected cost: a few thousand evaluations per document, which should take well under a second; to be measured.

## Where it lives

In the credentials agent, as its own crate ("Decisions", 2026-10-22), behind an interface that would let it be split out
later:

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

`ascent` also has lattices, which give the explanation a refusal needs: keep, for each pair of nodes, the shortest witness
path, and reachability over a cycle still ends.

The model is small (about 20 agents and 150 topics), so a full evaluation takes milliseconds; an incremental engine
(differential dataflow, DBSP) would be more than it needs.

If administrators ever need to write their own rules, that is a runtime engine (Biscuit's Datalog, say), with rules as
signed documents like the policy, and a separate decision.

**Chosen: `ascent` (2026-10-22), after the research below.**

### Ascent, researched (2026-10-22)

What it is: Datalog embedded in Rust by a macro, from Arash Sahebolamri, Thomas Gilray (University of Alabama at
Birmingham) and Kristopher Micinski (Syracuse). It is described in "Seamless Deductive Inference via Macros" (CC 2022,
https://doi.org/10.1145/3497776.3517779), which reimplements the Rust borrow checker in it and benchmarks it close to
Datafrog and Soufflé. Its custom data structures are in "Bring Your Own Data Structures to Datalog" (OOPSLA 2023,
https://doi.org/10.1145/3622840).

The project, as it stood on 2026-10-22:
- 0.8.1 is the newest release (2026-08-29); before it 0.8.0 (2025-03), 0.7.0 (2024-10), 0.6.0 (2024-03). MIT licensed,
  not archived, last push 2026-08-29 (https://crates.io/crates/ascent, https://github.com/s-arash/ascent).
- 660,000 downloads, 250,000 of them recent; 591 stars. One maintainer writes most of it (142 of 162 commits), with
  outside contributions: in 2026, a change that split the generated `run()` and cut one user's build of about 4,000
  lines of rules from 12 minutes to 1 (https://github.com/s-arash/ascent/pull/86).
- Depended on by Quantinuum: `tket`, its quantum compiler, and `hugr-passes`, the compiler passes for its HUGR
  representation (https://crates.io/crates/ascent/reverse_dependencies). No other production use was found.
- Features we need, all documented (https://s-arash.github.io/ascent/): recursion; stratified negation; aggregation;
  user lattices (the shortest-path example with `Dual<T>` is the witness we want); `ascent_par!` on rayon; columns any
  `Clone + Eq + Hash` Rust type.

Its open issues that bear on the design, and what the design does about each:
- **A lattice joined on its lattice column can be read before it settles** (https://github.com/s-arash/ascent/issues/22,
  open since 2023-12; the maintainer agrees it is a bug and proposes rejecting such programs). The witness lattice is
  read after the run, as the refusal's reason, and no rule joins on it.
- **`run()` is one-shot**: running again after removing input facts leaves stale derived facts
  (https://github.com/s-arash/ascent/issues/30; "Ascent wasn't really designed for incremental processing of data").
  Each evaluation builds a fresh program from the whole pair and the catalog's facts; nothing is run twice.
- **Lattice merging is not applied to input facts** (https://github.com/s-arash/ascent/issues/26). No lattice is an
  input.
- **Macro hygiene in `ascent_source!`** (https://github.com/s-arash/ascent/issues/68). Not used: the rules are one
  `ascent!` block.

A spike, outside the repository, on 0.8.1 with this workspace's toolchain: the settled rules in miniature (`edge`,
`reach`, `closes`, `lives`, `held`, a delivery with stratified negation, and the witness lattice), over the renewal and
correction paths of the sandbox.
- Allowing everything, both invariants hold, and the renewal witness is the four-hop round trip through the observer and
  the consumer.
- Forbidding the observer's issuance requests, renewal no longer closes, the three agents the credentials agent issues
  stop living, and correction fails with them: the derived dependency, as decided.
- Forbidding the scheduler processor's forward, correction fails and renewal still holds.
- Each evaluation took 20 to 60 microseconds. A synthetic model far denser than Polar's (40 agents, a ring of topics with
  cycles, 51,000 reachable pairs, each with its witness path) took 66 ms.
- Rebuilding after a change to the rules took 2 s in release, 6 s in debug.
- One thing learnt: a rule's body binds variables and matches enum values with `if let` clauses, not with patterns in
  the atom; a rule's head may build them.

## Open questions

To settle one at a time, before building:

1. ~~Where the path facts come from.~~ Settled: local causes, derived paths ("Decisions", 2026-10-22).
2. ~~Which rules refuse and which only report.~~ Settled: hearing authority and correction refuse, renewal is derived as
   their dependency, and everything else is reported ("Decisions", 2026-10-22).
3. ~~The vocabulary of causes.~~ Settled ("Decisions", 2026-10-22), to grow as Polar needs.
4. ~~Whether the groups document is checked too.~~ Settled: the same way as a policy, against the same rules ("Decisions",
   2026-10-22).
5. ~~The engine.~~ Settled: `ascent`, after the research recorded in "The engine" (2026-10-22).
6. ~~The credentials agent, or a policy agent of its own.~~ Settled: the credentials agent, as a crate of its own
   ("Decisions", 2026-10-22).
7. ~~What a refusal and an effect report look like, and where they are recorded.~~ Settled: an `invalid` outcome and
   an `AuthorityRefused` ledger event; the effect in `AuthorityPublished`, a summary on the outcome ("Decisions",
   2026-10-22).
8. ~~Persons.~~ Settled: every account in the credentials agent's ledger ("Decisions", 2026-10-22).
9. ~~Whether the render's projections are replaced.~~ Settled: the issuance floor is replaced when the model lands;
   cert-issuer's floor and the served workloads stay ("Decisions", 2026-10-22).
10. Whether the model also checks what the inventory found unchecked. (a) A groups document's keys: settled ("Decisions",
    2026-10-22). (b) Who publishes on the credentials agent's request topics: audited ("The consumer's request topics,
    audited"); the three that proved nothing were removed; whether the other thirteen are also judged by Cedar
    (`Action::"request"`, the floor being each topic's declared publishers) is open.

## Effect on stage 5b

Slice 5 as planned (hand-written probes in `validate_policy`) is not built. Slices 6 (the agent's side) and 7 (the
deploy and the measurement) do not depend on it and go ahead. Until the model is built, a `forbid` on an authority path
can lock a deployment out now that the broker enforces.
