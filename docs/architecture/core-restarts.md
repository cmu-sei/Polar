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

# Restarts of the core as normal operation

Status, 2026-10-23: the maintainer's design, written down with what exists today and what each part needs. The design's
own parts are not built; what they build on (cert-issuer as a Cassini client, its certificate record) is, as "What exists"
says.

## The design (the maintainer, 2026-10-23)

Polar is a distributed system: restarts, crashes and network splits are normal operating conditions, and some
situations are not normal, such as two or more of the core services being unavailable. The core is cert-issuer, Cassini
and the credentials agent (`cert-issuer-isolation.md`, "Decisions": the bootstrap set).

- **The floor is for bootstrap only.** It exists "to prevent anyone from talking to cert-issuer directly, once the core
  had brought the system to nominal operation" (`cert-issuer-isolation.md`). Reopening it after bootstrap has
  consequences: if the state after bootstrap is not the expected nominal one, reopening it may be the wrong thing. So it
  is reopened, and cert-issuer reached directly, only when that is clearly the right call.
- **One core service restarting is nominal.** Each detects that the system is past bootstrap and that its partners
  are available, comes up through them, and reports in:
  1. cert-issuer restarts: it tells that the state is nominal, comes up as normal, and reports "I'm alive again".
  2. Cassini restarts: it detects that the system is past bootstrap and that its partners in the core are available,
     uses them to initialize, and reports "I'm healthy again".
  3. The credentials agent restarts: it detects nominal, comes online, and reports "I'm available again".
- **Two or more core services offline or restarting is not nominal:** it is a bootstrap situation, and the core
  proceeds as in bootstrap.
- **The rest of the system rides out a core restart.** It can operate for some time degraded. Ideas from the same
  conversation, not yet decided:
  - the credentials agent may announce on a topic that it is restarting ("back in a moment");
  - an agent with Kubernetes privileges, beside the Reaper, could tell the others "the credentials agent has crashed:
    continue as normal while you can, or until your timers expire".

- **At nominal, cert-issuer is spoken to over Cassini (2026-10-23).** It has no dependency on Cassini but may speak
  over it (`cert-issuer-isolation.md`, "The model", revised). Once the system is nominal, the services that need it,
  such as the credentials agent, reach it over the same channels everything else uses. Its HTTP routes are then an
  extra footprint the broker does not see: no topic gate, no `published_by`, nothing in the graph.

- **The core rejuvenates like any other agent, on a plan its members make together (2026-10-23).** cert-issuer,
  Cassini and the credentials agent each rejuvenate, as every agent does. They know each other's schedules, and the
  schedule is flexible enough that two or more of them are never rejuvenating at once, which would be situation 4 made
  by the system itself. Their health check actors make the plan among themselves, over Cassini.

### What the rejuvenation plan must respect (read from the code, 2026-10-23)

- **Expiry is the hard edge.** An agent rejuvenates when its shortest-lived certificate is within the threshold of
  expiry (`polar-health/src/machine.rs:373`), and a certificate lives an hour (`values.dhall:53`, `:54`). The plan moves
  a member's restart earlier within that window, never past it: a peer that does not answer, or a plan not made, leaves
  each member on its own deadline, so coordination can never let a certificate expire.
- **Cassini's own turn is planned before it goes.** While Cassini restarts, nothing coordinates over it. So its slot is
  announced while it is up, and the others keep clear of it.
- **One turn at a time, decided the same way by each member.** Each announces when it means to go. A clash is settled
  by a rule every member applies alike (the earliest deadline goes first, then the name), so no member needs another
  to agree before it moves.
- **The same message tells dependents.** A member announcing its turn is the "I'm restarting, back in a moment"
  notice above, and its report on return is the "I'm alive again".
- **To settle when it is built:** whether the credentials agent's observer, also a bootstrap service, takes part; it
  is not one of the three, but the path to the credentials agent goes through it.

### What the HTTP routes are still needed for (read from the code, 2026-10-23)

Agreed, with two exceptions, which say how far the routes can close at nominal:
- **Bootstrap, and situation 4.** Cassini does not exist yet, so the routes are the only way in. They are the floor's
  channel, open exactly when the floor is.
