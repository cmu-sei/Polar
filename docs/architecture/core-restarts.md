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
- **The attestation agent and the Reaper are designed, not built** (`runtime-attestation.md`, `pod-reaper.md`). The
  attestation agent, which takes over the pod observer's role (2026-10-26), is the agent that sees pods with Kubernetes
  privileges, apart from the one that acts on them, so it is where "the credentials agent has crashed" would come from.
- **Every certificate cert-issuer signs becomes known to the credentials agent** (`cert-issuer-isolation.md`, step 2,
  built 2026-10-23): cert-issuer records each signing before it returns the certificate, and the credentials agent asks
  for the record over Cassini (`cert-issuer-record-requests`, `-replies`) when it registers and every 30 seconds. That
  asking is also how each of the two can know the other is there: cert-issuer hears the credentials agent ask, and the
  credentials agent hears cert-issuer answer.

## What each situation needs

*Since 2026-10-28 an ending restarts the same pod in place* (`deployments.md`, step 4): "its new pod" below is that pod,
restarted, under its UID, `polar-init` again; a pod deleted, evicted or lost with its node is still replaced by a new one,
under a new UID. The credentials agent's own restart goes through its successor ticket ("Step 5, the ticket", below).

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
  not count the gap against its health. "It has crashed" needs a source with Kubernetes privileges (the attestation agent).
  Both work inside the 90-minute bound, not beyond it.

## Closing the floor: where it stands (read 2026-10-26)

The aim: the floor open only at bootstrap and when two or more of the core are down, and closed at nominal.
- **Done.** The floor is enforced on `sandbox-local` and `local-minimal` (`floor_enforced: true`): only the three
  bootstrap identities are signed on a token alone. Every signing is in cert-issuer's record and reaches the credentials
  agent's ledger within 30 seconds (`cert-issuer-isolation.md`, step 2).
- **Decided, not built.** The credentials agent's restart (question 1: B, C and the instance check); the observer's and
  Cassini's (a held request, above); Cassini's route narrowed to while cert-issuer sees Cassini down ("What the HTTP
  routes are still needed for").
- **The keystone, open.** The floor has no state: `admit_token_only` (`cert-issuer/server/src/floor.rs:107`) admits a
  bootstrap identity at any time. Closing it at nominal needs cert-issuer to tell bootstrap from nominal, and to tell
  when two or more are down: question 2.
- **`GET /ca` is still needed at nominal, by Cassini alone.** Only the bootstrap services ask cert-issuer directly
  (`Issuance::CertIssuer`, `polar-init/src/main.rs:305`), and of them the credentials agent and its observer pin the CA
  (`pinned-ca`, `infra/config/workloads/credentials.nuon:38`, `:55`); Cassini's `polar-init` fetches `/ca` unverified.
- **The observer path trusts what it is handed (found 2026-10-26).** Every other workload is issued through the
  credentials agent's observer over plain HTTP (`issue_cert_via_observer`, `polar-init/src/main.rs:744`), and writes
  the CA chain that comes back as its `ca.pem`, checked against nothing. Whoever can intercept that exchange could hand
  a pod a CA of their own, or, since the session offer the token is sealed to is not authenticated, swap in their own
  key and unseal the token ("how the requester authenticates the credentials agent in the negotiation", left open in
  `cert-issuer-isolation.md`, "Settled, and what is left").
- **Smaller, on record.** `floor_enforced` is `false` by default (`infra/config/cluster.nuon:55`), which the
  maintainer's rule calls a gap, not an option. The `local` target cannot enforce it yet: it still deploys three
  hand-written charts that ask cert-issuer directly (gitlab, build-processor, openapi; `infra/targets/local/target.nu:48`).
  No NetworkPolicy (layer 4, "Network isolation, with a fallback"; the dev cluster runs on the fallback, decided
  2026-10-13).
- **`/validate` needs no decision of its own.** With the floor set, a token-only caller that is not a bootstrap identity
  is refused on `/validate` as on `/issue` (`cert-issuer/server/src/handler.rs:91`); its user, the credentials agent,
  calls it over mTLS. So it follows the floor's state (`cert-issuer-isolation.md`, open question 3, answered by the
  floor).

### The plan (cleared by the maintainer, 2026-10-26)

