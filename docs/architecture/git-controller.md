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

# The git agents: observer, processor, and a controller

Status, 2026-10-14 (design agreed with David 2026-10-12). The observer's `credentials` section and its use are built (2026-10-14, step 3: `git/observe/src/credentials.rs`); the minimal controller is (2026-10-14, step 4: `git/controller`, it announces the `repositories` section, with no proposals, admission, status or backoff); the observer's per-repository status is built (2026-10-15, see "Status and checkpoints"; the controller does not read it yet); the full controller and the checkpoints are not; the `runtime` document type, replay, `c:update-runtime`, `polar-sign runtime` and `RetrieveSecret` are (each section says which). It replaces the
git scheduler (deleted 2026-10-14; it had been migrated onto the builder on 2026-10-12 as it was, for the move only) and
settles three things that `proposals.md` and `signed-configuration.md` left open for the git agents: who
receives proposals, how a credential reaches the observer, and how per-repository settings travel.

## Why the git scheduler went

It was built before the credentials agent and the polar scheduler existed, and it holds what they now own:

- It decides defaults for a repository and reads `RepoObservationConfig` nodes from the graph that nothing
  writes. The schedule data belongs to the schedules repository, through the polar scheduler.
- Its configuration events carried `credentials` inline, always `None`. Credentials belong to the credentials
  agent; the event now names a credential (`credential`) and carries none. (Done 2026-10-14.)
- A repository whose worker failed to start is never started again, and nothing can say whether a
  repository is being observed as expected.
