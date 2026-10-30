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
written, not an explicit cursor node), not built. Written from the code as it stands on `main` (`1b82f624`); paths are relative to
`src/agents/`. First user: the Jira observer's issue cursor (`jira.md`, "Open").

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

## Gaps kept open

- Neo4j has one superuser for every agent (`infra/config/capabilities.nuon:77-97`); "reader only" is the code's
  discipline until Neo4j roles exist.
- Pagination by `startAt` can skip an issue updated during a pull (it moves to the end); the existing incremental pull
  has the same hazard. Not changed here.
- The display name `Updated` is Jira's English name; a Jira in another language names it otherwise.
- No index on `updated_ms`; `Max` scans the label once per observer start.