Each step tests first, with property tests where an invariant holds.
1. **Every workload pins the CA, and the observer path is verified against it** (widened by the maintainer,
   2026-10-26, from pinning alone). `pinned-ca` in the `agent` kind, and in the charts that run `polar-init` (Neo4j,
   the ACE job); a `resolve.nu check` rule that nothing runs `polar-init` unpinned. `polar-init` verifies every
   certificate it is issued against the pinned CA (signed by it, naming its identity), and the credentials agent signs
   the session offer with its certificate's key, which `polar-init` verifies against the pinned CA, and the signer's
   identity, before it seals its token to it. `GET /ca` then serves only a cold start. In three parts:
   - *1a, pinned everywhere.* The `agent` and `service` kinds give `pinned-ca` (the `ca` kind, cert-issuer itself, does
     not: it is the CA); the Neo4j and ACE charts mount `polar-ca`; `polar-init` on the observer path requires
     `POLAR_INIT_CA_CERT`, and the check rule refuses a workload that runs `polar-init` without it.
   - *1b, every issued certificate verified.* `polar-init` judges what it is issued against the pinned CA, as it already
     judges a bundle it keeps (`cert-client`, `output::existing_bundle`: signed by the CA, valid now, naming its identity
     and every requested name, the right key usage, its own key), and writes the pinned CA as `ca.pem`, never the chain
     it was handed. A bundle that falls short is refused.
   - *1c, the offer signed.* By the rule that the credentials agent never uses one keypair for two protocols
     (`credentials-agent-architecture.md`, "Cryptographic material: two separate keypairs"), and the precedent of the
     escrow key's own certificate type (`CertType::Service`, `cert-issuer/common/src/lib.rs:62`), the offer is signed
     with a key of its own, not the mTLS key: a signing keypair the consumer makes in memory at start, certified by
     cert-issuer as a new type (`CertType::Signing`: digital signature only, no TLS use), which cert-issuer signs only
     for an internal caller's own identity, over `/internal/issue`. The offer carries that certificate and a signature
     over a domain-separated encoding of the session id, the public key and the lifetime; `polar-init` checks the
     certificate chains to the pinned CA, is of the signing type, names the credentials agent
     (`POLAR_INIT_CREDENTIALS_AGENT_IDENTITY`, rendered from the catalog), and that the signature holds, before it
     seals. 1c's two sides deploy together.

   *1c in detail (settled while building it, 2026-10-26).*
   - *The signing type is kept from TLS by an extended key usage, not by having none.* rustls-webpki takes a
     certificate with no extended key usage for client and server authentication alike (both are
     `required_if_present`, rustls-webpki 0.103 `verify_cert.rs`), so a certificate made like `CertType::Service`'s,
     with none (`cert-issuer/server/src/ca.rs:283`), would serve as an mTLS certificate naming the credentials agent.
     `CertType::Signing` carries `digitalSignature` and the one extended key usage `id-kp-documentSigning`
     (1.3.6.1.5.5.7.3.36, RFC 9336), which no TLS stack takes for either role; `polar-init` requires both, and no
     TLS usage.
   - *The offer has a domain of its own.* `polar-signing` signs and verifies under a domain line the caller names
     (`sign_in`, `verify_in`); configuration documents keep `polar-config-document-v1`, and the offer is signed under
     `polar-session-offer-v1`, so neither signature can be passed off as the other. What is signed is
     `{session_id, public_key, valid_for_secs}` (the key as its 32 bytes, as the offer carries it) in `polar-signing`'s
     canonical encoding; the offer carries the signature block (algorithm, value, chain) beside the three fields,
     which travel as before.
   - *Ed25519, in memory, renewed.* The workload issuance actor makes the keypair when it starts, and asks
     `/internal/issue` for a signing certificate for its own identity, read off its own mTLS certificate
     (`extract_own_identity`, the first DNS name, which is what cert-issuer reads off the connection). It renews at
     half the certificate's life with a new keypair, and tries again every few seconds while cert-issuer cannot
     answer. Until it holds a certificate valid now, it answers a session request with `Busy`, which a workload
     already waits out.
   - *cert-issuer.* `/issue` refuses `Signing` to every caller, vouched or not; `/internal/issue` signs it only for the
     caller's own identity. It lives `server_lifetime`, as `Service` does: the agent's own, renewed by it.
   - *polar-init stops on an offer that does not verify*, before it seals, as 1b stops on a certificate that does not:
     a pod that crash-loops is seen, where one that waits quietly is not. It reads the credentials agent's identity
     from `POLAR_INIT_CREDENTIALS_AGENT_IDENTITY`, required on the observer path, which the catalog renders from the
     `credentials-agent` workload (`<name>.<namespace>.serviceaccount.<domain>`, as `model_facts` writes identities)
     and the Neo4j and ACE charts take through `credentialsAgentIssuerEnv`.
   - *Found on the way: an escrow certificate is an mTLS certificate too.* By the same rustls rule, a `Service`
     certificate, which has no extended key usage and carries its agent's permissions, authenticates as that agent to
     Cassini, so whoever holds an escrow private key (the agent, and the escrow it is kept in) can connect as the
     agent. 1c does not change it; it is on record in `remaining-work.md`.

   *Built (2026-10-26): 1c,* in code and tests, not yet on a cluster. `polar-signing` signs and verifies under a
   named `Domain` (`document.rs`; one line, checked when it is made); cert-issuer has `CertType::Signing`
   (`cert-issuer/common/src/lib.rs`, `OID_DOCUMENT_SIGNING`), refuses it on `/issue` and signs it on `/internal/issue`
   only for the caller (`handler.rs`); `credentials-agent-common`'s `session_offer.rs` has the signer and the check;
   the consumer's workload issuance handler signs each offer, and its actor gets the first signing certificate before
   it takes a request and renews it (`renewal_delay`, at half its life, never sooner than `SIGNER_RETRY`); `polar-crypto`
   checks the offer before it seals (`RequestError::Untrusted`), and `polar-init` stops on one. The catalog renders
   `POLAR_INIT_CREDENTIALS_AGENT_IDENTITY` (`resolve.nu`, `identity_of` and `resolve.nu identity`), and `render.nu`
   gives it to the Neo4j and ACE charts. Tests: a rustls check that a signing certificate is refused for TLS client
   and server use (`ca.rs`); property tests for the domain (`polar-signing/tests/props.rs`), the offer
   (`credentials-agent/common/tests/props.rs`: believed as offered, and no change to it believed) and the renewal
   schedule; `infra/tests/resolve.nu` and `charts.nu`. Mutations: all on the new code caught but one, the consumer
   dropping a session whose offer it could not sign, which no test can reach (a signer's key is checked against its
   certificate when it is made). Its two sides deploy together: a new `polar-init` refuses an old consumer's unsigned
   offers.

   *Validated on sandbox-local, 2026-10-26* (a fresh `just reset` and `up.nu --dev` from `645f23f8`, with the escrow
   certificate's EKU fix): the consumer's workload issuance actor was issued its signing certificate before it took a
   request (digitalSignature and 1.3.6.1.5.5.7.3.36, on the internal path, 12 h under sandbox-local's `server_lifetime`,
   renewal set for 21600 s); all 16 pods on the observer path (Neo4j and the ACE job among them) were rendered with
   `POLAR_INIT_CREDENTIALS_AGENT_IDENTITY` and were issued their certificates, which `polar-init` asks for only after the
   offer's signature holds, and none refused an offer. OpenSSL takes the signing certificate for neither TLS role. An
   offer from someone else is refused by the tests, not on the cluster, which would need someone in the middle of the
   observer's HTTP.

   *Built (2026-10-26): 1a and 1b,* in code and tests, not yet on a cluster. The catalog's `agent` and `service` kinds
   give `pinned-ca` and the `ca` kind does not (`infra/config/kinds.nuon`; the per-workload entries that said it are
   gone); the Neo4j and ACE charts mount `Constants.polarCaVolume` and set `POLAR_INIT_CA_CERT` through
   `credentialsAgentIssuerEnv` (`infra/schema/functions.dhall`); `polar-init` requires the CA on the observer path
   (`environment.rs`), judges what it is handed with `cert-client`'s `judge_bundle` (`output.rs`, split out of
   `existing_bundle`, which now takes the pinned CA too), writes the pinned CA as `ca.pem`, and stops when either
   check fails. Tests: `infra/tests/resolve.nu` (every workload with `polar-init` pins the CA, cert-issuer none),
   `infra/tests/charts.nu`, `cert-client`'s `output_tests.rs`, and `polar-init`'s, among them an observer that signs the
   CSR with another CA and is refused with nothing written. Mutations: all on the new code caught; `main` and
   `issue_cert`, which this only passes the pinned CA through, have no tests of their own.

   *Validated on sandbox-local, 2026-10-26* (a `up.nu --dev` run from `9998acbc`, the node's kube-proxy fixed): all
   14 running pods with `polar-init` pin the CA (`POLAR_INIT_CA_CERT` and the `polar-ca` ConfigMap mounted), the 11
   issued through the credentials agent (Neo4j and the ACE job among them) and the 3 bootstrap services alike;
   each of the 11 logged an issuance whose expiry was read off the certificate as it was verified against the
   pinned CA, with no refusal; and the `ca.pem` a workload holds is the pinned CA (fingerprint equal to the
   ConfigMap's, checked in three pods). A refusal of a certificate from another CA is shown by the unit tests, not
   on the cluster, which would need someone in the middle of the observer's HTTP.
2. **cert-issuer's floor state.** A pure state machine over what cert-issuer hears (the record empty or not, its Cassini
   session, the credentials agent's presence with time), answering which bootstrap entries are open, by the table in
   question 2; `admit_token_only` asks it. Properties: no message opens an entry; for any sequence of events, what is
   open follows the table; after a restart with a record, nothing opens before the credentials agent is heard or the
   bound passes.

   *2 in detail (settled with the maintainer, 2026-10-26).*
   - *What "the record is empty" means.* cert-issuer signs its own TLS certificate and its own client certificate at
     every start, before it serves anyone, so the record is never literally empty. A fresh cluster is a record with
     nothing in it but cert-issuer's own (the `Own` path; its client certificate is now recorded there too, below),
     read when it starts.
   - *A fresh cluster is bootstrap until the credentials agent is first heard.* The record stops being empty at
     Cassini's first certificate, long before nominal; were that the end of bootstrap, the observer's entry would close
     in the middle of it. So on a fresh start every entry is open until the credentials agent is first heard, and from
     then the table applies. On a start with a record, nothing opens until the credentials agent is heard or the bound
     (60 s) passes; then the table applies.
   - *Shadow mode first.* Step 2 feeds the state (its Cassini session, the credentials agent's record requests, time)
     and logs, at every token-only request, what it would decide beside what the floor did; `admit_token_only` obeys it
     from step 3, with the held requests, since the state alone would close the observer's and Cassini's entries with
     nothing to take their place. The log is also what step 8 checks the state against.
   - *cert-issuer's own entry: self-issuance only (the maintainer, 2026-10-26).* The table covers the credentials
     agent, its observer and Cassini; cert-issuer's own entry was open over HTTP at all times, so whoever held a token
     for its service account could be signed a client certificate as cert-issuer. cert-issuer now signs its own client
     certificate in process at start (recorded as `Own`, as its TLS certificate is), learns its own workload from its
     own token as it does, and refuses that workload to every HTTP caller, on a token or vouched. This is in force now,
     not shadowed: nothing but cert-issuer itself ever used the entry.

   *Built (2026-10-26): 2, in shadow.* `cert-issuer/server/src/floor_state.rs`: `FloorState` hears
   `CassiniRegistered`, `CassiniLost` and `CredentialsAgent`, and says what is `Open` (`Nothing`,
   `CredentialsAgentOnly`, `All`) at a given instant; `Open::admits` says whether a request for a bootstrap identity
   gets through. The supervisor reads "fresh" when it opens the record, feeds the state from its Cassini session and
   from each record request the broker says the credentials agent published, and logs every change; the handler logs,
   at each token-only request for a bootstrap identity, `floor_state`, `would_admit` and `floor_admitted`.
   `Handler::handle_own` signs cert-issuer's own client certificate (path `Own`), and `handle` refuses its workload.
   Tests: the table in examples, and three properties (`cert-issuer/server/tests/floor_state.rs`: what is open
   follows the table for any history; no message opens an entry, now or later; after a restart with a record nothing
   opens within the bound unless the credentials agent is heard); self-issuance (recorded as its own, refused to every
   HTTP caller after); the shadow line, captured from the log. Mutations: all on the state and the handler caught; the
   supervisor's feeding has no unit test (it needs the whole agent) and is checked on the cluster from its log.

   *Seen on sandbox-local, 2026-10-26* (a redeploy from `16c4791a`, no reset): cert-issuer read its start as one with
   a record (`fresh=false`). Flux replaced the core one member at a time (cert-issuer 02:28:31, Cassini 02:29:31, the
   credentials agent and its observer 02:30:01 UTC), and the old credentials agent asked for the record until it was
   replaced, so the state held `Nothing` throughout: the new Cassini (60 s after cert-issuer's start), the new observer
   and the new credentials agent each asked on their tokens and would have been refused (`would_admit=false`), where
   the floor admitted them. That is the table: once enforced, the first two are held requests the credentials agent
   vouches for (step 3), and the credentials agent's successor waits out the bound after its predecessor falls silent
   (step 4). Hearing the credentials agent is shown by this (`Nothing` past the bound); the Cassini session's events
   changed nothing here to log, and are left to step 8.
3. **Held requests.** A token-only request for a closed entry (the observer's, Cassini's) is held, not refused, and
   `polar-init` waits on it. cert-issuer offers it to the credentials agent over Cassini, and over mTLS to the
   credentials agent's poll while its Cassini session is lost (the poll is also its presence, question 2). The
   credentials agent vouches by its own rules, and cert-issuer signs. Steps 2 and 3 deploy together: the state alone
   would close the observer's and Cassini's entries with nothing to take their place.
   *3 in detail (cleared by the maintainer, 2026-10-26).*
   - *The gate.* A token-only request for a bootstrap identity is signed only when the floor admits it and the floor
     state does; one the floor admits and the state does not is held. Anything the floor refuses is refused as before.
     With no floor configured nothing changes.
   - *Held, then asked again (the maintainer's choice).* `/issue` answers a held request at once with 202 and
     `IssueOutcome::Held`; polar-init sends the same request again (the same CSR and key, its token validated again)
     every few seconds, logging that it is held, until it is signed or refused. No connection is held open. A held
     entry is keyed by workload, pod UID, type and the CSR's public key, so asking again finds it; at most 64 are held,
     each forgotten 10 minutes after it was last asked for; only a bootstrap identity can be held at all.
   - *What the credentials agent is told.* What cert-issuer validated (the identity, the pod UID, the type, the names,
     the CSR), never the token.
   - *Who vouches.* For the observer and Cassini, the credentials agent, by its own rules: a bootstrap service in its
     authority model, one certificate per pod instance and type, and the attestation agent's instance check once it
     exists (step 6); cert-issuer still checks the names against the floor and writes in the floor's topics and
     groups, as for any bootstrap service, and records the signing as `Vouched`. A refusal is final. The credentials
     agent's own successor is vouched for by no one: it waits until the state opens its entry (step 4), or a ticket
     (step 5).
   - *Two ways to the credentials agent.* Over Cassini, cert-issuer offers each held request on
     `cert-issuer-held-requests` (again each time it hears the credentials agent, so a lost one is offered again), and
     takes answers on `cert-issuer-held-replies` only when the broker says the credentials agent published them. While
     the credentials agent's Cassini session is lost, it polls cert-issuer over mTLS (`GET /internal/held`) and answers
     there (`POST /internal/held/<id>`); each poll counts as its presence, so a Cassini restart completes with nothing
     but cert-issuer and the credentials agent up.
   - *One answer to a request, however often it is offered (2026-11-01, found in the fifth set's broker restarts,
     `broker-baseline.md`).* Being offered again is how a lost answer is recovered, and the voucher used to decide
     each offer afresh: its first decision admitted the pod under the renewal rule, so an answer lost with a session
     that ended (or still in the voucher's outbox while the broker was away) was followed by "too soon" for the same
     request, which cert-issuer takes as final. The voucher now keeps the answers it gave (`Decisions`, the last 256,
     keyed as cert-issuer keys what it holds: identity, pod, type and CSR, not the offer's id, which a cert-issuer that
     restarted gives anew) and gives each request its first answer again, over Cassini and over mTLS alike. Another
     CSR from the same pod within the bound is still refused. Tests: `held_voucher.rs`, the same request offered again
     and listed again over mTLS, and a property (each request one answer, a pod one vouch of each type); and
     `cert-issuer/server/tests/broker_restarts_test.rs`, cert-issuer run whole against a broker restarted twelve times
     in a row, orderly and at once by turns, each time as a held request's offer reaches the credentials agent, which
     was refused at the first or second restart before the change.
     *Superseded the same day by the general rule* (`deployments.md`, "A retry is not a second request"): the renewal
     rule admits the same CSR's key again within the bound, so the voucher decides each offer afresh again and the
     memory is gone. The tests above stand, on real CSRs, and `broker_restarts_test` passes without the memory.

   *Built (2026-10-26): 3, and with it the floor obeys step 2's state.* cert-issuer: `held.rs` (the table: keyed by
   workload, pod, type and CSR, at most 64, kept 10 minutes from the last ask, the first answer final), `gate.rs`
   (`FloorGate`: the state, the table, and the offer through the supervisor), the gate in `Handler::gate_on_token`, the
   routes `GET /internal/held` and `POST /internal/held/<id>` (`server.rs`), and the supervisor offering on
   `cert-issuer-held-requests`, offering again at each record request, and taking answers on
   `cert-issuer-held-replies` from the credentials agent alone; both topics are in its `TOPICS`. `IssueOutcome::Held`
   is 202. `cert-client` has `HandshakeError::Held`, `held_list` and `held_answer`; `polar-init` waits on a held
   request with the same CSR (`obtain_from_cert_issuer`), and the `cert-client` binary exits 2 on one, as on
   unreachable. The credentials agent: `held_voucher.rs` (the rule, and the actor that answers over Cassini, and
   polls over mTLS every 10 s while its Cassini session is lost). Tests: the table in examples and as a property (bounded,
   the same request the same entry, the first answer stands, only the unanswered offered again); the gate in the
   handler (held, offered once, signed once vouched for and recorded `Vouched`; a refusal final; admitted by the
   state on its token; the credentials agent's successor held, offered to no one, and admitted once its entry opens;
   neither a vouched request nor an open floor held; a full table; the mTLS routes for the credentials agent alone,
   and its poll as its presence); the client's parsing and routes; polar-init asking again with the same CSR, and
   stopping on a refusal; the voucher's rule, its publisher check, the ledger's renewal rule, and its polling only
   while the session is lost. Mutations: all on the new code caught.

   *First run on sandbox-local, 2026-10-26* (a redeploy from `d5e9157a`, no reset): the gate did as the table says.
   cert-issuer restarted with a record; Cassini's new pod was held and offered (03:22:38), but the credentials agent
   still running was the one built before step 3, with no voucher, and fell silent when Cassini went down, so 60 s on,
   with the credentials agent and Cassini both absent, the state opened everything and Cassini was admitted on its
   token (03:23:33); Cassini back, the state closed again (`All` to `CredentialsAgentOnly`, then `Nothing` as the old
   credentials agent was heard). The new observer was held and offered (03:24:08); the credentials agent's successor
   was held, offered to no one, and admitted once the bound had passed since its predecessor fell silent (03:25:03),
   which is step 4. That successor then could not start: its ledger had been forked by two writers during the redeploy
   before (`remaining-work.md`, "Two credentials agents appended to one ledger"), so nothing vouched for the observer.
   Vouching over Cassini and over mTLS is left to the next run, from a fresh cluster with both sides on step 3.

   *Validated on sandbox-local, 2026-10-27* (a fresh cluster from `bbf35777`, each core Job then replaced the way Flux
   replaces one, `kubectl replace --force`, with both sides on step 3):
   - *The observer, vouched for over Cassini.* Its new pod's request was held and offered (10:16:34.102), the credentials
     agent vouched 1 ms later, and polar-init's next ask, 5 s on, was signed.
   - *Cassini, vouched for over mTLS.* The credentials agent lost its Cassini session (10:17:18.7); Cassini's new pod was
     held (10:17:19.1) with no broker to offer it on; the credentials agent's mTLS poll listed it and vouched
     (10:17:27.4), and Cassini was signed at its next ask (10:17:29.1). Its offer, published while the broker was down,
     reached the credentials agent once the client was back (10:17:30.4) and was answered again; the request was signed
     by then, so the answer was not taken.
   - *The credentials agent, its own successor.* The old pod drained in 5 s (SIGTERM 10:18:20.1, its ledger stopped and
     the process gone at 10:18:25.1) and wrote nothing after the signal. The new pod's request was held, offered to no
     one, twelve times, and admitted once the bound had passed since the old one was last heard (10:19:20); the ledger,
     opened at 10:19:21, replayed whole (372 entries, one chain), and the floor state closed again as it was heard.
     That is step 4 as well. The ledger lock was not waited on: the old process had long gone.

4. **The crash window (C)** falls out of steps 2 and 3: the credentials agent absent while the Cassini session holds
   opens its entry alone. Until the ticket exists, a rejuvenating credentials agent's successor comes through it too,
   after waiting out the bound.
5. **The ticket (B), after the move to Deployments** (`runtime-attestation.md`, "Deployments, next"): a pod restarted
   in place keeps its UID, so the ticket can name the successor's pod and not only its identity. Decided with the move's
   plan (`deployments.md`, question 5, the maintainer, 2026-10-27): built after the move, and the credentials agent's
   successor is attested at runtime as every pod is.
6. **The attestation gate.** cert-issuer asks the attestation agent about the pod behind every token-only request
   whenever it is reachable; until the attestation agent is built it is not, and the floor behaves as option A. The
   attestation agent itself is built from `runtime-attestation.md`.
7. **`floor_enforced` true by default,** once `local`'s three hand-written charts are catalog workloads.
8. **On a fresh sandbox-local:** each core member killed in turn, and two at once, checking what opened against the
   table, in cert-issuer's log, its record and the ledger.

## Step 5, the ticket: the plan (2026-10-28, cleared, its questions decided, built)

`deployments.md`'s build step 6. What was decided (question 1, B, 2026-10-26; with the move, `deployments.md`, question
5): a planned restart hands over; the outgoing credentials agent tells cert-issuer over Cassini that its successor may
be signed once, within a few minutes; the ticket is in the record; a crash leaves none and goes through its window (C,
step 4); attestation decides who gets through, when it exists (step 6).

### What was read, and what the move changed

- *How the successor comes through today* (seen on `u7c`, 2026-10-28, `deployments.md`, "On the cluster"): its
  polar-init asks cert-issuer on its token alone for its client certificate and its custodian's, both bootstrap
  signings; the floor state (`cert-issuer/server/src/floor_state.rs`) holds them until the credentials agent has gone
  unheard for the bound (`PRESENCE_BOUND`, 60 s) while Cassini holds (`Open::CredentialsAgentOnly`), then signs. The
  restart in place took 45 s of holding.
- *What the move changed:* the successor is the same pod, under the same UID, and cert-issuer already knows that UID:
  its record (`record.rs`, `RecordEntry::instance_binding`) keeps the pod each signing was for, the credentials agent's
  current certificate included. So a ticket need not carry the UID, nor the agent learn its own: cert-issuer binds a
  ticket to the instance of the last certificate it signed for the credentials agent. That answers the gap found after
  the decision ("the Job makes the new pod only once the old one has ended ... so the outgoing agent cannot name it"):
  the ticket now admits one pod, not any pod of the service account.
- *What it does not close:* whoever can create a token for the credentials agent's service account bound to that pod
  (`serviceaccounts/token`) can ask inside the ticket's minutes. *Not Polar's to defend (the maintainer, 2026-10-28):*
  such a token is signed by the Kubernetes API, which is Polar's trust base (`runtime-attestation.md`, "Not a gap").
- *Where it would sit:* the outgoing side in the consumer's drain (`credentials-agent/consume/src/supervisor.rs`,
  `begin_drain`, on `PrepareShutdown`, before `ProceedWithShutdown`, while its Cassini session holds); the incoming side
  in cert-issuer's gate (`gate.rs`), beside the held requests (`held.rs`), whose topics show the precedent: published by
  the credentials agent, taken only when the broker says the credentials agent published them
  (`cert-issuer-held-replies`).

### The design

1. *The ticket:* the credentials agent publishes it on a new topic, `cert-issuer-successor-tickets`, as its drain
   begins; it carries only when it was issued. cert-issuer takes it only when the broker says the credentials agent
   published it, and binds it to the pod UID of its last client certificate for the credentials agent, from its record.
2. *What it admits:* a token-only request for the credentials agent's identity whose token is bound to that UID, once
   for each type the successor asks for (its client certificate and its custodian's), within five minutes of the ticket
   (the renewal bound: `deployments.md`, item 2). The signing is recorded with a path of its own, `Ticketed`, as a
   vouched one is `Vouched`.
3. *In the record:* the ticket itself is an entry, as decided, so the ledger learns of it with the signings; cert-issuer
   restarted inside the five minutes reads it back.
4. *What else is unchanged:* without a ticket (a crash, a pod deleted or evicted, a ticket expired or used), the crash
   window (C) as today; with one, a request bound to another pod is not admitted by it, and waits for the window as
   before; the observer's and Cassini's held requests as they are.
5. *The floor's topics:* the new topic in the credentials agent's and cert-issuer's own (their `TOPICS` and the floor
   entries, `topics.md`), so the broker carries it between them alone.

### For the maintainer

1. *A message that opens.* `floor_state.rs` says "No message opens an entry ... What opens is an absence, and time", for
   question 2's reason: "A rule that opened it on being told would be a rule an attacker could tell." A ticket is a
   message that opens the credentials agent's entry, for one pod, for five minutes. Taking it only from the credentials
   agent's own session meets the reason (whoever can publish as the credentials agent already holds its key, and could
   sign as it), but the principle as written would change. *Or,* with no ticket: cert-issuer admits the credentials
   agent's successor bound to the same pod as its last certificate, once it has gone unheard (the window's absence, so
   no faster than today, but only that pod, until the window opens for any). *Recommendation:* the ticket as decided,
   with the principle restated as "no message opens an entry except the credentials agent's own ticket, for its own
   pod". It is what makes a rejuvenation quick and asks the outgoing agent's consent. **Decided (the maintainer,
   2026-10-28): the ticket,** and the principle restated so (`floor_state.rs`'s module comment with it).
2. *Which endings leave a ticket.* Every orderly ending (a rejuvenation, and a SIGTERM's drain), or a rejuvenation
   alone. A pod deleted is replaced by a new UID, which a ticket bound to the old one cannot admit, so a ticket left by
   every drain admits nothing more. *Recommendation:* every orderly ending: one path, and a SIGTERM to a pod that then
   restarts in place (an operator's, a test's) is quick too. **Decided (the maintainer, 2026-10-28): every orderly
   ending.**

### Tests first

- cert-issuer: a property over sequences of tickets (from the credentials agent or another publisher), requests (the
  identity, the pod UID, the type) and time: a request is admitted against a ticket exactly when a ticket from the
  credentials agent is under five minutes old, bound to that UID, and that type has not been signed against it;
  otherwise what the window and the held requests decide, as now. Examples: the ticket in the record, and read back
  after a restart; the signing recorded `Ticketed`; a ticket with no certificate for the credentials agent in the
  record binds to nothing.
- The credentials agent: the ticket published as the drain begins, before `ProceedWithShutdown`, and only once.
- polar-init: unchanged (it asks as it does).
- Mutations: the publisher check dropped; the UID check dropped; a ticket used twice; one used after five minutes.

### Built (2026-10-28), before the cluster

- *The ticket's words* (`cert-issuer/common/src/ticket.rs`): `SUCCESSOR_TICKETS_TOPIC`, `SuccessorTicket { issued_at }`,
  `TICKET_LIFETIME_SECS` (300).
- *The record, its own entry kind (the maintainer, 2026-10-28)*: `TicketEntry` (the pod, when it was taken and expires,
  when the credentials agent says it issued it) is a line of cert-issuer's log of its own, numbered with the
  certificates; a page holds both, by their one numbering and one limit, and `RecordReply::tickets` is empty from a
  cert-issuer before tickets; `IssuePath::Ticketed` for a signing against one. cert-issuer: `Record` reads and writes
  both kinds of line, `last_client_binding`, and `RecordingCa::take_ticket`, which takes a ticket only from the
  credentials agent and only when its last client certificate names a pod.
- *What a ticket admits* (`cert-issuer/server/src/ticket.rs`, `Tickets`): the bound pod, a client and a custodian's
  certificate once each, until it expires; a failed signing gives its type back; restored at start from the record's
  last ticket and what was signed against it. The gate holds it (`FloorGate::install_ticket`, `admit_ticket`), the
  handler asks it before the floor state for the credentials agent's identity (and its custodian's), and records the
  signing `Ticketed`. The supervisor takes tickets on the topic, records and installs them.
- *The floor's principle* restated in `floor_state.rs`, as decided.
- *The credentials agent*: on `PrepareShutdown`, before its drain lets go of anything, it publishes the ticket
  (`leave_successor_ticket`), once; it declares the topic as one it publishes of its own accord. Its ledger keeps a
  reported ticket in `CertificatesReported::tickets` (an old ledger replays with none), read through once by number.
- *Found on the way, fixed apart:* `served` and the catalog topic test still asked for kind `agent` alone, so step 4's
  `run` kind had cost the scheduler its harness topics (`05a1bdbf`); and since custody step 9 the credentials agent's
  four escrow topics had no other side in the catalog, now said to wait for custodians (`97554be2`).
- *Tests:* the record (bound to the last client certificate's pod; refused from anyone else, with nothing to bind,
  unreadable; one numbering and one page limit; read back after a restart); the table (examples, the record's
  restoring, and a property: an ask is admitted exactly as the rule says, over any sequence of tickets, asks and
  givings-back); the handler (the successor signed at its first ask and recorded `Ticketed`, once for its type; another
  pod's token held); the credentials agent's ticket readable by cert-issuer and declared; the ledger reporting a ticket
  once, and an old report replaying. Mutations caught: the pod check, the once-only check and the expiry dropped in the
  table; the publisher check dropped (caught only once its test gave the other publisher a certificate of its own); the
  ledger reporting a ticket again. Not tested apart: the supervisors' few lines between the topic and the table, on each
  side, which the cluster run exercises. The workspace: 2853 passed, none failed.

### On the cluster

On `u7c`: the credentials agent sent SIGTERM, as on 2026-10-28: its successor signed at its first ask, against the
ticket, with no hold, under the same UID, the ticket and both signings in the record and the ledger; a pod of the
credentials agent's service account with another UID, asking inside the five minutes, not admitted by the ticket; the
pod deleted instead (a new UID): the window, as today.

*Done (2026-10-28), on the five-node `u7c`* (`up.nu sandbox-local --dev` at `20c5972e`):
- *A rejuvenation's successor, through its ticket:* the credentials agent sent SIGTERM at 19:37:11; its ticket was taken
  at 19:37:14 (bound to its pod, `5f3d19e6…`); the successor, the same pod, was signed at its first ask at 19:37:22,
  client and custodian both "admitted on its token against the credentials agent's successor ticket", and ready at
  19:37:23. Twelve seconds, against a minute with 45 s held before the ticket (`deployments.md`, "On the cluster").
- *Another pod of the service account, inside the ticket's minutes:* a bare pod running polar-init as the credentials
  agent (`zz-ticket-impostor`, UID `5ba9d08b…`, a key-file certificate), asking every 5 s, then the credentials agent
  ended again: the impostor asked twice between the ticket and the successor's signing, with the types unused, and
  was held both times; held 13 times in all, signed never; the successor came through the ticket again.
- *A pod deleted* (a new UID, so no ticket of its): held 11 times, admitted once the crash window opened, 56 s on, as
  before.

## Open questions

To be taken one at a time, after the certificate record (`cert-issuer-isolation.md`, 2026-10-23), which the rest
builds on.

1. When the credentials agent restarts, who vouches for its new pod's certificate (situation 3)?

   **Decided (the maintainer, 2026-10-26): B for rejuvenation and C for a crash, with A in force until they are built.**
   Why it is needed at all: the floor is for bootstrap, but every restart of the credentials agent (each rejuvenation,
   about hourly with one-hour certificates, and each crash) is a new pod signed by the floor, so the floor is in
   practice open at nominal for the most trusted identity there is. Anyone who can produce a Kubernetes token for the
   credentials agent's service account (by running a pod as it, or creating a token for it) could get that certificate
   from cert-issuer directly; the record would show it within 30 seconds but not stop it. The maintainer chose to close
   that even against someone with that Kubernetes access.

   Prepared for the maintainer, 2026-10-23. Read from the code:
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
     say which pod is the credentials agent's new one. It needs an agent that does not exist yet. (Taken up as the
     attestation agent, 2026-10-26.)

   Recommendation: B for rejuvenation and C for a crash, with A in force until they are built. Both keep the entry
   closed at nominal except for one identity, briefly, for a reason cert-issuer can see, and every signing is in the
   record.

   **Found after the decision (2026-10-26): B and C narrow the exposure, they do not close it.** Inside a ticket's
   window or a crash's, cert-issuer still signs on a token alone, and nothing tells it which pod is the real successor:
   the token names the service account and the pod's UID, but the Job makes the new pod only once the old one has
   ended (`podReplacementPolicy: Failed`, read on sandbox-local), so the outgoing agent cannot name it in a ticket. Any
   pod running as that service account could get there first, and whoever can delete pods can open C's window at will.
   What B and C buy: the entry is shut outside those minutes, and a race is visible (two credentials-agent
   certificates in the record for one restart, or the real pod refused). The maintainer (2026-10-26): what closes it is
   runtime attestation.

   **Runtime attestation: what Polar has on it (read 2026-10-26).**
   - *The original design*, the Attestation Service (`docs/architecture/attestation_and_issuance.md`, deleted in
     `1bbea735` when cert-issuer's spec replaced it; its sequence survives as
     `sequences/general/attestation_and_issuance.mmd`). A service gets a short-lived certificate only by proving three
     things: it comes from a trusted build (its build hash, looked up in the trusted build record and the registry), the
     request is fresh (a one-use nonce, expiring in 60 s), and it comes from an actual running instance (an ephemeral
     secret, `HMAC(master, instance + challenge)`, binding the request to one instance; then the attestation service
     introspects the host, by SSH, to confirm that instance is running that image). Rate limits and logging besides.
   - *What cert-issuer kept and set aside* (`cert-issuer-architecture.md`, sections 3 and 12). Kept: the host is not a
     trust anchor; pod identity comes from signed assertions of the Kubernetes control plane (the projected token, with
     the pod's UID as the instance binding, `instance_binding_claim`, `cert-issuer/server/src/config.rs:93`), which is
     what ruled out SSH. Deferred to "v2", each with a trigger: cosign verification of the requester's image; workload-image
     attestation (the init container passes the workload container's image digest from the downward API, and cert-issuer
     verifies it); the challenge-response with HMAC binding; TPM or SPIRE node attestation for bare metal.
   - *The other sense of it* (`credentials-agent-architecture.md`, "Protecting the escrowed private key"): runtime
     attestation as the "already-named piece of this system's own architecture, not yet built" that would help operators
     detect a live, compromised process misusing its key. That is attestation of a process while it runs, not at issuance.
   - *What already attests runtime facts.* The Kubernetes consumer's "attestation ladder for container images"
     (`kubernetes/consume/src/lib.rs`): only the kubelet's `imageID`, the digest actually running, is trusted for what a
     container runs, never the spec's tag. The pipeline's `binding_digest` ties a binary to its source and SBOM
     (`lib/src/events.rs`, `BinaryLinkedPayload`). Every image in a deployment is pinned by digest, and the signed
     `provenance.json` records the image lock (`cluster-bootstrap.md`, step 9).
   - *What the control plane can attest about the requester, read on sandbox-local.* The credentials agent's pod is owned
     by `Job/credentials-agent`, runs as service account `credentials-agent`, and its spec and its status agree on the
     digest it runs (`credentials-agent-consumer@sha256:03a5e274…`, the same in `spec.containers[].image` and
     `status.containerStatuses[].imageID`). So "the pod this token was bound to exists, is the Job's own, and runs the
     image the signed deployment names" is something the API server can say, not the host.
   - *Not built:* any of it at issuance. cert-issuer verifies the token and the floor's names; it does not look at the
     pod.

   **Decided (the maintainer, 2026-10-26): both an instance check at issuance and runtime attestation of the process,
   by an attestation agent and a node sensor** (`runtime-attestation.md`). B and C decide when the credentials agent's
   floor entry is open; the attestation agent's instance check decides who gets through it.

   **The observer's restart, decided (the maintainer, 2026-10-26): a held request.** The credentials agent's observer
   is a bootstrap identity too (`infra/config/workloads/credentials.nuon:54`), and question 1 did not cover it. Its new
   pod cannot go through the credentials agent, since the observer is the relay to it. So when its token-only request
   comes, cert-issuer does not sign: it holds the request and asks the credentials agent over Cassini; the credentials
   agent has the attestation agent check the pod and vouches for it, as for every workload, and cert-issuer signs. The
   observer's floor entry is closed at nominal, with no window. The same mechanism serves Cassini's restart, with the
   credentials agent taking the held request over mTLS, since Cassini is down. What is left of token-only signing at
   nominal is the credentials agent's own successor, ticketed (B) or in its crash window (C), and attested.
2. What tells "two or more down" (4) from one restarting (1 to 3), and who decides it?

   *Prepared for the maintainer, 2026-10-26; not decided.* Read from the code:
   - **What cert-issuer can hear.** Its Cassini session (`ClientEvent::Registered` and `TransportError`,
     `cert-issuer/server/src/supervisor.rs:305`), and the credentials agent's record requests over Cassini, every 30
     seconds. The credentials agent calls it over mTLS (`/internal/issue`) only when it signs for someone, not on a
     schedule. Its record is durable, so after a restart it knows whether it has signed anything before, but nothing
     says the system ever reached nominal.
   - **While Cassini is down, cert-issuer hears nothing of the credentials agent,** since the record requests go over
     Cassini. So "Cassini down" (2) and "Cassini and the credentials agent down" (4) look the same to it, unless the
     credentials agent reaches it another way. The held request for Cassini's restart already needs one: the
     credentials agent taking the request over mTLS while its own Cassini session is lost.
   - **A long Cassini outage becomes 4 by itself.** The credentials agent gives up after 120 seconds without Cassini
     (`POLAR_HEALTH_CASSINI_DEADLINE_SECS`, `polar-health/src/vars.rs:120`) and restarts; a Cassini restart was
     measured at 27 to 29 seconds (the same place).
   - **No message may open the floor.** A rule that opened it on being told would be a rule an attacker could tell.

   A rule that follows from these, with cert-issuer deciding, since it holds the floor:
   - *Presence.* The credentials agent is present while cert-issuer has heard it within a bound: a record request over
     Cassini, or, while Cassini is down, its mTLS call for held requests. Two missed asks (60 seconds) is the bound.
   - *Bootstrap:* the record is empty (a fresh cluster), or the credentials agent is absent and so is Cassini. Every
     bootstrap identity's entry is open.
   - *The credentials agent restarting (3):* absent, while cert-issuer's Cassini session holds. Its entry alone opens
     (C), or earlier against a ticket (B).
   - *Cassini restarting (2), or the observer:* the credentials agent present. No entry opens; held requests.
   - *cert-issuer restarting (1):* on start with a record that is not empty, it holds token-only requests for up to the
     bound, until it hears the credentials agent (nominal) or does not (bootstrap).

   The ways the decision could go:
   - **A. cert-issuer decides alone, from what it hears, as above.** Nothing can talk it into opening the floor; an
     outage can. Whoever can delete both the credentials agent's and Cassini's pods can force the floor open.
   - **B. A, and every token-only signing is attested whenever the attestation agent is reachable.** The attestation
     agent reaches cert-issuer over mTLS, not only over Cassini (`runtime-attestation.md`, "Open"), so in situation 4
     too each new core pod must pass the instance check. Only a cold start, with no attestation agent yet, signs on the
     bare floor, and that is attested after the fact.
   - **C. After the first bootstrap, situation 4 needs an admin.** cert-issuer keeps the floor shut and an admin
     reopens it with a signed instruction, which cert-issuer verifies against the cluster CA. The strongest against a
     forced outage, and the slowest: the system waits for a person.

   Recommendation: B. It needs no person and no new trust, and a forced outage then gains the attacker nothing unless
   it also takes down the attestation agent.

   **Decided (the maintainer, 2026-10-26): B, the rule plus attestation** (this question's B, not question 1's ticket).
   cert-issuer decides from what it hears, by the rule above, and every
   token-only signing passes the attestation agent's instance check whenever the attestation agent is reachable over
   mTLS. Only a cold start signs on the bare floor, and that is attested after the fact.
3. The notice to dependents: a topic the credentials agent publishes before it ends, the attestation agent's report
   of a crash, or both; and what an agent does on each.