- The stored `max_depth` is handed on as the shallow fetch depth (the constructor's fourth argument).

What is left for a control plane is real, and is not observation or processing: deciding **what should be
observed**, **by whom**, and **whether it is working**.

## The parts

| Part | Does | Does not |
|---|---|---|
| **Observer** (`git-repo-observer`) | Clones, fetches, walks commits, publishes commit and ref events. Registers its credentials with its CryptoActor and uses one only through it, which enforces that a credential goes only to a host its binding names. Enforces the network guard. Reports per-repository status. | Decide what to observe. Touch the graph. |
| **Processor** (`git-repo-processor`) | Writes commits, refs and status to the graph. Answers the observer's checkpoint request. | Decide anything. |
| **Controller** (`git-repo-controller`, new; replaces the scheduler) | Builds the desired set of repositories from signed runtime documents and admitted proposals. Receives proposals and decides them. Tells the observer what to observe. Watches status, retries, backs off, paces. Later: assigns repositories to observers. | Hold a credential or see a plaintext one. Hold the authoritative copy of any schedule. |
| **Polar scheduler** | Carries signed documents from the schedules repository, writes the ledger (every document, proposal, decision and outcome) to the graph, replays to an agent that has just started. | Decide. |

The controller is a consumer of schedules. Restart it and it has nothing of its own: it asks for what the
polar scheduler holds and rebuilds. Normally control-plane work sits in a processor, but the git processor is a
graph writer and the controller is not, and mixing them would give one agent both the graph password and the
power to decide what is observed.

## Flows

### The repository set (permanent items)

```
author --signs--> schedules repo --> polar scheduler observer --> scheduler processor (ledger, graph)
                                                                         |
                                           agent.git-repo-controller.configuration
                                                                         v
                              controller: verify, validate against its declared section,
                              hold the latest, report the outcome  -->  desired set
                                                                         |
                                                  git.repo.config.events (existing topic)
                                                                         v
                                                                    observer
```

### A proposal (what the Jira consumer used to be going to send)

```
Jira consumer --polar-proposal-observe-repo--> controller  (receiver: decides)
                                          \--> scheduler processor (ledger: records, decides nothing)
controller: accepted | rejected(rule) | pending(awaiting approval | no credential | no admission rules)
administrator: pushes a signed edit (an approval, a credential binding)  --> controller re-evaluates
```

The proposer publishes once. Both the receiver and the ledger hear it from the broker, so the controller does
not forward anything to the polar scheduler, and nobody has to trust a relay. This is `proposals.md`, with one
change: the receiver is the controller, not the observer. The observer does not take the controller's word for
the two checks it makes itself (below).

### A credential

```
author --encrypts to the observer's escrow public key, signs--> runtime document, section "credentials"
   --> observer: verify, register it with the CryptoActor under its name and binding; the CryptoActor decrypts it
                 (key in custody) and escrows it with the credentials agent (StoreSecret, retried until acknowledged)
later, at init: the CryptoActor retrieves (RetrieveSecret { name }) each credential the observer needs, before anything else
```

The controller never sees a credential. A repository names the credential it wants (`"credential": "github-sei"`);
the observer has the secret by that name (stored under its own identity as a prefix,
`<identity>/github-sei`; `credentials-policy.md`). Decided 2026-10-14 (David), step 3: it does not fetch it back with `LoadSecret`. The section is replayed to the observer every time it starts, which registers the credentials again; a fetch could bring back an older copy of a credential whose newer version failed to register, or one the section has since withdrawn. If the schedules repository is unreachable when the observer starts, its repositories stay pending until it is back. A repository whose credential the observer lacks is
`pending: no credential` in the status it reports, and becomes active when the credential arrives. Decided
2026-10-14 (David): the repository and its credential reach the observer by different routes, so the repository can
come first. Its worker does not fail: it stays pending and tries again when the `credentials` section changes and on
its own frequency tick (the status event itself comes later). A credential a later version of the section leaves out
is withdrawn: the CryptoActor drops it (`ReplaceSection`, `credentials-agent-architecture.md`) and a repository naming
it is pending again.

### Status and checkpoints

The observer publishes, per repository, on `polar.git.observation.status`: last successful fetch, last error,
current tips. The controller consumes it for liveness (stalled, failing, backing off); the processor may write
it to the graph for visibility.

**Built 2026-10-15 (observer and processor); seen on a cluster the same day for `observing`, `pending-credential` and `failing` with a credential bound to another host; a failed fetch and a failed clone are only unit-tested.** After every attempt, including one that
cannot go ahead, the observer publishes an `ObservationStatus` (`git-agent-common/src/status.rs`) on
`polar.git.observation.status`: `repo_id`, `state`, `reason`, `last_success`, `tips`, `consecutive_failures`,
`observed_at`. Only three states exist so far: `observing`, `pending-credential` (waiting is not failing, and
resets the count) and `failing` (the reason says whether the credential cannot be used for the host, the clone
failed or the fetch failed). `starting` and `stopped` are not emitted: a worker's first attempt follows its start at
once, and removal of a repository is deferred. Timestamps are epoch milliseconds, like every other
`observed_at` the observer writes (this document first said RFC 3339). A reason is made fit to show before it is
published (`sanitize_reason`: one line, no login in a URL, at most 300 characters); the observer never holds a
credential's plaintext, so none can reach it, but a remote's error text is not trusted to be tidy. A failed
**fetch** no longer ends the worker: it is a `failing` status and the next attempt (the frequency tick, or a change
of credentials) fetches again, as a failed clone already did. The git processor subscribes to the topic and writes the
latest word onto the repository's node (`observation_state`, `observation_reason`,
`observation_consecutive_failures`, `observation_checked_at`, and `observation_last_success` only when the observer
knows one, so a replaced pod does not erase its predecessor's); a reason is cleared when the next attempt has none.
The topic needs no broker rule: the broker's permission checks are on its control plane, not on topics. The
controller does not consume the status yet (liveness and backoff belong to path 2).

A starting observer asks the processor over Cassini (request and reply) for the last observed tip of each ref,
so a replaced pod does not walk history it already published. The processor reads the newest `POINTS_TO` edge
(`observed_at` is a string, so it compares as a number). The observer still never touches the graph.

## The `runtime` document type

**Built 2026-10-12** (the library; no agent declares a section yet): `document_type::RUNTIME`, the capability `c:update-runtime` (appended to the closed list), `UpdateVerifier::verify_document`, `PolarAgent::RUNTIME` with `RuntimeSection { name, doc, body, validate }`, handling in the existing ConfigurationActor (`runtime(section)`, `runtime_changes()`, the same history and outcome events), the `runtime` list in `--scaffold`, routing and per-section replay order in the polar scheduler. Not built: the typed Dhall templates generated from the declarations. (`polar-sign runtime` is built.) `admin-provision` now grants the first admin `c:update-policy` and `c:update-runtime` as well.

There are two kinds of configuration for these agents, and they need different authors:

- **Deployment configuration** (`type: configuration`): the agent's own Dynamic variables, such as the log level.
  Whoever may change it can change how the agent runs. Capability `c:update-configuration`.
- **Runtime configuration** (`type: runtime`): what the agent works on. Which repositories, under what rules,
  with which credentials. A policy author may change it and cannot touch the log level, the shutdown budget or
  anything about the deployment. Capability `c:update-runtime`.

It is processed by the ConfigurationActor, with no new machinery for the events themselves. The same topic
(`agent.<name>.configuration`), the same `Common` verification (the signature, the chain to the pinned CA, the
target, the revocation seam), the same outcome events and `ConfigurationOutcome` graph nodes. The actor
dispatches on `type`: `configuration` takes the path it has; `runtime` validates the body against the section
the agent declared, checks the section's version, keeps the latest, and tells subscribers.

```
payload = {
  type:    "runtime",
  id:      "<unique>",
  target:  { agent: "<name>" },
  section: "<a section this agent declared>",
  version: <whole number, rises per (agent, section)>,
  body:    <structured; typed by the agent's declaration>
}
```

Rules:

- **A section is replaced whole.** The latest version of a section is the state; there are no per-key patches,
  because a list of repositories is not patched. A removal is the next version without the entry.
- **Versions rise per agent and section**, so the `repositories` and `admission` sections move independently.
- **The agent declares its sections** in `--scaffold`, next to its variables (below). The same template
  generation that gives an author a Dhall record type for the Dynamic variables gives one for each section's
  body, so a misspelt field fails before anything is signed. A body that does not fit is refused, with the
  field named, and nothing is applied.
- **Secret fields** are marked in the declaration, are ciphertext in the document, and are bound to the target
  agent, section, field and version as authenticated data (`signed-configuration.md`, "Secrets in a document").
  A secret field is always encrypted to the document's own target agent: a document for one agent cannot carry
  ciphertext for another. That is why credentials for the observer get a document of their own, addressed to it,
  and are not tucked into the controller's `repositories` section. (Passing ciphertext through a second agent
  would need a document with two recipients, and the observer would refuse a document whose target is the
  controller. No use case justifies that machinery.)
