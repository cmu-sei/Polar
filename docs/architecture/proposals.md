# Proposals

Status, 2026-10-11: design, decided (see the end); the build has begun with the shared `type` in signed payloads. Direction from David: a proposal is how one agent tells another
"here is something to add to what you handle", without an administrator editing configuration by hand, and
the rules for accepting one are signed repository content that the agents interested in that kind read. This
is the second of the three kinds in `scheduling-kinds.md` (configuration, proposal, command); the sequence is
`sequences/general/proposal_flow.mmd`.

**Amended 2026-10-12 (`git-controller.md`):** for the git agents the **receiver is the git controller**, not the
observer (the observer keeps the host-binding check and the network guard, since it holds the credentials and
opens the connections), and **admission specifications are the `admission` section of a `runtime` document**
rather than a document type of their own. Everything else here stands. Where this file says "git observer" as the
receiver, read "git controller".

## The case that drove it

The Jira consumer finds a repository named in a Jira issue. It should be able to say so, and the git agent
should start observing that repository (pull it, collect statistics) without a person adding it to a list.

What exists today, read from the code, 2026-10-11:

- Nothing in the Jira agents detects a repository. The proposing side is new work.
- The git observer's repositories come from `RepoObservationConfig` messages (`git.repo.config.events`; the `git.yaml` file is gone),
  each naming the credential it needs (`credential`, 2026-10-14; it used to carry its own `credentials`). The git
  scheduler that sends them is slated for retirement.
- The broker tells a subscriber who published a message (`published_by`, from the publisher's verified mTLS
  certificate) whenever the publisher allows it, which is the default.
- `polar-policy` is a Cedar wrapper whose request takes an identity, an action and a resource as strings, with
  the principal's attributes resolved by `PermissionsActor`. It cannot evaluate "the host of this URL matches a
  pattern" or "no more than twenty an hour", because nothing lets a caller supply attributes of the resource.
- `RetrieveSecret { name }` gives a requester a secret that was escrowed under a name, re-encrypted to the
  requester's own certificate.

## What a proposal is, and is not

It is an event from an agent, about the world it observed, that asks another agent to take something on. It is
not configuration (nobody authored and signed it as desired state), and it is not an instruction (the receiver
may say no). It **adds to a set**, it is **idempotent by key** (the same repository proposed twice is one entry),
and the entry it creates is **durable and traceable**: who proposed it, on what evidence, when, and what became
of it.

An attacker can write anything in a Jira issue, so everything in a proposal's subject is untrusted text. The
design is mostly about what the receiver does with that.

## The parties

- **Proposer**: the agent that observed something (the Jira consumer). It publishes; it does not decide.
- **Receiver**: the agent that owns the kind (the git observer). It **decides**, by applying the admission rules
  it has been given. It does not trust the scheduler to decide for it, for the same reason a receiver of
  configuration verifies for itself: there must be no relay whose word is taken.
- **The scheduler processor**: the **ledger**. It writes every proposal and every decision to the graph, and
  replays the proposals to a receiver that has just started. It decides nothing.
- **Admission authors**: people who write and sign the rules.

## The proposal message

```
Proposal {
  kind:     "observe-repo"                   which set it is for; the receiver declares the kinds it owns
  subject:  { url: "https://..." , ... }     typed per kind; the receiver's declaration says how
  evidence: { source: "jira", issue: "ABC-12", field: "description" }   where it was found, for the trail
}
```

The proposer's identity is not a field: it is what the broker says (`published_by`). A proposal that arrives
with no publisher identity is dropped. The key of an entry is a function of the subject that the receiver
declares (for `observe-repo`, the normalised URL), so a repeated proposal updates `last_seen`, a count and a
bounded list of evidence, and does not make a second entry. It is published on a topic per kind
(`polar-proposal-<kind>`), and both the receiver and the scheduler processor subscribe.

## The receiver declares the kind

The schema export (`<agent> --scaffold`) gains a `proposals` section, next to the variables: for each kind the
agent owns, its subject fields and their types, how its key is derived, and the **schema of its admission
rules**. This is what makes the rules typed: the same template generation that gives an author a Dhall record
type for an agent's Dynamic variables gives them one for the admission rules of a kind, so a misspelt rule
fails before anything is signed.

## Admission specifications

A **new kind of signed repository document**, as David proposed: a specification of the parameters under
which a proposal of a given kind is accepted. It is addressed to a proposal kind, not to an agent, and an agent
that owns or cares about the kind subscribes to it (`polar-admission-<kind>`). It is signed, verified and
delivered exactly like a configuration document (same layout, same `polar-signing`, same chain to the pinned
CA, same authority rule in version one), with a payload of its own:

```
{ type: "admission", id, kind: "observe-repo", version, rules: { ... } }
```

Rules for `observe-repo`, as an example of what a kind's schema might declare:

| Rule | Meaning |
|---|---|
| `hosts` | host patterns a repository may be on (`github.com`, `*.example.org`); nothing outside is accepted |
| `organisations` | optional, per host: which organisations or paths |
| `proposers` | the identities allowed to propose this kind (as the broker names them) |
| `require_approval` | accepted entries wait until listed in `approved` |
| `approved` | URLs an administrator has approved |
| `denied` | URLs that are never accepted, and are removed if present |
| `credentials` | bindings from a host pattern to the **name** of an escrowed secret |
| `anonymous` | host patterns where no credential is needed. Decided 2026-10-15 (David): a separate list, so a host with neither a binding nor an entry here is `pending: no credential` and "this host is public" is a signed, deliberate statement |
| `limits` | most entries, and most proposals per proposer per hour |

Until a receiver holds a verified specification for a kind, it **accepts nothing**. It fails closed, and the
proposals it sees are recorded as `pending: no admission rules`.

Version one uses structured rules and an identity list for "who may propose". Cedar, through `polar-policy`,
is the right tool once the question is richer ("agent X may propose kind K to agent Y", per environment), but
as that crate stands it cannot express the host and rate rules above, so it comes later, for the part it can
do.

## Deciding

For each proposal the receiver applies, in this order, and records the first rule that decides:

1. A publisher identity is present, and is in `proposers` (if the rules list them).
2. The subject is well formed, and is **normalised**: scheme and host lower-cased, a default port removed,
   no fragment. A URL with user information in it (`https://user:secret@host/...`) is rejected, not
   stripped. Only `https` is accepted (never `ssh`, `git`, `file`, or git's remote-helper forms such as
   `ext::`), and the git invocation is restricted to it as well (`protocol.allow`).
3. It is not in `denied`.
4. The host matches `hosts` (and the organisation, if the rule names one).
5. **The network guard**: the host is resolved, and an address that is private, loopback, link-local, or in
   the cluster is refused, checked again at connect time so a name that changes its answer is caught. An
   issue that names `http://169.254.169.254/` or an internal service must never make an agent connect there.
6. The limits hold.
7. If `require_approval`, it is `pending: awaiting approval` unless listed in `approved`.
8. A credential binding for the host exists, or the host matches `anonymous`. If neither, it is
   `pending: no credential`.
9. Otherwise it is **accepted**.

Each outcome is `accepted`, `rejected` (with the rule), or `pending` (with what it waits for). A pending entry
is **re-evaluated** whenever the rules change, a binding appears or an approval is added: approval and
credentials are not new mechanisms, they are edits to a signed specification.

## Credentials

A credential is bound to a host pattern and **never tried on another host**: a crafted URL on the attacker's
host matches no binding and gets no credential. The binding holds the **name** of a secret escrowed with the
credentials agent, not its value, so the specification contains no secret. The receiver retrieves it at the
moment it needs it (`RetrieveSecret`, through its CryptoActor). The secret itself arrives by the route in
`signed-configuration.md`, "Secrets in a document". This replaces the `credentials` field carried inline in
`RepoObservationConfig`.

## Persistence, replay, removal

**Amended 2026-10-15 (David): proposals are soft state.** The receiver (the git controller) keeps the proposals it has
seen in memory only, and the proposers announce again, as the controller itself announces its desired set every 60
seconds. A restarted controller has its pending proposals back after the proposers' next announcement, and what it
needs from signed content (the rules, the approvals) comes back by the replay of signed documents. The graph shows
every proposal and decision for people to read, and **is never read back to rebuild the receiver's set**, which is the
rule for everything else (`git-controller.md`, "What has to exist first"). Wherever the text below says the processor
replays proposals to a receiver, read: the proposers re-announce. The ledger stays as a projection, not a source.

Agents here are short-lived Jobs without storage of their own, and the receiver must not be the only place a
proposal is remembered. So:

- The scheduler processor writes each proposal to the graph as it arrives (a `(:Schedule:Proposal)` node
  keyed by kind and key, with first and last seen, a count, the proposers and a bounded list of evidence), and
  each decision as an outcome node linked to it. This is the observability principle in `scheduling-kinds.md`.
- A receiver that starts asks for its kind's proposals and the processor **replays** them. The receiver then
  **re-evaluates** every one against the current rules, so its set is always a function of the proposals and the
  current signed rules, never of an earlier decision. Replay is the scheduler's reconciliation item.
