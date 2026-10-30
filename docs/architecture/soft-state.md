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
written, not an explicit cursor node). BUILT 2026-10-30, the reader and the Jira cursor (below, "As built"); unit-,
property- and Neo4j-container-tested, its image built, not yet run on a cluster. The writer half is not built. Written
from the code as it stood on `main` (`1b82f624`); paths are relative to `src/agents/`. First user: the Jira observer's
issue cursor (`jira.md`, "What it does at start").

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
- **Ownership is declared by the agent and signed off by applying the catalog.** Each agent declares, beside its topics,
  the graph labels it OWNS (writes) and the ones it READS (`PolarAgent::GRAPH`, below). `--scaffold` prints them; the
  render writes them into the state agent's floor, as it writes declared topics into the issuance floor today
  (`infra/resolve.nu:821-840`, `infra/rules.nu:260-280`). Applying the catalog is the administrator's approval
  (`policy-enforcement.md`, "Decisions", 2026-10-21); there is no separate signed seed. This replaces the 2026-10-15
  note that the schema be a reviewed seed never regenerated on deploy: the 2026-10-21 decision covers the same case for
  topics, and an image that declares more than it should is the registry gate's and the review of the catalog's
  business, as it is for topics.
- **Cedar overrides both ways** (`policy-enforcement.md`, 2026-10-21): a permit adds a read, a forbid removes one, and the
  floor decides where policy is silent. The vocabulary gains `Action::"read-state"` on `Label::"<label>"`, with
  `context.declared` (the floor says so), as the topic actions carry it (`polar-policy/src/schema.rs:56-100`).
- **The graph is a projection.** A reply is a hint the asker may act on; nothing authoritative is decided from it.

## Shape

One workload, `soft-state-observer` (`soft-state/observe`), a graph READER only. It needs a new capability `graph-read`
(the graph section and client certificate of `graph-write`, without `expects_graph` in the health check), since none
exists (`polar-agent-builder/src/capability.rs:41-50`). The writer half (`soft-state-processor`) waits for a first
writer: the Jira cursor is derived from issues jira-processor already writes, so it needs no new writes, and the git
checkpoints (git-controller.md, "Status and checkpoints") read the `POINTS_TO` edges the git processor writes.

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
`soft-state-replies.{workload}` (`topics.md`, "Families"): each asker subscribes only to its own, so one agent's state is
not broadcast to the others (reply topics are otherwise shared, `cassini-request-reply-protocol.md:125-135`).

## The Jira cursor (first user)

- jira-processor writes `updated_ms` (epoch milliseconds, i64) on each `JiraIssue`, parsed from the issue's `Updated`
  field (the system field `updated` under its display name after `rename_fields`, `jira/observe/src/issues.rs:276-293`;
  stored today as the string property `Updated`, `jira/process/src/issues.rs:166+`). A number, so times written with
  different UTC offsets compare correctly. It declares that it owns `JiraIssue`.
- jira-observer declares that it reads `JiraIssue`. At start, before its first issue pull, it asks `Max { JiraIssue,
  updated_ms }`:
  - `Value(Some(t))`: the first pull asks `updated >= -Nm`, N the minutes from `t` to now plus the 300 s overlap
    (`INCREMENTAL_OVERLAP_SECS`), rounded up. Relative, so the Jira user's time zone (which JQL applies to an absolute
    date) does not enter; the skew between the clocks is what the overlap covers, as for every incremental pull.
  - `Value(None)`: the graph has no issues; the pull is full, or from `QUERY_DATE`. `QUERY_DATE` applies only here.
  - `Unavailable`, `Refused` or no reply: retried with the observer's backoff (`lib.rs:403`, `settle`). It never falls
    back to a full pull, which is what the cursor exists to avoid.
- Every issue pull orders `ORDER BY updated ASC`, so a pull cut short leaves the graph's newest `updated` meaning that
  everything older was written.

## As built (2026-10-30)

Paths relative to `src/agents/`; the line numbers are those of the commit that built each piece.