- **The authority rule** for `runtime` is the capability `c:update-runtime`. It is a new variant of the closed
  capability list, so every consumer of cert-issuer capabilities has to be redeployed before a certificate
  carries it (old consumers reject an unknown variant as malformed). Cedar is asked first, per section
  (`update-runtime` on `Section::"<agent>/<section>"`, `authority-distribution.md`) and the capability is the
  fallback. A credentials document needs no capability of its own, and none is planned (David, 2026-10-14).
- **Admission specifications become a section.** `proposals.md` made `admission` a document type of its own,
  addressed to a proposal kind. Here it is the `admission` section of the controller's runtime documents, with
  the kind inside the body. One fewer type and one fewer path; the rules and the fail-closed behaviour are
  unchanged. `document_type::ADMISSION` can be retired when this lands.

## Sample data

### What `--scaffold` says: variables and runtime sections side by side

Today an agent's export lists `sections`, each a group of variables. A runtime section is declared next to
them, in a `runtime` list, with the same habit of saying what each field is for.

```json
{
  "agent": { "name": "git-repo-controller", "purpose": "Decides which git repositories are observed, by whom, and whether it is working" },
  "requires": ["cassini"],
  "sections": [
    { "name": "builder",  "capability": null, "variables": [ { "name": "POLAR_SHUTDOWN_BUDGET_SECS", "mutability": "static", "...": "..." } ] },
    { "name": "logging",  "capability": null, "variables": [ { "name": "RUST_LOG", "mutability": "dynamic", "default": "info", "...": "..." } ] },
    { "name": "cassini-client", "capability": "cassini", "variables": [ "..." ] }
  ],
  "runtime": [
    {
      "name": "repositories",
      "doc": "The repositories to observe, and how. The latest version replaces the one before.",
      "body": {
        "repositories": { "list_of": {
          "url":            { "type": "https URL", "required": true,  "doc": "normalised; the key of the entry" },
          "refs":           { "type": "list of full ref names", "required": false, "default": "the repository's default branch (decided 2026-10-14, David)" },
          "frequency_secs": { "type": "whole number, at least 60", "required": false, "default": 900 },
          "max_depth":      { "type": "whole number", "required": false, "default": 100, "doc": "commits walked per ref, a safety bound" },
          "shallow_depth":  { "type": "whole number", "required": false, "doc": "depth of a shallow fetch; unset is a full incremental fetch" },
          "credential":     { "type": "name of a credential", "required": false, "doc": "by name; the controller never sees its value" }
        } }
      }
    },
    {
      "name": "admission",
      "doc": "Under what parameters a proposal of a kind is accepted. Until one is held, nothing is accepted.",
      "body": { "kind": { "type": "text", "required": true }, "hosts": "...", "proposers": "...", "approved": "...", "denied": "...", "credentials": "...", "anonymous": "...", "limits": "..." }
    }
  ]
}
```

