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

# Policy enforcement: the model, where it is enforced, and the gaps

Written 2026-10-21. What the system is meant to do, as the maintainer stated it, what each enforcement point does in the
code today, and what stands between the two. The detail of each part is in the doc it links to; this one is the map.

## The model (stated 2026-10-21)

- **Nominal:** the credentials agent is available, the scheduler publishes signed Cedar policy, and decisions rely on
  Cedar first. Where no policy speaks to a request, the decision falls back to the certificate.
- **Degraded:** when required infrastructure is not available, decisions fall back to the certificate.
- **Default deny.** Anything not allowed by policy or by the certificate is refused.
- **The broker enforces who may talk to whom about what:** it identifies each agent or user from its certificate,
  reasons about what it may do, and decides each subscribe and publish. Who may talk to whom is partly readable from
  certificates today; which topics a service may publish or subscribe to comes from policy.
- **Where the policy comes from.** An agent declares, in its scaffold, the topics it wants and the access it wants on
  each (publish, subscribe). From the scaffold come the catalog's entries, the seeds, and the default policy templates
  for the agent's configuration; an administrator edits and signs them. Nothing is hand-rolled configuration.
- **Enforcement points:** the scheduler, the broker, the credentials agent, cert-issuer, and each agent itself, through
  the `PolicyActor` every builder agent has.
- **The first worked example** is the Kubernetes agent: the observer publishes observations on topics for its consumer
  only, listens on its configuration topics from the scheduler, requests a credential from the credentials agent and
  retrieves what was escrowed for its use alone. Any other agent's attempt to use its topics, or its attempt to use any
  other topic, is denied.

## Decisions

- **Degraded is bounded, then a restart into init (2026-10-21, gap 4).** When required infrastructure is not available,
  decisions fall back to the certificate, for a bounded time; past it, the agent fails its health check and restarts
  into its init container, which waits for the infrastructure. A cluster that has lost its authority does not run on
  certificates indefinitely. This keeps the aging design in `authority-distribution.md` and replaces the open-ended
  degraded state `cert-issuer-isolation.md` described.
- **In the window, the cached policy keeps applying (2026-10-21).** An agent decides as when nominal: the last policy it
  verified first, the certificate where that policy is silent. Only fresh policy stops arriving. A signed `forbid` does
  not lose force because its publisher is unreachable; the bound limits how stale it can get.
- **Bootstrap services start on the floor, and their clock starts at their first snapshot (2026-10-21).** The broker,
  cert-issuer and the credentials agent run before any authority exists (the credentials agent's own traffic, issuance
  and escrow go through the broker). They decide on the certificate floor from the start, which admits that traffic by
  identity, and are healthy without a snapshot. Once one has verified its first snapshot it ages like any agent: the
  cached policy in the degraded window, then the bound and a restart into init. The credentials agent is exempt from
  aging: it is the authority. Every other agent is unhealthy until it has verified a snapshot. The bound is 90 minutes
  by default (`POLAR_AUTHORITY_MAX_AGE_SECS`), tunable.
- **The workload name is an agent's one name (2026-10-21, gap 2).** Every agent passes its workload name to
  `AgentBuilder`, so its topics are `agent.<workload>.*` and its signed documents are addressed to `<workload>`; the
  builder enforces it at start. An identity maps to its agent mechanically, `<workload>.<namespace>.serviceaccount.<domain>`
  to `<workload>`, so the broker, Cedar and the floor join a certificate to an agent's topics without a lookup table. The
  agents that pass another name today change (`topics.md`).
  *Refined 2026-10-21:* the name is a constant in each agent's code, one binary per workload: every image runs as
  exactly one workload, and its constant is that workload's name. The builder checks at start that the name is its own
  certificate's identity (`<name>.<namespace>.serviceaccount.<domain>`) and refuses to start if not. The agents whose
  constant differs are corrected: kube-observer, kube-consumer, jira-observer, jira-processor, oci-registry-resolver,
  git-repo-processor, polar-scheduler, and the credentials agent (`credentials-agent-consumer` becomes
  `credentials-agent`, and its signed documents are addressed to that). The secret harness, test code that ran one image
  as two workloads, is built twice (`secret-harness-owner`, `secret-harness-intruder`), and is no longer deployed by the
  shipped targets: it is added to a target when it is what is being tested.