- **Declarations.** `PolarAgent::GRAPH` (`polar-agent-builder/src/agent.rs:148`) holds `GraphDecl::owns` and
  `GraphDecl::reads` (`polar-agent-builder/src/graph_schema.rs`); the scaffold prints them under `graph`, only when
  there is one, so every other scaffold is byte-identical. The set is ordered by (label, access) and holds each pair
  once, whatever the order of the declarations (`graph_set`, `:125`, proptests); a label that is not a plain
  identifier (`is_identifier`, `:113`), one with no purpose, a pair declared twice, or a read by an agent that is not a
  Cassini client is a declaration problem (`problems`, `:147`). Declared today: jira-processor owns `JiraIssue`,
  `JiraProject`, `JiraUser`, `JiraGroup`, `JiraIssueType` and `JiraIssueChangeLog` (`jira/process/src/agent.rs:64`);
  jira-observer reads `JiraIssue` (`jira/observe/src/agent.rs:83`).
- **The capability.** `Capability::GraphRead` (`polar-agent-builder/src/capability.rs:49`), `graph-read` in
  `infra/config/capabilities.nuon`: the graph section and the pod's one client certificate, as `graph-write` has, the
  health check not watching the graph (`startup.rs`, `expects_graph` only for `GraphWrite`), and no `dependency`.
- **The vocabulary.** `Action::"read-state"` on `Label::"<label>"` (`polar-policy/src/schema.rs:88`), its context the
  topic actions' `{ now_secs, declared, implied }` (`policy-enforcement.md`, "Decisions").
- **The messages.** `soft-state/common/src/lib.rs`: `StateRequest::Max`, `StateReply`, in the request/reply
  `Envelope` over rkyv; `REQUESTS_TOPIC`, `REPLIES_FAMILY`, `replies_topic`; `ask` (`:111`), one attempt with a fresh
  correlation id; `deliver` (`:138`).
- **The reader.** `soft-state/observe`: a builder agent, `NEEDS [GraphRead]`, subscribing to `soft-state-requests`
  and publishing on the family `soft-state-replies.{workload}` expanded over the readers
  (`soft-state/observe/src/agent.rs:49`). The decision is `decision.rs` (`checked`, `:70`; `decide`, `:93`): no
  verified asker, or a label or property that is not a plain identifier, refused; then Cedar; where policy is silent,
  the floor (`floor.rs`, `declares_read`, `:125`), matched by the exact identity; anything else refused; policy that
  cannot be consulted is `Unavailable`. The query is built in `query.rs` (`max_cypher`, `:34`): `MATCH (n:`L`) WHERE
  n.`p` IS NOT NULL RETURN max(n.`p`) AS value`, a non-integer value refused (`reply_of`, `:45`), a graph that does not
  answer within 10 s or fails `Unavailable` (`read_max`, `:57`). Each request is answered in a task of its own, on the
  asker's reply topic (`supervisor.rs`, `answer`, `:108`; `reply_topic_for`, `:130`).
- **The render.** `infra/graph.nu`: `readers`, `unowned_reads`, `state_floor`; `infra/resolve.nu`: `collect_into`
  asks every image for its scaffold, then again, with `POLAR_STATE_READERS`, the images whose scaffold has a family over
  the readers (`:226`); `resolve` writes `POLAR_STATE_READERS` (`:582`) and `SOFT_STATE_FLOOR` (`:604-614`) into the
  state agent's document; `check_all` fails a read of a label no workload of the target owns. The catalog row is
  `infra/config/workloads/soft-state.nuon`; the image `softStateObserverImage` (`soft-state/package.nix`).
- **The Jira cursor.** jira-processor writes `updated_ms` (`jira/process/src/issues.rs`, `parse_updated_ms`, `:62`;
  written at `:355`); jira-observer asks before its first issue pull (`jira/observe/src/issues.rs`, `learn_start`,
  `:134`; `start_from`, `:75`; `cursor_minutes`, `:94`; `jql_filter`, `:352`; `issue_jql`, `:378`), through
  `state_asker` (`jira/observe/src/supervisor.rs:81`) on its reply topic (`STATE_REPLIES_TOPIC`, `:49`), hearing only
  the state agent (`from_state_agent`, `:57`); a state agent that has not said is retried as a failed pull is
  (`lib.rs`, `JiraError::no_cursor`, `settle`).