The observer's export has the same shape, with one runtime section:

```json
"runtime": [ {
  "name": "credentials",
  "doc": "Credentials the observer may use, each only for the hosts it names.",
  "body": { "credentials": { "list_of": {
    "name":         { "type": "text", "required": true },
    "host_pattern": { "type": "host pattern", "required": true, "doc": "the only hosts this credential is ever sent to" },
    "kind":         { "type": "token | user-password", "required": true },
    "username":     { "type": "text", "required": false },
    "secret":       { "type": "ciphertext", "required": true, "secret": true }
  } } }
} ]
```

### The documents on the wire, next to one another

All three have the same two fields, `payload` and `signature`; only the payload differs. The signature block is
the same in each (`algorithm` is `ed25519` or `ecdsa-p256-sha256`; the chain is PEM, leaf first).

**Deployment configuration** (as today):

```json
{ "payload": { "type": "configuration", "id": "2026-10-12-observer-debug", "target": { "agent": "git-repo-observer" },
               "version": 3, "changes": [ { "name": "RUST_LOG", "value": "warn,git_repo_observer=debug" } ] },
  "signature": { "algorithm": "ed25519", "value": "<base64>", "certificate_chain": [ "<leaf PEM>", "<intermediate PEM>" ] } }
```

**Runtime, section `repositories`** (to the controller):

```json
{ "payload": { "type": "runtime", "id": "repos-0007", "target": { "agent": "git-repo-controller" },
               "section": "repositories", "version": 7,
               "body": { "repositories": [
                 { "url": "https://github.com/cmu-sei/Polar.git", "refs": ["refs/heads/main", "refs/heads/dev"],
                   "frequency_secs": 900, "credential": "github-sei" },
                 { "url": "https://gitlab.example.org/team/service.git", "frequency_secs": 3600, "max_depth": 500 }
               ] } },
  "signature": { "...": "same shape" } }
```

**Runtime, section `admission`** (to the controller):