- **An agent declares its topics, and some are implied (2026-10-21, gap 1).** A `TOPICS` constant on `PolarAgent`, beside
  `NEEDS` and `SECTIONS`, lists each topic the agent uses, with its access (publish, subscribe) and a purpose. The topics
  every agent uses because of the builder (its own configuration and command topics, self-test, the authority topics,
  replay and outcome), and those a need brings (the credentials agent's request and reply topics, for `Encryption`), are
  implied, not declared by each agent. The condition: how the implied topics are derived is documented, and **the agent,
  the catalog and what reads it, and the scheduling scaffolds and what reads them, always arrive at the same set of
  declared and implied topics for every agent.** The way to hold that: the set is computed once, in the agent (the
  builder), and printed in full in its scaffold, each implied topic with the rule that implied it; nothing else
  recomputes it, and the builder refuses at run time to publish or subscribe outside it.
- **The agent's own client decides as the broker does: Cedar first, then the set (2026-10-21).** Before a publish or a
  subscribe leaves the agent, the builder's check asks the agent's PolicyActor; a `permit` admits a topic outside the
  computed set (an administrator's runtime addition, such as a workload the scheduler now serves, takes effect without a
  restart), a `forbid` refuses one inside it, and where no policy speaks the computed set decides. With no policy yet
  (before the first snapshot) the set decides. The check is a function the builder hands the Cassini client, so
  `cassini-client` does not depend on `polar-policy`; a refusal is logged in the agent and reported as
  `PublishRefused` or `SubscribeRefused`, and nothing is sent. It is the same rule the broker applies (stage 5), so an
  agent never sends what the broker would refuse for policy, and never refuses what policy permits. Until 2026-10-21
  the client consulted no policy at all (its `connect.rs` "Policy" is the connection retry setting); the PolicyActor
  was asked only for signed documents (`configuration.rs:261`), commands (`command_actor.rs:215`) and the credentials
  agent's requests.
- **Topics are exact names (2026-10-21).** No patterns. An agent that serves a family of topics (the scheduler, which
  publishes `agent.<workload>.configuration` for every workload) has its list generated from the catalog, one exact name
  per workload, so policy and the broker deal only in exact strings.
- **The certificate carries the floor; runtime Cedar policy is the ceiling (2026-10-21, gap 3).** Each workload's
  certificate carries its declared topics as `p:` names (a new form for topics, for example `p:pub:<topic>` and
  `p:sub:<topic>`), written at issuance from the catalog's declarations, by the credentials agent and, for the bootstrap
  services, by cert-issuer. The implied topics are derived from the identity, not written into each certificate. The
  broker reads the certificate and needs no table of its own. A changed set reaches an agent with its next certificate
  (within the hour, at rejuvenation).
  **Cedar overrides the certificate in both directions (settled; do not reopen):** a `permit` admits beyond what the
  certificate carries, and a `forbid` refuses what it carries; where no policy speaks, the certificate decides. This is
  the rule `authority-distribution.md` has held since 2026-10-12 ("Cedar overrides the certificate; the certificate is
  the fallback"), now applied to topics. "Floor" and "ceiling" mean which comes first, not that policy may only narrow.
- **The Cedar vocabulary for topics (2026-10-21, gap 5).** `Action::"publish"` and `Action::"subscribe"` on
  `Topic::"<exact name>"`; the principal as today, `Identity::"<certificate identity>"` with its permissions, capabilities
  and groups. The broker gives as facts the workload the identity maps to and whether the topic is in its declared or
  implied set. A schema, carried in the code that evaluates, validates a policy when it arrives (the broker, every builder
  agent) and before it is signed (`polar-sign`). The builder's run-time check and the broker ask the same question
  through the same `PolicyLookup`, so an agent and the broker cannot disagree about a topic.
- **Applying the catalog is the administrator's sign-off (2026-10-21, gap 6).** The administrator applies the catalog:
  its render and apply (on a Flux target, its signed publish, with provenance) are the approval of every workload's
  declared topics. There is no separate grants document. The render writes each workload's declared topics into the
  credentials agent's issuance floor, as it writes cert-issuer's floor of server names today, and into cert-issuer's for
  the bootstrap services; each certificate carries them from the first one issued. A change to an agent's topics is a
  change to the catalog, applied like any other, and reaches the agent with its next certificate.
- **Open: an administrator's scheduled override or addition to an agent's set.** Considered, not decided: a signed,
  scheduled document that adds topics to (or overrides) what the scaffold declares and implies. If it comes, it is one
  more input every consumer reads the same way, and the builder's run-time check must honour it.

## Where policy is enforced today

| Point | Cedar asked | Without a policy, or when it does not speak | Detail |
|---|---|---|---|
| Builder agents: signed configuration, runtime sections, commands | yes (`policy_authority::admit`) | the signer's certificate capability; any other policy error refuses | `authority-distribution.md`, "As built" |
| Credentials agent: retrieve, decrypt | yes | refused (no fallback, by design); no policy loaded is a failure, so the client retries | `credentials-policy.md` |
| Credentials agent: store | yes | its floor: the requester's own names | `credentials-policy.md` |
| Credentials agent: admin actions, issuance | no | the certificate capability, or the credential | `identity-and-bootstrap-update.md`, `cert-issuer-isolation.md` |
| Broker: subscribe, publish | no | nothing is decided: every client may use every topic | `cassini-broker.md` |
| Scheduler | no | signed documents are forwarded unverified (the receiver verifies); unsigned schedules flow unchecked | `topics.md` |
| cert-issuer | no | a static floor | `cert-issuer-isolation.md` |
| Polar git server | no | static lists of identities per repository | `git-server.md` |

## The gaps

Numbered so a plan can refer to them. "Core" gaps block enforcement anywhere; the rest are per component.

**Core**

1. **No declaration of what an agent communicates** (decided, above: declared and implied topics in the scaffold; not
   built). A scaffold declares sections, commands, needs and the shutdown
   budget, but no topics and no access (`polar-agent-builder/src/agent.rs:242-412`); the catalog declares none either. So
   there is no source from which to derive a default policy, a certificate's permissions, or a seed.
2. **Three names for one agent** (decided, above: the workload name; not built). Topics use the service name, documents are addressed to it, and the broker knows the
   identity, and for some agents they differ (`topics.md`). A rule needs one name, or a mapping it can trust.
3. **Certificates say little about a workload** (decided, above: they will carry the declared topics; not built). Workload certificates carry their identity and no `p:` names (a grant
   would add them, but grants are single-use and none are configured); `c:` capabilities are administrative and minted
   only for accounts (`cert-issuer-isolation.md`). The certificate fallback for a workload is therefore its identity alone,
   until certificates carry what the declarations say.
4. **No degraded mode** (the answer is decided, above; none of it is built). Before the decision there were two answers in
   the docs. Nothing tells "the credentials agent is unavailable" from "no
   policy", a policy once loaded never ages, and `admit` refuses (does not fall back) when its policy actor does not
   answer. `authority-distribution.md` designs aging that ends in a restart into init; `cert-issuer-isolation.md` describes
   running on the floor while the credentials agent is away; the model above says the certificate decides when
   infrastructure is unavailable.
5. **Cedar has no schema and no vocabulary for topics** (or pods) (decided for topics, above; not built). Policies are only parsed (256 KiB), so a misspelt
   attribute denies at evaluation; resource facts are the asker's to give; and no decision is recorded.
6. **Seeds and templates are not generated.** One seed exists (the credentials agent's `policy`); no `groups` seed, no
   per-agent templates from scaffolds, and `polar-sign runtime` (what the seed uses) skips the scaffold check; Cedar is
   not validated when signed.

**The broker**

7. **No authorization, and no policy in the broker.** No publish or subscribe decision; no `AuthorityHandle` (it is a
   `ServiceBuilder` service and cannot be its own client); the certificate is dropped after the first DNS SAN is taken.
8. **Bootstrap order.** The broker runs before the credentials agent, and the credentials agent's own traffic (authority
   distribution, issuance, escrow) flows through it. Its floor must admit that traffic before any policy exists, and no
   policy may cut off the authority channel itself. The authority key is pinned from the first snapshot (trust on first
   use).
9. **Refusals are invisible.** A client learns nothing it can act on: refusals are `warn!`s, never `ClientEvent`s, and a
   subscribe is never acknowledged on success. Subscriptions are sent again at every registration.
10. **Resuming a session by its registration id does not check the identity**, so the id is a bearer token.
11. **Shared reply topics.** Every crypto actor subscribes to every crypto reply topic; privacy is by encryption, not by
    topic, so topic rules cannot make replies private.

**Other points**

12. **The scheduler** forwards documents unverified, processes unsigned schedules, and answers replay requests from anyone.
13. **The credentials agent** decides admin actions and issuance by capability or credential only; `DecryptOnBehalfOf`
    takes an unbound, caller-asserted resource.
14. **cert-issuer and the git server** have static floors and no Cedar.

**The Kubernetes agent, for the worked example**

15. kube-observer has no crypto actor (no `Encryption` need), so it cannot retrieve a secret escrowed for it; its
    Kubernetes access is a mounted token with `*/*` get, list and watch, cluster-wide.
16. kube-consumer does not check `published_by` on the observations it reads.

## The build plan (2026-10-21)

Each stage is built test first, with property tests where a function has an invariant over its inputs (`AGENTS.md`,
"Testing"): stage 0, a refusal never asks the client to re-register and a resumed session never changes identity; stage 1,
an agent's set is deterministic, order-independent and the union of declared and implied; stage 3, a topic `p:` name
round-trips through a certificate and malformed names are refused; stage 4 and 5, a decision follows the rule (Cedar
first, then the floor, default deny) for any request; stage 6, the aging state machine never decides on an expired cache.
It ends with the repository's checks passing (`cargo check`, `clippy -D warnings`, `fmt`,
the package's tests, the infra tests), and is committed when they do. A stage names the gaps it closes. Nothing in a
stage relies on a later one; each leaves the cluster working, with enforcement switched on only in stage 5.

**Stage 0: what the later stages stand on** (gaps 2, 9, 10)

- The client turns refusals into events: `ClientEvent::SubscribeRefused { topic, reason }` and
  `PublishRefused { topic, reason }`, from the broker's refusal replies, which never contain "session mismatch" (the
  client re-registers on that). The broker acknowledges a successful subscribe. Tests: the broker harness, a refused
  subscribe seen by the `Collector`.
- Resuming a session checks that the connection's identity is the session's (`session.rs:595-622`); a mismatch is a new
  session, never the old one. Test: two identities, one registration id.
- One name: every agent's service name is its workload name, a constant in its code, and its `PolarAgent::NAME` is the
  same constant; the builder refuses to start when the workload its own certificate names is not that name. Tests: the
  builder's start check; `topics.md` updated.
  *Built 2026-10-21:* the refusal events (`e75bbc42`), the resume check (`c9969cd0`), and one name: the secret harness
  built twice (`ff2147ef`), seven agents renamed (`aca90ec1`), `NAME` from the service constant (`4c604db3`), the
  credentials agent as `credentials-agent` (`e53336c6`), and the start check (`workload_of` in
  `cert-issuer/common/src/identity.rs`, the inverse of `normalize_identity`; `polar-agent-builder/src/own_name.rs`,
  called from `ConfiguredAgent::build` before anything is spawned), each with a property test. Not checked: services
  that are not Cassini clients (`ServiceBuilder`: the broker), whose certificates are not a workload's identity. Not yet
  run on a cluster.

**Stage 1: agents declare their topics** (gap 1)

- `PolarAgent::TOPICS`: each topic, its access (`publish`, `subscribe`) and its purpose.
- The builder computes the agent's full set: declared, plus implied, each implied topic with the rule that implied it.
  The rules are written in `topics.md` ("Implied topics"), and are code in one place in `polar-agent-builder`.
- `--scaffold` prints the full set; `resolve.nu`, the catalog and `polar-sign` read it and never recompute it.
- The builder refuses, at run time, to publish or subscribe outside the set (an error in the agent's own log), asking
  the agent's PolicyActor first: a `permit` admits beyond the set, a `forbid` refuses within it, the set decides where
  policy is silent or there is no policy yet ("Decisions"). The client is given the check as a function. Cedar's
  vocabulary for topics is stage 4's, so until then no policy speaks about topics and the set decides; the check is
  in place and asks.
- Every agent declares what it uses today (`topics.md`'s inventory). Unused and mismatched topics are fixed or removed
  as they are met (the openapi pair's mismatch, the unused constants).
- Tests: per agent, the scaffold's set equals what its code uses (a test that runs the agent's subscriptions and
  publishes against a recording client); the builder's refusal; `infra/tests` reads the scaffold's set unchanged.

**Stage 2: the catalog carries the sets, and the render writes the floors** (gaps 1, 3, 6)

- `resolve.nu` reads each workload's set from its scaffold. The scheduler's family (`agent.<workload>.configuration`
  for each workload) is generated from the catalog as exact names: the render passes the list of workloads to the scheduler, which expands it
  in its computed set (open point A, decided).
- The render writes each workload's declared topics into the credentials agent's issuance floor and, for the bootstrap
  services, into cert-issuer's floor, as server names are written today. `render.nu` checks that every declared topic has
  at least one other side (a subscriber for a publish, a publisher for a subscribe), so a mismatch fails the render.
- Tests: the render of `local-minimal` and `sandbox-local`; the cross-check refuses a one-sided topic.

**Stage 3: certificates carry the floor** (gap 3)

- A `p:` form for topics in `cert-issuer-common` (`p:pub:<topic>`, `p:sub:<topic>`), parsed and written by the same
  module as the existing scopes; the implied topics are derived from the identity by one function, shared by the builder
  and the broker.
- The credentials agent writes a workload's declared topics into each certificate it issues, from its floor; cert-issuer
  does the same for the bootstrap services.
- Tests: issuance unit tests; a certificate's SANs for a catalog workload; the size of the largest set (open point C); and
  a certificate with a few hundred topic `p:` names presented to the broker (rustls) and to Neo4j (whose TLS stack, the
  JDK's or OpenSSL's, sets the tightest limit), to confirm the handshake limits rather than trust the defaults.

**Stage 4: Cedar knows topics** (gap 5)

- A schema in `polar-policy`, in code: `Identity`, `Group`, `Topic`, and the actions in use (`publish`, `subscribe`,
  `retrieve`, `store`, `decrypt`, `update-configuration`, `update-runtime`, `run-command`), with their attributes. A
  policy is validated against it on arrival (it is refused whole, and the old one stays, if it does not validate) and by
  `polar-sign` before signing.
- Facts for a topic request: the workload the identity maps to, and whether the topic is in its declared or implied set.
- Tests: the schema refuses a misspelt attribute; a permit and a forbid for a topic decide as stated.

**Stage 5: the broker enforces** (gaps 7, 8, 11)

- The Listener keeps the leaf certificate, and decides each subscribe and publish: Cedar first, through an
  `AuthorityHandle` the broker runs itself (fed the authority topics' publishes in process, with the publisher identity
  from its own Listener); where no policy speaks, the floor (the implied topics from the identity, the declared topics
  from the certificate). Default deny. A refusal is sent to the client (stage 0).
- The bootstrap services start on the floor, healthy without a snapshot; the authority channel and the credentials
  agent's request and reply topics are on every workload's implied floor, and the credentials agent refuses a policy that
  would deny them (open point B, decided; its test: a policy forbidding a workload `polar-authority-updates` is refused
  whole, and the old policy stays).
- Reply topics stay shared; their privacy is the payload's encryption, as now.
- Tests: the broker harness with certificates that carry topic `p:` names: an undeclared publish refused, a declared one
  delivered, a Cedar forbid refusing a declared one, a Cedar permit admitting an undeclared one.

**Stage 6: degraded is bounded** (gap 4)

- `POLAR_AUTHORITY_MAX_AGE_SECS` (default 90 minutes), the heartbeat (any verified message from the credentials agent),
  a snapshot asked for at 45 minutes, and on expiry a failed health check that restarts the agent into init.
- An agent that is not a bootstrap service is unhealthy until it has verified a snapshot; the bootstrap services start
  their clock at their first; the credentials agent does not age.
- Tests: an injected clock (the publisher's tests use paused time); the health verdict at the bound.

**Stage 7: the Kubernetes agent, end to end** (gaps 15, 16)

- kube-observer gains the `Encryption` need (a crypto actor), so it can retrieve what was escrowed for it alone.
- On a cluster: the pair declares its topics; the observer's observations reach the consumer; another agent publishing
  on the observer's topic is refused, and so is the observer publishing on a topic it did not declare; the observer
  retrieves a secret escrowed for it, and another agent cannot.

**Stage 8: the other enforcement points** (gaps 12, 13, 14)

- The scheduler verifies a signed document before forwarding it, refuses unsigned schedules, and answers a replay only
  for the requester's own workload.
- The credentials agent asks Cedar for admin actions and issuance, with the capability as the fallback; `DecryptOnBehalfOf`
  binds its resource to the ciphertext.
- cert-issuer and the Polar git server get an authority holder (service mode) and ask Cedar, with their static floors as
  the fallback.

**Open points, to settle before the stage that needs them**

- **A. The scheduler's family and the computed set** (stage 2). *Decided 2026-10-21:* the render gives the scheduler the
  list of workloads it serves as an input (an environment variable from the catalog); the builder reads it at start and
  expands it into exact names in the computed set, and `--scaffold` is asked with the same input, so every consumer still
  reads one computed set. The scheduler does not declare a pattern.
- **B. Can a policy cut off the authority channel?** (stage 5). *Decided 2026-10-21: no exception to the rule; a check on
  arrival.* Before the credentials agent accepts a new policy, it evaluates it against a fixed set of requests: every
  catalog workload's identity against the authority topics and the credentials agent's own request and reply topics. If
  any would be denied, the whole policy is refused with the reason, and the old one stays in force, as for a policy that
  does not parse or validate. Cedar keeps overriding the certificate both ways; a policy that would cut the channel through
  which policy is corrected, or certificates renewed, is never accepted.
- **C. The size of a certificate's topic list** (stage 3). *Decided 2026-10-21: no compact form, no hard limit.* The
  largest declared sets are the credentials agent consumer's (about 34 `p:` names: its 16 request and 16 reply topics and
  the two authority topics) and the scheduler processor's (one per workload, about 20 today); most agents declare 1 to 8.
  At 40 to 60 bytes a name that is about 2 KB, far inside what a TLS handshake carries (the tightest limit in the path is
  a JDK's default of 32 KiB for a handshake message, if Neo4j's TLS is the JDK's; rustls and Go allow 64 KiB, OpenSSL
  100 KiB for the certificate list). The render warns when a workload declares more than 64 topics, as a signal to look
  at its design, not a failure.

## Sequence diagrams

Under `sequences/`; each diagram says whether it shows the code as built or the design. Updated 2026-10-21 for these
decisions: `broker/client_registration`, `broker/client_reconnect`, `broker/client_subscription`, `broker/client_publish`,
`general/policy_update_enforcement`, `general/health_rejuvenation`, `general/configuration_startup`, the new
`general/topic_floor_issuance`, and `bootstrap/overview`, `bootstrap/phase1-before-flux`, `bootstrap/phase2-flux`, `bootstrap/phase3-after-flux` and
`general/signed_configuration_delivery` (which also lagged what was built).