- **Removal** is declarative: put the URL in `denied` (a signed edit). It is dropped on the next evaluation,
  and the ledger keeps the history. A removal that lived only in the receiver's memory would be undone by the
  next replay.

## Authenticity

Version one trusts the broker's statement of who published, which is the publisher's own mTLS identity and the
same attribution every Polar topic relies on. A proposal is about something an agent observed, from inside the
cluster, so it is not signed by a person. A proposer may later also sign what it proposes with its workload
key (the signing library already handles workload certificates' key types), which would make a proposal
verifiable without trusting the broker and would be kept in the graph as evidence. Not in version one.

## What changes in the code

- Jira processor: a publisher of `observe-repo` proposals.
- Git observer: the receiver, which replaces its `git.repo.config.events` path. It holds the set and re-evaluates on change.
- Scheduler processor: the ledger, and the replay.
- Builder: a proposal helper (declare a kind, receive, normalise through a hook, report decisions on the outcome
  path) so each receiving agent writes only its subject type, key and constraints.
- `--scaffold` and the typed templates: the `proposals` section and the admission schema.
- `polar-signing` and `signed.rs`: a document `type`. Configuration documents today have none; adding it is
  part of this (see the decisions).

## Built 2026-10-15: the decision (step 1)

`git/controller/src/admission.rs` is the decision as a pure function (`decide`), with the rules read from a signed
`admission` section body (`Rules::parse`) and the canonical form of a URL in `git-agent-common` (`canonical_repository`).
Tests were written first; thirty-eight deliberate breakages of it and thirteen of the URL reading were each caught. It is
not wired to anything yet. Decided while building, in the places this document was silent (David, say if you disagree):

- **What is passed on is the canonical URL**, never the text that arrived: only `https`, a host name (never an
  address, in any spelling), no login, no query, no `%` anywhere (it can hide a `/`, a `\` or a dot segment from one
  reader and not another), no empty path segment, at least one segment, no trailing dot on the host, at most 2048
  characters, a fragment dropped, a default port dropped, dot segments resolved, a trailing slash dropped.
  Two spellings of one repository have one id (`.git` or not).
- **Host patterns are matched by the same function the observer's CryptoActor uses** (`polar_crypto::registry`), so
  the controller never accepts a repository for a host the observer would refuse the credential for.
- **`organisations`** is per host pattern; the first path segment is compared without regard to case; a host no
  entry names is not restricted.
- **`hosts` and `proposers` are required** in the rules. A list of proposers that is empty lets nobody propose.
  **`require_approval` defaults to true.** `limits` default to 500 entries and 20 distinct repositories per proposer
  per hour. Every list is bounded at 1000.
- **Counting is the caller's:** `Standing.accepted_entries` leaves out the proposal's own entry, and
  `proposed_this_hour` counts distinct repositories (an announcement repeated every minute is not a flood).
- **The network guard allows only ordinary public addresses**: it refuses this network, private, shared,
  loopback, link-local (cloud metadata), documentation, benchmarking, multicast and reserved IPv4; for IPv6 it
  allows only global unicast (`2000::/3`) minus documentation, Teredo and 6to4, and refuses IPv4-mapped. One bad
  address among good ones refuses the name. A name that does not resolve is **pending**, not rejected: a failed
  lookup is not the proposer's fault, and the next announcement asks again.
- **A bound credential is preferred to `anonymous`, and the first binding that matches wins.**
- The guard here is at decision time. The observer must check again when it connects, so a name that changes its
  answer is caught (step 6); until then an accepted repository is only as safe as the answer at decision time.

## Build order, tests first

1. The decision procedure as a pure function: normalisation, each rule, the order, and an attack list (user
   information, `ext::`, `file://`, internal addresses and a name that rebinds, a lookalike host, a very long
   URL, a flood from one proposer). Tests before any wiring.
2. The admission document: layout, signature, delivery by kind, and failing closed before one arrives.
3. The processor's ledger and replay.
4. The builder helper and the git observer as the first receiver.
5. The Jira side.

## Decisions (David, 2026-10-11: all four confirmed)

1. **The receiver decides and the processor is the ledger**, as above, so that no relay is trusted. Confirm,
   or say if you would rather the processor decide.
2. **Admission specifications as a document type of their own**, addressed to a proposal kind. Confirm that
   "a distinct kind of scheduler repository" means this, and that where they live in the repository can be a
   directory of their own.
3. **Version one authenticates the proposer by the broker's identity**, with signing a possible addition.
4. **A `type` in every signed payload** (`configuration`, `admission`, `command`), required from now.