```json
{ "payload": { "type": "runtime", "id": "admission-0002", "target": { "agent": "git-repo-controller" },
               "section": "admission", "version": 2,
               "body": { "kind": "observe-repo",
                 "hosts": ["github.com", "*.example.org"],
                 "organisations": { "github.com": ["cmu-sei"] },
                 "proposers": ["<the Jira consumer's identity as the broker names it>"],
                 "require_approval": true,
                 "approved": ["https://github.com/cmu-sei/Polar.git"],
                 "denied": [],
                 "credentials": [ { "host": "github.com", "name": "github-sei" } ],
                 "anonymous": ["*.example.org"],
                 "limits": { "max_entries": 500, "per_proposer_per_hour": 20 } } },
  "signature": { "...": "same shape" } }
```

**Runtime, section `credentials`** (to the observer; the only one with ciphertext):

```json
{ "payload": { "type": "runtime", "id": "cred-github-sei-0001", "target": { "agent": "git-repo-observer" },
               "section": "credentials", "version": 1,
               "body": { "credentials": [
                 { "name": "github-sei", "host_pattern": "github.com", "kind": "token", "username": "oauth2",
                   "secret": "<base64 of the EscrowedSecret bytes, bound to git-repo-observer|credentials|1|github-sei|secret>" } ] } },
  "signature": { "...": "same shape" } }
```

The binding context and the text encoding are in `signed-configuration.md` (decided 2026-10-14). What is escrowed after decrypting is not the bare token: it is `{host_pattern, kind, username, token}`, so that
a `RetrieveSecret` after a restart brings the binding back with the secret, and the host check cannot be lost.

### The two messages between the agents

**Amended 2026-10-15: the controller announces a `RepositorySet`, not one message per repository.** One message holds every
repository (each as the `config` below) and a `complete` flag. An observer starts or replaces a worker for each repository in
it and, only if `complete`, stops the workers of repositories it does not name: that is how removal and expiry reach it, and one
message is all-or-nothing, so a lost announcement stops nothing. The controller marks the set incomplete for
`POLAR_GIT_CONTROLLER_WARMUP_SECS` (default 600) after it starts, since it may not yet have had the signed sections replayed or heard
its proposers again, and announces nothing at all until it has received a section or rules.

Controller to observer, on the existing `git.repo.config.events` (a `ConfigurationEvent`), minus the inline
credentials and plus the credential's name:

```json
{ "repo_id": "<id>", "repo_url": "https://github.com/cmu-sei/Polar.git", "remotes": ["origin"],
  "refs": ["refs/heads/main"], "max_depth": 100, "shallow_depth": null, "frequency": 900, "credential": "github-sei" }
```

Observer to controller, on `polar.git.observation.status`, per repository, after each attempt:

```json
{ "repo_id": "<id>", "state": "observing", "last_success": "2026-10-12T14:40:01Z", "last_error": null,
  "tips": { "refs/heads/main": "bbbb..." }, "consecutive_failures": 0 }
```

with `state` one of `starting`, `observing`, `pending-credential`, `failing`, `stopped`.

## The observer's own checks

It does not take the controller's word for these, because it is the one holding a credential and opening
connections:

1. A credential goes only to a host matching its `host_pattern`; the CryptoActor enforces it when the observer asks to use the credential for a host (`credentials-agent-architecture.md`, "Named operations on registered secrets"). A repository naming a credential whose pattern
   does not match its host is `failing`, with the reason, and nothing is sent.
2. The network guard, at connect time: a private, loopback, link-local or cluster address is refused, and checked
   again at connect so a name that changes its answer is caught. Only `https`; git's own protocol list is
   restricted to it.

## What has to exist first