- **Cassini's own restart (situation 2), which is nominal.** Cassini's new pod cannot ask over Cassini for the
  certificate it needs to run Cassini. So one route stays reachable at nominal, for Cassini's identity alone, with the
  credentials agent vouching for the request, unless a planned restart takes its successor's certificate before it
  ends (a crash cannot).
  - *Considered and set aside (2026-10-23):* Cassini escrowing a service identity at bootstrap and recovering it at
    restart. Recovering an escrowed key means asking the credentials agent over Cassini, which is the service that is
    down, and Cassini cannot establish trust before it runs. A certificate and key left on a volume for the next pod
    would put a private key at rest and would not cover a crash.
  - *Narrowed:* the route opens only while cert-issuer itself sees Cassini down (as a Cassini client it loses its
    session), closes when Cassini accepts it again, admits only Cassini's identity with the credentials agent vouching
    over mTLS, and every signing on it is in the certificate record, reported over Cassini once Cassini is back.
- **`GET /ca` is not needed at nominal.** The CA can be pinned in every pod from the `polar-ca` ConfigMap (the
  `pinned-ca` capability, `infra/config/capabilities.nuon:151`); without it, polar-init fetches `/ca` unverified and pins
  it (`polar-init/src/main.rs:45`). Once every workload pins the CA, the route serves only bootstrap.

So at nominal the routes close to everyone but Cassini's restart, and that use is made observable by the certificate
record: the credentials agent learns of what it signed, and says so over Cassini.

## What exists (read from the code and the docs)

- **How the core gets its certificates.** All three are bootstrap identities (`infra/config/workloads/credentials.nuon:34`,
  `:54`; `cassini.nuon:35`), signed by cert-issuer on their Kubernetes token alone at each start, by the floor
  (`cert-issuer-isolation.md`, "Built: the floor"). cert-issuer signs its own TLS certificate at start, with its own CA
  (`cert-issuer/server/src/tls.rs:95`). Client and server certificates live an hour
  (`infra/layers/2-services/cert-issuer/values.dhall:53`, `:54`). The credentials agent and its observer rejuvenate
  through the builder. How Cassini replaces its one-hour server certificate is not read yet.
