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

# Soft state: the state agent

Status: DESIGN CLEARED by the maintainer 2026-10-30 (the reader half only for now; the cursor derived from the issues
written, not an explicit cursor node). BUILT 2026-10-30, then REVIEWED by the maintainer and REBUILT the same day
(below, "Revised by the maintainer's review" for why, "As built" for what): the floor a signed runtime section seeded
from the scaffolds, a `StateActor` in the builder, the `soft-state` capability computed by the builder. Unit-,
property- and Neo4j-container-tested, its image built; the rebuild has not run on a cluster (the first build has,
"Seen on a cluster"). The writer half, `soft-state-processor`, was BUILT 2026-11-01 for pulled health ("As built:
pulled health"), unit-, property- and Neo4j-container-tested, its image built, not yet run on a cluster; self-state
writes (`StateHandle::put`) are not built. Paths are relative to `src/agents/`. First user: the Jira
observer's issue cursor (`jira.md`, "What it does at start").

## What it is for

Some state is not authoritative, costs little to lose and is not sensitive: an observer's checkpoint (the last tip of
each ref), a poller's cursor. Decided 2026-10-15 (remaining-work.md, "STATE FOR AGENTS THAT IS NOT AUTHORITATIVE"): it
MAY be kept in the graph and read back, but an observer never touches the graph itself, and signed configuration and
secrets are never rebuilt from the graph. Rather than every agent pair answering its own observer, one agent answers
for all of them, and decides each request by WHO asks against the graph schema that agent is allowed to read.

## The rules it keeps

- **Who asks is the broker's word.** The broker takes a publisher's identity from its certificate's first DNS SAN at the
  TLS handshake and stamps it on each delivered message as `published_by` (`cassini/broker/src/listener.rs:93-120,
  395-418`); a publisher can stay anonymous but never name another identity. It reaches a builder `Subscription` as
  `IncomingMessage.published_by` (`polar-router/src/lib.rs:66-71`). A request with no identity is refused, as the
  credentials agent's request gate refuses one (`credentials-agent/consume/src/request_gate.rs:73-75`).
- **Ownership is declared by the agent; the floor is the state agent's signed configuration.** Each agent declares,
  beside its topics, the graph labels it OWNS (writes) and the ones it READS (`PolarAgent::GRAPH`, below), and
  `--scaffold` prints them. Who may read which label is the state agent's runtime section `schema`, signed like any
  runtime configuration: its first version is made from the scaffolds' declarations and signed by the first
  administrator at setup (`infra/seed.nu`, as the credentials agent's first `policy` is), and later versions are an
  author's. (Built first as an env var the render wrote; the review replaced it, below.)
- **Cedar overrides both ways** (`policy-enforcement.md`, 2026-10-21): a permit adds a read, a forbid removes one, and the
  floor decides where policy is silent. The vocabulary gains `Action::"read-state"` on `Label::"<label>"`, with
  `context.declared` (the `schema` section says so), as the topic actions carry it (`polar-policy/src/schema.rs:56-100`).
- **The graph is a projection.** A reply is a hint the asker may act on; nothing authoritative is decided from it.

## Shape

One workload, `soft-state-observer` (`soft-state/observe`), a graph READER only. It needs a new capability `graph-read`
(the graph section and client certificate of `graph-write`, without `expects_graph` in the health check), since none
exists (`polar-agent-builder/src/capability.rs:41-50`). The writer half (`soft-state-processor`) waited for a first
writer: the Jira cursor is derived from issues jira-processor already writes, so it needs no new writes, and the git
checkpoints (git-controller.md, "Status and checkpoints") read the `POINTS_TO` edges the git processor writes. Its first
writer is pulled health, built 2026-11-01 ("As built: pulled health").

Requests are closed, typed operations, never Cypher from the asker. Labels and property names cannot be Cypher
parameters, so the observer accepts only a label in the asker's allowed set and a property matching
`[A-Za-z_][A-Za-z0-9_]*`, and builds the query itself. First operation:

```
Envelope<StateRequest> { correlation_id, payload: StateRequest::Max { label, property } }
Envelope<StateReply>   { correlation_id, payload: Value(Option<i64>) | Refused(reason) | Unavailable(reason) }
```

`Max` returns the largest integer value of `property` over nodes with `label` (`None`: no node has it). `Refused` is a
decision (not allowed, malformed); `Unavailable` is the graph failing, and the asker retries.

Topics: requests on `soft-state-requests` (every asker publishes, the observer subscribes). Replies on a FAMILY,
`soft-state-replies.{workload}` over the served workloads (`topics.md`, "Families"): each asker subscribes only to its
own, so one agent's state is not broadcast to the others (reply topics are otherwise shared,
`cassini-request-reply-protocol.md:125-135`). The request topic is implied by the `soft-state` rule; every Cassini client's own reply topic by the `agent` rule.