- **Replay to an agent that has just started: built 2026-10-12.** A restarted controller needs the current
  `repositories` and `admission` back, and a restarted observer needs its `credentials` to close the loop.
  Every builder agent publishes a `ConfigurationReplayRequest` on `polar-configuration-replay` each time it
  registers. The **polar scheduler's observer** answers from its checkout of the schedules repository, which
  is the repository as last pulled, by republishing the documents that name the asking agent on that agent's
  own configuration topic, oldest version first. The agent verifies them like any other and refuses what it
  has already applied. A request that arrives before the observer's initial scan has finished is answered once,
  when it has. **The graph is never read for this**: it is a projection of what was observed, and a state
  rebuilt from it could disagree with the repository. (A first version read the stored documents back from the
  graph in the processor; David's rule that the graph is never authoritative removed it.)
- **`CryptoMessage::RetrieveSecret { name }`: built 2026-10-12.** The crypto actor asks the credentials agent
  for a named secret and decrypts the reply with the agent's own escrow key, which stays in custody
  (`CryptoClient::retrieve_secret`, `Ok(None)` for a name nothing is stored under, the plaintext wiped on
  drop). On the credentials agent `RetrieveSecret` now (1) refuses names that could end a policy resource early,
  (2) asks policy (`Action::"retrieve"`, resource `Secret::"<name>"`) before saying anything about whether the
  secret exists, failing closed, and (3) encrypts to the requester's **agent escrow key**, never its mTLS key.
  Before this it returned any stored secret to any authenticated requester that knew its name. **A policy
  has to be authored for each agent and secret**, or retrieval is denied (and with no policy loaded at all it is
  a failure, so a client retries). `DecryptOnBehalfOf` still re-encrypts to the mTLS certificate key and has
  the same problem; not touched.
- **`c:update-runtime`**, and the redeployment of the capability consumers.
- **The credentials agent's HTTP read-only endpoint for an agent's public escrow key**, which the author's tool
  needs to encrypt a `credentials` document. Not built (`signed-configuration.md`).
- **Instance ids** if observers are ever more than one: today a document targets an agent by name.

## Sharding (not now)

David does not expect more than one observer pod until production shows the need. The shape that keeps the door
open: the controller owns the assignment (repository to observer instance), the observer receives only what is
assigned to it, and the configuration topic gains a field or an instance suffix. A PVC for the clone cache is
one claim per pod, so it is decided together with the partitioning (by repository id), not before. The cache is
an emptyDir meanwhile.

## What changes in the code, in order, tests first

**Amended 2026-10-14 (David):** the credential path through a permanent schedule is built first. It includes the
observer's `credentials` section and its use of the CryptoActor, the repository configuration naming a
credential, the author's side (encrypting to the observer's key), a Cedar `store` action on the credentials
agent, and a **minimal controller**: `git-repo-controller` on the builder, which declares the `repositories`
section, holds the latest version and publishes each repository on `git.repo.config.events` naming its
credential. The minimal controller **replaces the git scheduler** (which is removed from the catalog and the
cluster); it has no proposals, admission rules, status events or backoff. Those (steps 1 and 7 below, and the rest
of step 6) come after, as path 2.

**Decided 2026-10-14 (David), for the minimal controller:**

- **Discovery is dropped for now.** The git scheduler was the only subscriber of `polar.git.repositories.discovery`
  (the GitLab consumer publishes it). With it gone, nothing observes a discovered GitLab project until path 2, when
  the GitLab consumer becomes a proposer under the admission rules. The `repositories` section is the only source.
- **The controller republishes the whole desired set** on `git.repo.config.events` once at start (after the replay
  delivers its section), whenever the section changes, and every `POLAR_GIT_CONTROLLER_REPUBLISH_SECS` seconds
  (a dynamic variable, default 60, at least 5). Cassini keeps nothing for a subscriber that is not there yet, so an
  observer that starts or restarts later would otherwise never hear.
- **The observer restarts a worker whose configuration changed** (stop and start; the clone in the cache is reused)
  and ignores an announcement identical to what the worker runs. **Removal is deferred:** a repository dropped from a
  later version keeps being observed until the pod restarts. It needs a protocol decision (the whole set with the
  observer stopping what is missing, or an explicit retire event), made with status and backoff in path 2.
