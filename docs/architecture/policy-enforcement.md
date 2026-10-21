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

1. **No declaration of what an agent communicates.** A scaffold declares sections, commands, needs and the shutdown
   budget, but no topics and no access (`polar-agent-builder/src/agent.rs:242-412`); the catalog declares none either. So
   there is no source from which to derive a default policy, a certificate's permissions, or a seed.
2. **Three names for one agent.** Topics use the service name, documents are addressed to it, and the broker knows the
   identity, and for some agents they differ (`topics.md`). A rule needs one name, or a mapping it can trust.
3. **Certificates say little about a workload.** Workload certificates carry their identity and no `p:` names (a grant
   would add them, but grants are single-use and none are configured); `c:` capabilities are administrative and minted
   only for accounts (`cert-issuer-isolation.md`). The certificate fallback for a workload is therefore its identity alone,
   until certificates carry what the declarations say.
4. **No degraded mode** (the answer is decided, above; none of it is built). Before the decision there were two answers in
   the docs. Nothing tells "the credentials agent is unavailable" from "no
   policy", a policy once loaded never ages, and `admit` refuses (does not fall back) when its policy actor does not
   answer. `authority-distribution.md` designs aging that ends in a restart into init; `cert-issuer-isolation.md` describes
   running on the floor while the credentials agent is away; the model above says the certificate decides when
   infrastructure is unavailable.
5. **Cedar has no schema and no vocabulary for topics** (or pods). Policies are only parsed (256 KiB), so a misspelt
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