Decided where the design was silent (by the agent that built it, by the precedents named; to confirm):

1. **The reply family is expanded over the readers, not the served workloads.** Expanded over `POLAR_SERVED_WORKLOADS`,
   every document agent would have a reply topic only the state agent publishes and nobody subscribes to, which the
   catalog's cross-check fails (`topics.md`, "The cross-check"), and the state agent's certificate would carry a topic
   for each. So a family can say what it is expanded over (`FamilyDecl::over`, `polar-agent-builder/src/topics.rs:218`,
   `Over::StateReaders`), the render fills `POLAR_STATE_READERS` from the scaffolds' reads, and the state agent's
   scaffold is asked a second time (above). The scheduler's families are unchanged.
2. **The floor is a variable of the state agent's own document,** `SOFT_STATE_FLOOR` (JSON `{ format: 1, workloads:
   [{ workload, identity, owns, reads }] }`), written by the render as it writes `CREDENTIALS_AGENT_AUTHORITY_MODEL`
   (`infra/resolve.nu`), from every workload of the target that declares a label. Empty, no one may read anything
   unless policy permits it; one that does not parse is a configuration problem at start (`floor.rs`, `relations`).
3. **The asker is matched by its exact identity**, `<workload>.<namespace>.serviceaccount.<domain>` as the floor
   writes it, not by `workload_of` alone: a service account of the same name in another namespace is not it.
4. **Only a declared read is admitted by the floor;** owning a label is not reading it through the state agent
   (an owner has the graph already). The state agent declares no label of its own.
5. **`context.implied` is always false** for `read-state`, and the state agent asks policy with no certificate of
   the asker's (it holds none), so a rule on `principal in Group::"x"` holds only through the signed groups document.
6. **A request with no verified asker, or from an identity that names no workload, is dropped** with a warning: it has
   no reply topic. A refusal names only what the asker sent and its own identity (a proptest holds it), and the reason
   policy could not be consulted is logged, not sent.
7. **`graph-read` has no `dependency`:** polar-init does not wait for Neo4j, and the health check does not watch its
   certificate, so nothing about the state agent's health depends on the graph.
8. **Placement:** the writers' pool, since it holds the graph password (`placement.md`).
9. **Timeouts:** the state agent waits 10 s for the graph; jira-observer waits 8 s for an answer, under its observers'
   10 s stop bound, and asks again on its backoff (30 s doubling to 15 minutes) with a fresh correlation id.
10. **A value that is not an integer is refused**, not unavailable: asking again would not change it.
11. **jira-observer accepts a reply only from the state agent's identity** (`published_by`); it declares that it reads
    `JiraIssue`, and jira-processor declares the six labels with fixed names it writes. The `JiraIssue_<field>` labels
    it makes from custom fields carry Jira's names and are not declared.

## Gaps kept open

- Neo4j has one superuser for every agent (`infra/config/capabilities.nuon:77-97`); "reader only" is the code's
  discipline until Neo4j roles exist.
- Pagination by `startAt` can skip an issue updated during a pull (it moves to the end); the existing incremental pull
  has the same hazard. Not changed here.
- The display name `Updated` is Jira's English name; a Jira in another language names it otherwise.
- No index on `updated_ms`; `Max` scans the label once per observer start.
- A graph whose issues all predate `updated_ms` answers that none has it, so the first start after the upgrade pulls
  everything (or from `QUERY_DATE`) once.
- Not yet run on a cluster: the state agent's image is built, not deployed; the two-pass scaffold collection has run
  only from the workspace's binaries (`infra/tests/catalog-topics.nu`), not against a registry.
- The writer half (`soft-state-processor`) and the git checkpoints are not built.
