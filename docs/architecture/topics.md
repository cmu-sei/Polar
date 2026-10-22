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

# Cassini topics, as built

Read from the code on 2026-10-21. Every topic in use, who publishes and who subscribes. Topics are exact strings (the
broker has no wildcards), and nothing declares them: they are constants and `format!` patterns in each crate. Paths
below are relative to `src/agents/`. Who may use which topic is not decided anywhere yet (`policy-enforcement.md`).

## Three names for one agent

An agent's topics are named after the **service name** it passes to `AgentBuilder::new` (`polar-agent-builder/src/lib.rs:
220-223, 433`), not after its `PolarAgent::NAME`, and the broker knows it by a third name, its **identity** (the
certificate's first DNS SAN, from its ServiceAccount: `<workload>.<namespace>.serviceaccount.<domain>`,
`cert-issuer/common/src/identity.rs:52-53, 125`). The service name is the workload name, a constant in each agent's
code, one binary per workload (`policy-enforcement.md`, "Decisions"), so the identity maps to it mechanically:

| Workload | Service name (its topics) | Identity (what the broker sees) |
|---|---|---|
| every agent | the workload name (since 2026-10-21 for kube-observer, kube-consumer, jira-observer, jira-processor, oci-registry-resolver, git-repo-processor, polar-scheduler and the credentials agent, which passed other names; the credentials agent was `credentials-agent-consumer`, still the name of its binary and image) | `<workload>.polar.serviceaccount.cluster.local` |

Each agent's `PolarAgent::NAME` is the same constant, so its scaffold is `<workload>.json`, the file `polar-sign` opens
for `--agent <workload>`.

The builder refuses to start an agent whose certificate names another workload, or no workload
(`polar-agent-builder/src/own_name.rs`, built 2026-10-21).

A signed document must name the service name in `target.agent` (`signed.rs:312`, `configuration.rs:706`). A rule about
who may use an agent's topics has to join the two.

## Implied topics

An agent declares the topics it uses itself (`PolarAgent::TOPICS`: each topic, `publish` or `subscribe`, and what it is
for). The topics it uses because it is an agent at all, or because of what it needs, are implied: it does not declare
them, and declaring one is a declaration problem. The rules are code in one place, `polar-agent-builder/src/topics.rs`
(`implied_by`, `rules_for`), and that is where the set is computed (`TopicSet::compute`): the agent's `--scaffold`
prints the whole set, each topic with where it came from (`declared`, with its purpose, or `implied`, with its rule), and
nothing else recomputes it (`policy-enforcement.md`, "Decisions"). Built 2026-10-21: every builder agent declares its topics,
and its client refuses a publish or subscribe outside its set unless its policy permits it (`topic_gate.rs`).

| Rule (group) | Applies when | Topics, for an agent named `<name>` |
|---|---|---|
| `agent` (`cassini-client`) | the agent is a Cassini client (or may be one: `OPTIONAL` holds `Cassini`) | subscribe `agent.<name>.configuration`, `agent.<name>.command`, `polar-authority-updates`, `polar-authority-snapshot-replies`; publish `polar-configuration-replay`, `polar-configuration-outcome`, `polar-command-outcome`, `polar-authority-snapshot-requests` |
| `self_test` (`self-test`) | `agent` applies and `SELF_TEST` is on | publish and subscribe `selftest.<name>` (a `:` in the name as `-`) |
| `encryption` (`encryption`) | `agent` applies and the agent needs `Encryption` | publish the six request topics of its crypto actor, subscribe their six replies (`polar_crypto::REQUEST_TOPICS`, `REPLY_TOPICS`) |

Each rule is a group (`policy-enforcement.md`, "Decisions"): the scaffold prints the agent's `groups`, the catalog writes
them into its floor, and its certificate carries them as `g:` names, so the broker knows them as the agent does.
`implied_topics(name, groups)` is the one function from a name and its groups to the implied topics; the agent computes
its set through it, and the broker will compute its floor with it (a property test holds the two equal). A group that
implies nothing (`operators`) adds nothing.

A service that is not a Cassini client (the broker) has no topics. The set is ordered and has each topic once per
access; it does not depend on the order of the declarations, and it is exactly the declared and the implied topics
(property tests in `topics.rs`).

## Families

An agent that uses a topic once for each workload it serves declares a family (`PolarAgent::TOPIC_FAMILIES`): a pattern
with `{workload}` once, its access and its purpose. The scheduler pair declares the one family there is,
`agent.{workload}.configuration` (the processor forwards each workload's documents on it, the observer replays them). The
render gives such an agent the workloads its target runs, comma-separated, in `POLAR_SERVED_WORKLOADS`
(`infra/resolve.nu`, the catalog's workloads with `config: "document"`, less those the target disables), and the
builder expands the family into one exact topic per workload when it computes the set (`TopicSet::compute_with`). No
pattern reaches the set, the certificates or policy (`policy-enforcement.md`, "Topics are exact names" and open point A).
The scaffold shows the family unexpanded under `topic_families`, and, filled from a document (`--scaffold --config`), its
topics expanded for the workloads that document names, each with `from: family` and its workload. Built 2026-10-21.

## The cross-check

Every declared or family topic has another side somewhere in the catalog (a subscriber, declared or implied, for a
publish; a publisher for a subscribe), or its declaration says why it waits (`TopicDecl::waiting`, printed as `waits`).
`infra/tests/catalog-topics.nu` builds every catalog agent from the workspace, asks each for its scaffold with the
scheduler's family expanded for every agent the catalog serves, and fails on a one-sided topic with no reason; the logic
is `infra/topics.nu` (tests `infra/tests/topics.nu`). A render warns about a topic one-sided in its target
(`resolve.nu topics-check`), since it cannot see the workloads its target disables. Built 2026-10-21. Waiting today:
four of the credentials agent's request and reply topics (nothing calls them yet; three more that waited were the
unsigned grants, revocations and provisioning, removed 2026-10-22), the git controller's
`polar-proposal-observe-repo` (no catalog agent proposes yet), and the resolver's `polar.provenance.events` (read by
`build-processor`, a chart). `local-minimal` warns about kube-consumer's discovery and resolution topics, since it does
not run the resolver.

## Causes

What makes an agent publish on a topic, said beside the topic (`docs/architecture/authority-model.md`, "Decisions":
local causes, derived paths). A publish is caused by a `Cause`: a message on a topic the agent subscribes to (`Topic`),
a request on a route it serves (`Http`), its registration with Cassini (`Registered`), its own interval (`Timer`), or
something outside Polar it watches (`Outside`); or nothing, and then it says why it publishes of its own accord. A
subscription may answer an entry (every cause but `Topic`): what the agent sent when the entry came in comes back
there. An agent says only what its own code does; the authority derives the paths through the system from everyone's
causes.

```rust
TopicDecl::publish(WORKLOAD_ISSUANCE_REQUESTS, "polar-init's workload issuance")
    .because(&[Cause::Http("/workload/issue")]),