- **The path to the credentials agent needs Cassini.** Everything that is not a bootstrap identity asks the observer over
  HTTP, and the observer relays to the consumer over Cassini (`credentials-agent-architecture.md`, "Everything else talks
  only to the Credentials Agent's observer"). That document accepts that "if the Credentials Agent or Cassini is
  degraded, no ancillary service can renew a certificate". So Cassini cannot come up through the credentials agent on
  that path: Cassini is the one restarting.
- **cert-issuer signs over HTTP, and speaks over Cassini (since 2026-10-23).** Callers reach its signing routes over
  HTTP: the credentials agent as its verified mTLS caller (`internal_callers`), the bootstrap identities on their
  tokens. Since `cert-issuer-isolation.md` step 1 it is a builder agent with a Cassini client that does not count
  toward its health (`DEPENDS_ON_CASSINI = false`, `cert-issuer/server/src/agent.rs`), and since step 2 it keeps a
  durable record of every certificate it signs (`CERT_ISSUER_RECORD_PATH`). It keeps nothing about the state the system
  was in, so after a restart it knows only what its record and Cassini tell it.
- **Degraded is already bounded for every other agent.** An agent keeps deciding on its cached policy, with the
  certificate where policy is silent, for 90 minutes by default (`POLAR_AUTHORITY_MAX_AGE_SECS`), then fails its health
  check and restarts into init. The credentials agent is exempt (`policy-enforcement.md`, "Decisions").
- **Nothing announces a restart.** At rejuvenation the credentials agent drains and unsubscribes from its request topics
  (`credentials-agent/consume/src/supervisor.rs`, `unsubscribe_from_topics`); nothing tells its dependents.
- **The pod observer and the Reaper are designed, not built** (`pod-reaper.md`). The pod observer is the agent that sees
  pods with Kubernetes privileges, apart from the one that acts on them, so it is where "the credentials agent has
  crashed" would come from.
- **Every certificate cert-issuer signs becomes known to the credentials agent** (`cert-issuer-isolation.md`, step 2,
  built 2026-10-23): cert-issuer records each signing before it returns the certificate, and the credentials agent asks
  for the record over Cassini (`cert-issuer-record-requests`, `-replies`) when it registers and every 30 seconds. That
  asking is also how each of the two can know the other is there: cert-issuer hears the credentials agent ask, and the
  credentials agent hears cert-issuer answer.

## What each situation needs

- **1, cert-issuer restarts.** It must tell nominal from bootstrap with no state of its own about it. Its record is on a
  durable volume, and with Cassini up it can connect as a client and hear the core, so "nominal" can be "Cassini
  accepts it and the credentials agent answers". It says "I'm alive again" over Cassini itself.
- **2, Cassini restarts.** It needs a certificate without the floor, through a partner, on a path that does not need
  Cassini. One shape: cert-issuer holds Cassini's request, and the credentials agent answers it on its next ask over
  mTLS, vouching for it as it vouches for every other workload. cert-issuer still decides nothing alone, and no new
  channel is needed. The credentials agent, while Cassini is down, knows Cassini is the one away, since its own client
  lost the session.
- **3, the credentials agent restarts.** Its new pod needs a certificate, and the credentials agent is the one that
  vouches. So either something else in the core vouches for it, with cert-issuer and Cassini up, or a planned restart
  (rejuvenation) hands over before it ends. A crash has no such handover. This is the hardest of the three.
- **4, two or more down.** Something must tell this apart from 1 to 3, from what cert-issuer and the survivor can see,
  without opening the floor on a guess.
- **The rest, degraded.** A "restarting, back in a moment" notice is something an agent can act on: hold its requests,
  not count the gap against its health. "It has crashed" needs a source with Kubernetes privileges (the pod observer).
  Both work inside the 90-minute bound, not beyond it.

## Open questions

To be taken one at a time, after the certificate record (`cert-issuer-isolation.md`, 2026-10-23), which the rest
builds on.

1. When the credentials agent restarts, who vouches for its new pod's certificate (situation 3)?

   *Prepared for the maintainer, 2026-10-23; not decided.* Read from the code:
   - The credentials agent is a catalog `agent`: a Job with `restartPolicy: Never` (`infra/config/kinds.nuon:37`). So a
     restart, planned (rejuvenation) or not (a crash), is a new pod with a new UID, created after the old one has
     ended. The two never overlap, so the old pod cannot vouch for the new one while both run.
   - Today the new pod's `polar-init` asks cert-issuer on its Kubernetes token alone, and the floor signs it, since the
     credentials agent is a bootstrap service (`infra/config/workloads/credentials.nuon:34`). So at nominal the floor
     is open for the credentials agent's identity at every restart. Since step 2 each such certificate is in
     cert-issuer's record and reaches the ledger within 30 seconds, so it is seen, but nothing stands between a token
     for that service account and a certificate.
   - cert-issuer can now tell the credentials agent is there: it hears its record requests over Cassini every 30
     seconds, and its own Cassini session tells it whether Cassini is up.

   The ways it could go:
   - **A. As built: the floor stays open for the core's own identities.** Simplest, and observable through the record.
     But it is the floor at nominal, which the design keeps for bootstrap.
   - **B. A planned restart hands over.** Before it ends, the outgoing credentials agent tells cert-issuer over Cassini
     that its successor may be signed once, for its identity, within a few minutes: a one-use, time-bound ticket,
     itself in the record. cert-issuer signs the new pod's token-only request against the ticket, and otherwise the
     credentials agent's floor entry stays closed. It covers rejuvenation, the common case. A crash leaves no ticket.
   - **C. A crash opens the entry narrowly, as Cassini's route does.** cert-issuer opens the credentials agent's floor
     entry only while it sees the credentials agent gone and the rest of the core up: no record request for longer
     than a bound (two missed asks), while its own Cassini session holds. That is situation 3, not 4. It closes again at
     the first request from the new credentials agent.
   - **D. Something with Kubernetes privileges vouches.** The pod observer (`pod-reaper.md`, designed, not built) could
     say which pod is the credentials agent's new one. It needs an agent that does not exist yet.

   Recommendation: B for rejuvenation and C for a crash, with A in force until they are built. Both keep the entry
   closed at nominal except for one identity, briefly, for a reason cert-issuer can see, and every signing is in the
   record.
2. What tells "two or more down" (4) from one restarting (1 to 3), and who decides it?
3. The notice to dependents: a topic the credentials agent publishes before it ends, the pod observer's report of a
   crash, or both; and what an agent does on each.