## The Jira cursor (first user)

- jira-processor writes `updated_ms` (epoch milliseconds, i64) on each `JiraIssue`, parsed from Jira's system field
  `updated`, which the observer finds by its `/field` schema (`schema.system`), not by its display name, and passes
  beside the fields as `polar_updated`. A number, so times written with different UTC offsets compare correctly. It
  declares that it owns `JiraIssue`.
- jira-observer declares that it reads `JiraIssue`. At start, before its first issue pull, it asks `Max { JiraIssue,
  updated_ms }`:
  - `Value(Some(t))`: the first pull asks `updated >= -Nm`, N the minutes from `t` to now plus the 300 s overlap
    (`INCREMENTAL_OVERLAP_SECS`), rounded up. Relative, so the Jira user's time zone (which JQL applies to an absolute
    date) does not enter; the skew between the clocks is what the overlap covers, as for every incremental pull.
  - `Value(None)`: the graph has no issues; the pull is full, or from `QUERY_DATE`. `QUERY_DATE` applies only here.
  - `Unavailable`, `Refused` or no reply (the state actor's attempts spent): retried with the observer's backoff
    (`lib.rs`, `settle`). It never falls back to a full pull, which is what the cursor exists to avoid.
- Every issue pull orders `ORDER BY updated ASC`, so a pull cut short leaves the graph's newest `updated` meaning that
  everything older was written.

## Revised by the maintainer's review (2026-10-30): to rebuild

The as-built version below delivered the floor as an env var, `SOFT_STATE_FLOOR`, rendered by `resolve.nu`, and gave
the reply topics a family over the readers (`Over::StateReaders`), which needed the state agent's image to be asked for
its scaffold a second time. The maintainer: "completely not how polar is designed to work". Polar distributes control
through signed configuration at run time; authority updates (`polar-authority-updates`) are only for
security-sensitive authority. Decided instead:

- **The floor is a signed runtime section of the state agent** (who owns and who reads which label), read with
  `ConfigurationHandle::runtime`, its first version seeded from the scaffolds by `infra/seed.nu` (as the credentials
  agent's `policy` is) and later versions signed in polar-schedules. No `SOFT_STATE_FLOOR`, no second scaffold pass;
  a change reaches the running agent without a re-render or a rollout.
- **Health is pulled, not pushed.** The `agent` rule implies, for every agent, a subscribe to `agent.<name>.health`
  and a publish to `polar-health-replies`; the builder routes a query to the HealthCheckActor, which answers with its
  snapshot (status, phase, reason enum and a sanitized message, connections, counters, uptime). The state agent's
  writer half asks over a family `agent.{workload}.health` on `POLAR_SERVED_WORKLOADS` (the scheduler's precedent),
  alone subscribes to the replies, and writes the latest report and the recent transitions. Who may ask is decided by
  the request gate (Cedar `request` on `Request::"agent.X.health"`, `published_by`, then the floor). No heartbeat timer
  in each agent. This replaces the push design recorded earlier the same day.
- **The set of agents asked is the state agent's signed configuration** (seeded from the catalog's workloads,
  changed by a signed version), not an env var. Each expected workload is answering (with its status), unresponsive (no
  answer within the timeout: the observable the Reaper needs), or expected but unreachable (named by the configuration,
  outside the family the state agent was deployed with: drift, reported).
- **Self-state keeps its request and reply.** An agent that uses soft state for itself (jira-observer's cursor) has
  the capability `SoftState`, computed by the builder from how the agent is built, as `needs_of` adds `Cassini` for a
  Cassini client (`polar-agent-builder/src/agent.rs:373-385`), and printed under `requires`. Its reply topic,
  `soft-state-replies.<name>`, is implied by the capability's rule; the state agent publishes replies over a family on
  `POLAR_SERVED_WORKLOADS`. Topics stay exact names; privacy is on the subscribe side (only workload X subscribes to
  its reply topic).
- **Self-state has a builder actor of its own, beside the ConfigurationActor, not inside it:** a `StateActor` with a
  `StateHandle` under the InfrastructureSupervisor (`state.get(key)` at start, `state.changes()`, later
  `state.put(key, value)` when the writer half takes self-state writes). It owns the request and reply, the correlation
  ids, the timeouts and the retries, as the CryptoActor owns encryption; building with it is what makes the builder add
  `SoftState` to `requires`. Not the ConfigurationActor: what that holds is signed, verified against the pinned CA,
  versioned upwards and recorded as outcomes, and soft state is an unsigned hint that may be lost; one actor for both
  would put values from the graph beside signed configuration ("signed configuration is never rebuilt from the
  graph"). Separate types (`RuntimeValue` and a soft-state value) keep one from being used for the other. jira-observer's
  cursor is rebuilt on it.
- **Services that are not Cassini clients** (the broker) cannot be asked over Cassini; their health does not reach the
  graph yet.

### The as-built decisions, reviewed (2026-10-30)

Kept: an asker is matched by its exact identity (another namespace's same service account is refused); owning a label
does not grant reading it through the state agent, and the state agent declares no labels; Cedar is asked
`read-state` without the certificate, groups coming from the signed groups document, `context.declared` meaning (after
the rebuild) that the state agent's signed runtime section says so, and `context.implied` false; a request with no
verified asker has no reply topic and gets no reply; the state agent runs on the writers pool (it holds the graph
password); a property that is not an integer is `Refused` (asking again cannot change it). Changed by the rebuild:
`graph-read` keeps no dependency, and the state agent reports `Degraded` with reason `Graph` while the graph is down
(it stays up and answers `Unavailable`); the timeouts (10 s for the graph, 8 s for a reply) become declared variables
of the StateActor with those defaults; the check that a reply comes from the state agent's identity moves into the
StateActor. To fix: the cursor reads Jira's system field `updated` (known from `/field`'s `schema.system`), not the
English display name `Updated`. Gap kept: the `JiraIssue_<field>` labels jira-processor makes from custom fields cannot
be declared by name.

### Self-state and introspection (proposed 2026-10-30, accepted by the maintainer)

Two kinds of state pass through the state agent, and they are kept apart:

- **Self-state:** written and read back by an agent's own domain (or its own pair), under labels the domain owns and
  declares. The state agent is a store with an access rule; it does not interpret the content.
- **Introspection:** what is known about an agent from outside (health, connectivity, configuration in force, refused
  topics). It comes only from the builder's infrastructure actors, with a schema the framework defines and sanitizes.

Rules: a query is answered only by an infrastructure actor, and nothing from outside ever addresses a domain actor; a
domain makes a fact known only by publishing it as its own observation or by telling the infrastructure through a
typed, declared builder API (as it can send `GraphOpFailed` to the health actor), never by being looked into; the
state agent's writer writes only its own labels (`AgentHealth`). Domain introspection would make a domain's internals an
API, let its data out by a path its owned labels and topics do not govern, and put that data under another owner's
name. Debugging what an actor is doing belongs to logs and traces, not to the state channel. To watch: agent-specific
health counters (such as issues pulled) are domain facts, and come only through the declared API or the domain's own
graph writes.

## As built (rebuilt 2026-10-30, after the review)

Paths relative to `src/agents/`. Commits `9250bf10` (builder, jira-observer), `e1078d15` (state agent), `8ea49054`
(infra), `0e7a30f7` (Jira's system field).

- **Declarations.** `PolarAgent::GRAPH` holds `GraphDecl::owns` and `GraphDecl::reads`
  (`polar-agent-builder/src/graph_schema.rs`), printed under `graph`; unchanged from the first build. Declared today:
  jira-processor owns `JiraIssue`, `JiraProject`, `JiraUser`, `JiraGroup`, `JiraIssueType` and `JiraIssueChangeLog`;
  jira-observer reads `JiraIssue`. Since 2026-11-01 kube-consumer owns the 21 Kubernetes labels it writes, and an owner's
  graph controller makes the constraints for its owned labels at start, from `PolarAgent::GRAPH_KEYS`
  (graph-write-path.md, "Constraints and indexes"); the `JiraIssue.key` constraint is not an index on `updated_ms`.
- **The `soft-state` capability, computed.** `computed_needs` (`polar-agent-builder/src/agent.rs:378`) gives an agent
  `Cassini` first when it is a client, then what it lists, then `SoftState` when it declares a graph read, each once;
  listing `SoftState` by hand is a declaration problem (`:897`). Its rule, `Rule::SoftState` (group `soft-state`,
  `topics.rs`), implies publish `soft-state-requests`; the `agent` rule gives every Cassini client a subscribe to its own
  `soft-state-replies.<name>`, so the state agent's family has a subscriber for every topic. Both go through the one
  `implied_topics` the broker's floor uses (`floor.rs`); the property test holding the two equal draws `soft_state`.
  `infra/config/capabilities.nuon` `soft-state`: nothing in the pod, its `dependency` the workload
  `soft-state-observer`, which `resolve.nu check` requires a target to run (`graph.nu` `needs_workload`).
- **The `StateActor`** (`polar-agent-builder/src/state.rs:294`), under the InfrastructureSupervisor beside the
  CryptoActor, started for an agent with the capability (`infrastructure.rs:459`, `:565`); `AgentResources::state` is
  its `StateHandle`, behind the `StateClient` trait (`:169`). It owns the question: `ask_until_answered` (`:261`), a
  fresh correlation id per attempt, `POLAR_STATE_REPLY_TIMEOUT_SECS` (8) per attempt and `POLAR_STATE_ATTEMPTS` (3)
  (`:52`, `:60`, declared variables of the capability's section), a value or a refusal never asked again, and an
  `Unavailable` asked again by the actor itself every `POLAR_STATE_UNAVAILABLE_RETRY_SECS` (10) for up to
  `POLAR_STATE_UNAVAILABLE_LIMIT_SECS` (120) before the caller hears it (`ask_until_available`, since `bfede9ef`); its
  reply subscription
  (`replies_subscription`, `:239`) believes only the state agent of the agent's own namespace and domain
  (`state_agent_identity`, `:222`; `from_state_agent`, `:233`). `get(StateQuery::Max)` returns a `SoftValue` (`:128`),
  a type of its own, never a `RuntimeValue`; `changes` and `put` come with the writer half.
- **The vocabulary.** `Action::"read-state"` on `Label::"<label>"` (`polar-policy/src/schema.rs`), unchanged.
- **The state agent.** `soft-state/observe`: `NEEDS [GraphRead]`; its floor the runtime section `schema`
  (`floor.rs:42`, judged by `Floor::from_body`, `:79`), read at start and on each `runtime_changes`
  (`supervisor.rs:274`); its reply family over `POLAR_SERVED_WORKLOADS` (`agent.rs`), each topic heard by its
  workload by the `agent` rule, so nothing in it is one-sided. The decision (`decision.rs`) is as
  before, and before a version of `schema` has arrived a read policy does not permit is `Unavailable` ("no schema yet",
  `:123`). `POLAR_STATE_GRAPH_TIMEOUT_SECS` (10, `settings.rs`) bounds a read; a graph that is down is reported to the
  health actor as not connected (`report_graph`, `supervisor.rs:157`), so the agent stays up and healthy, answers
  `Unavailable`, and its health file says `graph_connected: false`.
- **The seed.** `infra/seed.nu` `documents` holds `permanent/soft-state-observer-schema.dhall` (version 1,
  `c:update-runtime`, its body `from: "scaffolds"`); `up.nu`'s `step_seed` asks `resolve.nu state-schema --target`
  (`resolve.nu:1140`, `graph.nu` `state_schema`) for the body only when that path is not in polar-schedules yet. The
  render gives the state agent nothing of its own, and asks the scaffolds once.
- **The Jira cursor.** jira-observer asks through the `StateClient` it is given (`jira/observe/src/issues.rs`,
  `cursor_query`, `:61`; `start_from`, `:80`): no request or reply code of its own. The observer finds the system field
  `updated` (`system_field_id`, `:550`) and passes its value as `polar_updated` (`mark_updated`, `:619`,
  `jira_common::SYSTEM_UPDATED_KEY`); jira-processor parses `updated_ms` from it alone
  (`jira/process/src/issues.rs:379`). (Line numbers as of 2026-11-01, `d5bb9200`.)

Decided where the review was silent (by the agent that rebuilt it; to confirm):

1. **The `soft-state` capability is computed from `GRAPH` reads.** The scaffold is printed without building anything,
   so "builds with the StateActor" is read as "declares a graph read", and the builder starts the actor exactly for an
   agent whose `requires` has it.
2. **Every Cassini client subscribes to its own reply topic, by the `agent` rule** (CHANGED on the maintainer's
   review, 2026-10-30). Built first with the subscription in the `soft-state` rule, the family over the served
   workloads had topics nobody subscribed to, and a `FamilyDecl::waiting` to excuse them in the cross-check. The
   maintainer: a new concept added to paper over a gap is the wrong shape; pulled health will change every agent's
   implied set anyway (`agent.<name>.health`); a subscription that never receives costs nothing; and an agent without a
   declared read that asks is refused on its own topic. So `waiting` on families is gone, the `agent` rule implies
   subscribe `soft-state-replies.<name>`, the `soft-state` rule keeps only the publish to `soft-state-requests` (and
   remains what starts the StateActor), and the catalog's cross-check passes with no wait for a reply topic.
3. **No `schema` yet means `Unavailable`, not `Refused`:** the seed is pushed after the state agent starts, and a
   refusal says asking again changes nothing.
4. **The capability's `dependency` is a workload** (`{ workload: "soft-state-observer" }`), checked by the render: the
   dependency facet's `service` names a TLS endpoint to wait for, and the state agent has none.
5. **The state agent of the asker's own namespace and domain** is the only publisher a reply is believed from (exact
   identity, as the state agent matches askers).
6. **`POLAR_STATE_ATTEMPTS` (3)**, after the request/reply protocol's three attempts: only no answer is asked again.
7. **Degraded is `graph_disconnected`:** of what the health actor takes today, the one that records the graph down
   without ever writing the file unhealthy (`GraphOpFailed` writes it unhealthy once, which a probe could catch).
8. **The schema's body has no `format` field:** the section's version is the document's.
9. **The system field is passed beside the fields, as `polar_updated`,** read by the processor from there only: a
   custom field named "Updated" is not the cursor, and an observer that does not pass it gives no `updated_ms`.

The first build's decisions are in "The as-built decisions, reviewed" above; its floor variable, readers variable,
`Over::StateReaders` and second scaffold pass are gone.

## As built: pulled health (2026-11-01)

Built in a worktree on main `2e76cc2d`, as decided in "Revised by the maintainer's review"; tested (unit, property,
Neo4j container) and the images built, not yet run on a cluster. Paths relative to `src/agents/`.

- **The report** (`polar-health/src/report.rs`). `HealthReport { status, phase, reason, message, cassini_connected,
  graph_connected, expects_graph, counters, uptime_secs }`, rkyv on the wire, serde for the file's readers. `status`
  is `ReportStatus`: `Starting | Healthy | Degraded | Unhealthy | Draining`. `reason` is `HealthReason`: `Broker`,
  `Graph`, `Dependency`, `Internal` (`restart_helps()`: the verdict ends the agent as before) and `Configuration`,
  `Credentials` (a restart does not help). `counters` are the health actor's own (`HealthCounters`: Cassini
  disconnects, graph operations failed, topics refused now, health-file writes failed in a row); a domain's counts are
  domain facts and are not here. The message is `sanitize`d: one line, at most `MAX_MESSAGE_CHARS` (200), each secret
  value of the agent in force (`polar_env::Config::secret_values`, at least `MIN_SECRET_CHARS`) and the shapes a
  credential takes (a PEM block, a URL's user information, the word after `Bearer`/`Basic`, the value of a name such as
  `password=`, `token:`) withheld; a message that would still carry a secret is not sent (`None`).
- **The machine** (`polar-health/src/machine.rs`, `HealthMachine::report`): `Draining` once a verdict is given (its
  reason the verdict's: `Broker` for the Cassini deadline, `Graph` for the graph's, `Internal` for the certificate's,
  the health file's or custody's; none for a rejuvenation) or once the supervisor begins to end the agent for a
  reason of its own (`HealthCheckMessage::Draining`); then `Unhealthy` with the held reason while a standing problem is
  held; then `Starting` until the connections the agent counts have all been up once, `Unhealthy` (`Broker` or `Graph`)
  when one drops after; `Degraded` (`Graph`) for an agent that reads the graph without depending on it while it is
  down (the state agent's reader); else `Healthy`.
- **A standing problem** (the typed, declared API a domain reports through, as it sends `GraphOpFailed`):
  `HealthHandle::hold(HoldReason::Configuration | HoldReason::Credentials, message)` and `HealthHandle::release(reason)`
  (`polar-agent-builder/src/health.rs`), messages `HealthCheckMessage::Hold`/`Release`. A hold never becomes a verdict:
  the agent stays up, its health file says `unhealthy` (the probe judges only its age), its report says why, and it
  still rejuvenates; the domain stops retrying what cannot work. `polar_agent_builder::held::stay_up` (`held.rs`) is
  the start-time form: an agent whose own configuration cannot work starts its infrastructure and no domain, holds
  the problem, answers health queries, and ends only by its health actor's verdict or a signal, unless a signed update
  mends it (below, "A held agent tries again on a signed update").
- **The question and the answer** (`polar-agent-builder/src/health_query.rs`). The `agent` rule implies, for every
  Cassini client, subscribe `agent.<name>.health` and publish `polar-health-replies` (`topics.rs`, `implied_by`; the
  answer is caused by the question), so the broker's floor, the certificates and the agent's own set have them. The
  builder subscribes for every client (`infrastructure.rs`, `spawn_client_parts`) and hands each question to the
  InfrastructureSupervisor (`HealthQuery`), which answers off its thread: the request gate (Cedar `Action::"request"`
  on `Request::"agent.<name>.health"` for the broker-stamped `published_by`; where policy is silent, none is loaded or
  none runs, the floor; a policy that cannot be consulted refuses), then the health actor's report
  (`HealthCheckMessage::Report`), its message cleaned of the secrets in force, published with the agent's identity
  (`share_identity`) and `OfflineBehavior::Drop`. The floor admits `soft-state-processor` of the agent's own namespace
  and domain (`writer_identity`), exactly; `context.declared` is true for it. No heartbeat timer in any agent.
- **The writer** (`soft-state/process`, workload `soft-state-processor`, `NEEDS [GraphWrite]`, owns `AgentHealth`).
  Its set of agents asked is its signed runtime section `agents` (`expected.rs`: `{ workloads: [{ workload, identity
  }] }`, judged as `schema` is), seeded by `infra/seed.nu` from the catalog (`resolve.nu health-agents`: every `agent`
  kind the target's family serves, by identity) and changed by a signed version. Every
  `POLAR_HEALTH_ASK_INTERVAL_SECS` (60) it opens a round: one `HealthQuery` with the round's correlation id on
  `agent.<w>.health` for each expected workload in its deployed family (its `TopicSet`: the family over
  `POLAR_SERVED_WORKLOADS`); it alone subscribes to `polar-health-replies`, and an answer counts only with the round's
  id and stamped with an expected identity exactly (`supervisor.rs`, `Round::accept`). After
  `POLAR_HEALTH_ANSWER_TIMEOUT_SECS` (10, less than the interval) the round closes (`classify.rs`): answering (its
  report), unresponsive (asked, no answer), or unreachable (named by `agents`, outside the deployed family: drift,
  warned). Each workload's history (`history.rs`) records a transition when its state or reason changes, at most
  `MAX_TRANSITIONS` (20), oldest first; the round is one `UpsertNodes` of `(:AgentHealth {workload})` with `identity`,
  `state`, `observed_at_ms`, the transitions (JSON strings), and, for an answer, the report's fields and
  `answered_at_ms` (an unresponsive round leaves them as the last answer said) (`graph.rs`). The histories are read
  back from the graph before the first round, so a restart keeps them. Before a version of `agents` arrives no one
  is asked.
- **jira-observer** (`jira/observe/src/agent.rs`, `prepare`): a placeholder or malformed `JIRA_URL`, or any other
  configuration error at its start, is held as `Configuration` through `held::stay_up`, instead of ending the process
  (which, with the pod's `RestartAllContainers` rule and the renewal rule, looped every five minutes). Its observers hold
  at run time too (below).
- **Infra.** `config/workloads/soft-state.nuon` (pool `writers`), `images.nuon`, `soft-state/package.nix`
  (`processorImage`) and `container-processor.{dhall,nix}`, the targets' resource lists and issuer entries, `seed.nu`
  (a computed body names its `resolve` command; `computed`), `up.nu` `step_seed` (asks each computed document's
  command), `graph.nu` `health_agents`.

### A held agent tries again on a signed update (the maintainer, 2026-11-01)

The rule, amended: a held agent does not only wait for its next rejuvenation; it tries again when a signed
configuration update arrives. Polar's control travels as signed configuration at run time: the builder's
ConfigurationActor announces each applied update on `ConfigurationHandle::changes()` (`ConfigChanged { version,
changed }`), and the new values are read with `current()`. The scheduler replays the signed documents to an agent that has
just registered (`polar-configuration-replay`, `polar-scheduler/common/src/lib.rs:65`), so after every restart the
render's environment applies first and the signed update arrives just after. Only a Dynamic variable can be changed by an
update; the maintainer's rule is that an agent's own non-secret settings are Dynamic (one whose are Static "hasn't been
properly migrated to the builder"), and making them so for every agent is a remaining-work item. Built 2026-11-01:

- **At start** (`8635c729`, `polar-agent-builder/src/held.rs`): `held::stay_up(configured, name, reason, message,
  try_again)`. `TryAgain` is a boxed async closure the agent passes; on each `ConfigChanged` it is called with a `Retry`:
  the configuration in force, the resources already built, the held supervisor's cell to link the work under, and
  whether Cassini has registered the agent. `Ok` means the work started: the hold is released, the work takes the
  agent's events itself (`events_output`, `infrastructure_events`) and answers the shutdown, and the work's ending ends
  the agent. `Err(why)` keeps the hold, with `why` as its message. `held::never()` is the closure of an agent no update
  can mend. General; jira-observer is its one user (its closure runs `prepare` on the configuration in force and starts
  the observer supervisor on the built infrastructure, `Infrastructure::Built`).
- **At run time, jira-observer** (`8c2ec420`, `e0901170`, `96ddd517`; `jira.md`, "What it does at start", 6): each
  observer holds a `Credentials` or `Configuration` failure through the agent's `HealthHandle` (given in
  `JiraObserverArgs` by `observer_args`), one line (`<observer>: <what failed>; <what to set>`, never a secret), and
  schedules no pull; a success releases the hold its observer placed; a transient failure keeps the backoff and never
  holds. Its settings (`JIRA_URL`, `JIRA_ANONYMOUS`, `JIRA_DEPLOYMENT`, `QUERY_DATE`) are Dynamic: an update naming one
  makes the supervisor run `prepare` on `current()` and start the observers again, which releases every hold (an observer
  that meets the problem again holds it again), or stop them and hold `Configuration` with why. Its credential is a
  signed runtime section (`credentials`), so a new token is a new version: the supervisor registers it with the
  CryptoActor and starts the observers again, clearing a `Credentials` hold without a restart. Egress stays Static (the
  cluster's setting; the CA is a mounted file).
- **Which variables an update can change today:** jira-observer's four, and every agent's logging (`RUST_LOG`,
  `POLAR_TRACE_FILTER`); `held::stay_up` retries on a change to any of them. The token is not a variable: it is the
  `credentials` runtime section (`RuntimeChanged`).

Decided (to review): several observers can hold; each releases only its own hold, and the health actor reports the
latest (`HoldReason` is one slot per agent: naming every holder would need holds keyed by holder in the builder). A
held observer is never pulled again in its process; starting again is a stop and respawn by the supervisor.

Decided where the design was silent (by the agent that built it; to confirm):

1. **A new workload, `soft-state-processor`, not part of soft-state-observer.** This doc names a writer half
   `soft-state-processor`; the pairs' precedent keeps reading and writing apart (the reader is `graph-read`, which
   keeps no dependency on the graph and so answers `Unavailable` while it is down; a writer is `graph-write`, ended by
   its health actor when the graph is gone past its deadline); and a separate identity is what Neo4j roles would bind
   a read-only and a writing credential to. Both hold the graph password, so both are on the writers' pool.
2. **The floor of the request gate** is the state agent's writer of the asked agent's own namespace and domain, as
   the StateActor believes only the reader of its own (`state_agent_identity`); Cedar can permit anyone else.
3. **A standing problem is `Unhealthy`, not `Degraded`**: the agent cannot do its job at all. It is written
   `unhealthy` in the health file too (informational; the probe judges age only), and it does not stop rejuvenation.
4. **The verdicts' reasons:** the certificate's hard threshold, a health file that cannot be written and custody are
   `Internal`; `Dependency` is defined and not yet given by any verdict.
5. **`Degraded` only for the graph** of an agent that does not depend on it; refused topics and waiting for
   dependencies' certificates are not degraded (refusals are counted, the phase says `waiting_for_deps`).
6. **The seed is the `agent` kind of the target's served workloads**: a run (the load harness) ends by design and
   would be unresponsive. The broker and cert-issuer are not Cassini clients and are not asked ("Services that are not
   Cassini clients").
7. **An answer that cannot go now is dropped** (`OfflineBehavior::Drop`, both ways): it would arrive after its round.
8. **The message is cleaned at the edge** with the secrets in force at that moment, and at the hold with the shapes
   only; values of fewer than four characters are not looked for.
9. **The histories are read back at start and no round is written until they are**, rather than writing over them
   with a short history.
10. **jira-observer holds any error from `prepare`** (the settings, the address, the egress section) as
    `Configuration`; a problem `run` judges before `start` (a missing variable) still ends the process with every
    problem named, since the infrastructure cannot be built on it.

## Seen on a cluster (u7c, sandbox-local, 2026-10-30), the first build

Against a fake Jira Data Center in its own namespace (120 issues two hours apart, their `updated` alternating `+0000`
and `-0400`; it logged each JQL and honoured `updated >= -Nm`):

- The render's two passes over the scaffolds ran against the in-cluster registry; soft-state-observer started on the
  writers node and loaded its floor (`workloads=2 readers=1`).
- First start, empty graph: jira-observer asked, was told the graph has no issue, and pulled all 120 in three pages, each
  `ORDER BY updated ASC`. jira-processor wrote 120 `JiraIssue` with `updated_ms`; the newest was the fake's newest
  instant (`19:22:07-0400` = `23:22:07Z`), so the offsets were read as instants.
- After its own rejuvenation 42 minutes later: "the graph's newest issue was updated at 1793402527000 ms; the first pull
  starts there", and Jira was asked `updated >= -47m ORDER BY updated ASC` (41.9 minutes plus the 300 s overlap, rounded
  up): one issue, not 120.
- While the fake answered `/field` without the fields a real Jira sends, the observer stayed up and retried (30 s
  doubling), as `jira.md` describes; jira-observer exits only on a configuration refusal (the placeholder `JIRA_URL`).

## The rebuild, seen on a cluster (u7c, sandbox-local, 2026-10-31)

Deployed over the earlier test (the graph holding the fake Jira's 120 issues). The seed step signed and pushed
`permanent/soft-state-observer-schema.dhall` version 1, which the scheduler observer could not evaluate: `polar-sign`
wrote every empty list as `([] : List {})`, so jira-observer's empty `owns` and jira-processor's `owns : List Text` were
two types in one list (`InvalidListElement`). Until a schema arrived the state agent answered "no schema yet; ask
again" and jira-observer retried, as designed, never pulling everything. Fixed in `polar-sign` (`50baf2f6`: an empty
list takes its element type from its siblings; a list that cannot be unified is refused at signing, naming its path).
The seed skips a path that exists, so version 2 was signed by an author and pushed; "the schema is loaded version=2
workloads=2 readers=1", and at jira-observer's next try "the graph's newest issue was updated at 1793402527000 ms; the
first pull starts there", Jira asked `updated >= -331m ORDER BY updated ASC` (325.1 minutes plus the overlap, rounded
up): one issue.

Found by it, FIXED 2026-10-31 (`bfede9ef`): while the state agent had no schema, jira-observer's retries followed its
Jira backoff (30 s doubling to 15 minutes), so on a fresh cluster, where the seed lands minutes after the agents start,
the first cursor pull could wait up to 15 minutes after the schema was there (here 14). A "no schema yet" belongs to the
StateActor's own retry, not to the domain's: it now asks again itself after an `Unavailable`, every
`POLAR_STATE_UNAVAILABLE_RETRY_SECS` (10) for up to `POLAR_STATE_UNAVAILABLE_LIMIT_SECS` (120), before the caller hears
it (`polar-agent-builder/src/state.rs`, `ask_until_available`); a value or a refusal is returned at once.

## Gaps kept open

- Neo4j has one superuser for every agent (`infra/config/capabilities.nuon:77-97`); "reader only" is the code's
  discipline until Neo4j roles exist.
- Pagination by `startAt` can skip an issue updated during a pull (it moves to the end); the existing incremental pull
  has the same hazard. Not changed here.
- No index on `updated_ms`; `Max` scans the label once per observer start.
- A graph whose issues all predate `updated_ms` answers that none has it, so the first start after the upgrade pulls
  everything (or from `QUERY_DATE`) once.
- The rebuild has not run on a cluster: the seeded `schema`, the StateActor and the system field are tested and the
  images built, not deployed. The seed's `resolve.nu state-schema` has run from the workspace's scaffolds only.
- Health in the graph is built ("As built: pulled health") and not yet seen on a cluster: the seeded `agents`, the
  rounds, the request gate against the broker's stamped identity, the `AgentHealth` writes, jira-observer held on a
  placeholder `JIRA_URL`, its observers' run-time holds, and a held agent mended by a signed update (neither the
  supervisor's wiring of `ConfigChanged` and `RuntimeChanged` nor jira-observer's `TryAgain` closure is covered by a
  test: they need the agent's built infrastructure). The state agent's reader now reports `Degraded` with reason `Graph` while the graph is down.
- Self-state writes (`StateHandle::put`, `changes`) and the git checkpoints are not built.
- The broker's own health cannot be asked over Cassini (it is not a client); remaining-work.md, "THE BROKER AS A USER
  OF SOFT STATE".