TopicDecl::subscribe(WORKLOAD_ISSUANCE_REPLIES, "the answer to it")
    .answers(Cause::Http("/workload/issue")),
TopicDecl::publish(HEARTBEAT, "that it is alive").of_its_own_accord("it says so every minute"),
```

A family takes `.because` and `.of_its_own_accord` the same way. Implied topics carry their causes from their rule
(`topics.rs`, `implied_causes`): an agent's requests for a replay and for the authority are made when it registers (the
latter also when it sees an update it cannot check), and the snapshot replies answer that; its outcomes when a document
or a command arrives; its self-test probe on its own interval, which it hears back; its crypto actor's requests when the
agent encrypts or decrypts, of its own accord.

The scaffold prints each topic's `causes`, `answers` and `unprompted`, and leaves out what a topic does not have. The
builder refuses to start an agent whose causes do not fit (`topics.rs`, `cause_problems`): a cause on a subscription, an
answer on a publish, a topic cause the agent does not subscribe to, an answer to an entry none of its publishes takes or
to a topic, a cause that names nothing or is named twice, a publish both caused and unprompted, or unprompted for no
reason. Built 2026-10-22 (the authority model's slice 1); a publish with no cause and no reason is refused once every
agent declares its causes (slice 2).

## Every builder agent

| Topic | Publishes | Subscribes | Carries |
|---|---|---|---|
| `agent.<service>.configuration` | the scheduler processor; the scheduler observer on replay | the agent | `ConfigurationNotification { agent, document_json }`; the document inside is signed, the wrapper and its publisher are not checked |
| `agent.<service>.command` | nobody yet (the scheduler cannot route commands) | the agent | `CommandNotification` |
| `selftest.<service>` (`:` as `-`) | the agent's own instances | the same | instance id and counter |
| `polar-configuration-replay` | every agent, at registration | the scheduler observer | `ConfigurationReplayRequest { agent }`: anyone may ask for any agent, and the answer goes to that agent's topic |
| `polar-configuration-outcome` | every agent | the scheduler processor (records `published_by`) | `ConfigurationOutcome` |
| `polar-command-outcome` | every agent | nobody | `CommandOutcome` |
| `polar-authority-updates`, `polar-authority-snapshot-replies` | the credentials agent consumer | every agent (believed only from `POLAR_AUTHORITY_PUBLISHER`) | signed `Update`, `AuthoritySnapshot` |
| `polar-authority-snapshot-requests` | every agent | the credentials agent consumer | `AuthoritySnapshotRequest` |

## The credentials agent

Request/reply is a payload convention (`cassini-request-reply-protocol.md`): `Envelope { correlation_id, payload }` on
`<subject>-requests`, the answer on `<subject>-replies`. **Reply topics are shared:** every crypto actor subscribes to all
six crypto replies and filters by correlation id, so a topic rule cannot make a reply private; the payloads are encrypted
to the requester where it matters.

| Subject | Requested by | Requester identity from |
|---|---|---|
| `credentials-retrieve-agent-escrow-keypair` | polar-init, a crypto actor | `published_by` |
| `credentials-agent-escrow-issuance` | polar-init, a crypto actor | the ServiceAccount token in the request |
| `credentials-escrow-key` | polar-init, a crypto actor | (the agent's public key, to anyone) |
| `credentials-store-secret` | polar-init, a crypto actor | `published_by` (anonymous refused) |
| `credentials-retrieve-secret` | a crypto actor (only secret-harness calls it today) | `published_by`, then Cedar |
| `credentials-agent-escrow-key` | a crypto actor, the credentials agent observer | (another agent's public key) |
| `credentials-workload-issuance` | the credentials agent observer, relaying polar-init's HTTP | the sealed ServiceAccount token |
| `credentials-issuance`, `-identity-normalization`, `-account`, `-admin-action` | the observer (HTTP gateway) | the request's credential or signature |
| `credentials-certificate`, `-decrypt-on-behalf-of` | nobody publishes them today | |

The consumer subscribes to all 13 request topics (`credentials-agent/consume/src/supervisor.rs`, `REQUEST_TOPICS`) and passes
`published_by` through for store, retrieve, keypair retrieval and decrypt only (614-634).

## The scheduler and the git server

- The scheduler observer publishes `scheduler.in` (`GitScheduleChange`) and, on replay, `agent.<agent>.configuration`; it
  subscribes `polar-git-repo-changed` and `polar-configuration-replay`.
- The scheduler processor subscribes `scheduler.in` and `polar-configuration-outcome`, and publishes
  `agent.<agent>.configuration`. It forwards signed documents unverified (the receiving agent verifies), and ignores a
  file that is not one: the unsigned schedule path (`scheduler.adhoc`, `agent.<id>.schedule`,
  `agent.type.<type>.schedule`) was removed 2026-10-21.
- The Polar git server publishes `polar-git-repo-changed` (`RepoChanged`, with the pusher).

## Domain topics

| Topic | Publishes | Subscribes |
|---|---|---|
| `kubernetes.cluster.consumer.Pod` (every kind, despite the name) | kube-observer (`RawKubeEvent`, JSON) | kube-consumer (does not check `published_by`) |
| `polar.kubernetes.cluster.resolution` | oci-resolver | kube-consumer |
| `polar.provenance.discovery` | kube-consumer, the gitlab consumer | oci-resolver |
| `polar.provenance.events` | oci-resolver | build-processor |
| `polar.provenance.linker` | the gitlab consumer | build-processor |
| `git.repo.config.events` | the git controller | the git observer |
| `polar.git.repositories.events`, `polar.git.observation.status` | the git observer | the git processor |
| `polar.git.proposals.decisions` | the git controller | the git processor |
| `polar-proposal-observe-repo` | `discover_repo` | the git controller |
| `gitlab.consumer.metadata`, `gitlab:consumer:{projects,groups,users,pipelines,repositories}` | the gitlab observer | the gitlab consumer (also `gitlab:consumer:runners`, which nobody publishes) |
| `jira:consumer:{projects,groups,users,issues}` | the jira observer | the jira processor |
| `polar.web.consumer` | the openapi observer | nobody: the openapi processor subscribes `polar.openapi.processor`, a mismatch |
| `logging:consumer:logs` | nobody | the logger |

Defined and unused: `kubernetes.cluster.observer.pods`, `polar.kubernetes.cluster.resources`, `git.repo.events`,
`polar.git.processor`, `polar.git.repositories.discovery` (published by the gitlab consumer, never subscribed).