- **The observer translates the refs.** A bare clone's `refs/heads/main` stops moving after the clone; a fetch
  advances only `refs/remotes/origin/main`. Authors write the remote's refs (`refs/heads/main`); the observer reads a
  configured `refs/heads/X` from `refs/remotes/<first remote>/X` when it sets a worker up. `refs/remotes/…` and
  `refs/tags/…` are taken as written, and an empty list still means the observer's default branch. (The scheduler's
  default `refs/heads/main` would have seen the first commit and no other.)

1. **The admission decision as a pure function**: normalisation, each rule, the order, the attack list from
   `proposals.md` (user information, `ext::`, `file://`, internal addresses, a lookalike host, a very long URL,
   a flood from one proposer). Tests before any wiring.
2. **The `runtime` type in the signed path**: `document_type::RUNTIME`, `section` and `body` in the payload, the
   capability and its authority rule, the section declaration in `--scaffold`, validation against it, the
   versions per section, the outcome events. Refusals tested first: a configuration document with a `section`,
   a runtime document with `changes`, a body that does not fit, an old version, a signer without the capability,
   a runtime document for the wrong agent.
3. **Replay** by the scheduler processor, and the confirmation of what exists.
4. **`CryptoMessage::RetrieveSecret`**, and the credential working set in the observer: add, escrow with retry,
   fetch at init, the host check.
5. **Status events** from the observer, and the checkpoint request and reply between observer and processor.
6. **The controller** on the builder: the desired set from `repositories` (built 2026-10-14, minimal), proposals
   decided under `admission`, reconciliation and backoff. The git scheduler was deleted with the minimal controller.
7. **The Jira side** publishes `observe-repo` proposals (`proposals.md`, step 5).

## Decisions (David, 2026-10-12)

1. A separate **git controller** is the control plane. The observer is an executor and the processor a graph
   writer; the git scheduler is retired.
2. The **receiver of proposals is the controller**, not the observer; the observer keeps the host-binding check
   and the network guard.
3. **Runtime configuration is a document type of its own** (`runtime`, with sections), processed by the
   ConfigurationActor with the events unchanged, under a capability (`c:update-runtime`) lower than
   `c:update-configuration`.
4. **Credentials reach the observer in a document addressed to it**, are used, then escrowed with the credentials
   agent, and are fetched from it at init. The controller never sees one.
5. **The observer reports per-repository status**, and restores checkpoints by asking the processor over Cassini.
6. **No more than one observer pod** until production shows the need; sharding and the PVC are decided together
   then.

## Policy for other receivers: decided

Decided 2026-10-12 (David): every receiver evaluates locally, so the builder hosts the policy and permissions actors, and the credentials agent pushes updates to them with an event; see `authority-distribution.md`. The text below is the question as it was first posed.

The credentials agent's policy decides who may have **a secret it holds**, and it evaluates that itself. There is
a second kind of decision, made by every agent that receives a signed document or command: *may this signer make
this change to me?* Today that is the signer's certificate capability (`c:update-configuration`, soon
`c:update-runtime`, `c:run-command`), which is coarse: a signer who may change one agent's runtime section may
change any.

Cedar could make it fine: "alice may change the `repositories` section of `git-repo-controller` but not the
`credentials` section of `git-repo-observer`", "operators may run `refresh-repo` on the observer in dev but not in
prod". There is no relay to trust, so **the agent receiving the document evaluates the rule itself**, which means
each such agent needs the rules, and the groups they refer to, as signed data it holds. That raises the question
this design leaves open: do those rules travel in the credentials agent's `policy` document (one document, which
every agent would have to receive, though most of it concerns secrets they never hold), or does each receiving
agent get its own `authority` section, or is there one `authority` document addressed to all agents? It does not
block anything here; the capability check stays as the gate until it is decided.
