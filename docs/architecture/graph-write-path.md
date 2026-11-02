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

# The graph's write path

How an agent's writes reach Neo4j: the node keys, the graph controller's compiler, the constraints the owners make at
start, and the repair of what was written wrong before. Built 2026-11-01 for 3.0.0 after what u7c showed
(`jira.md`, "Seen on a cluster (u7c, sandbox-local, 2026-11-01)"). Paths relative to `src/agents/`. Nothing here has
run on a cluster yet.

## The pieces

- **A node key says what identifies its node, nothing else.** `GraphNodeKey::identity` (`lib/src/graph/controller.rs`)
  returns a `NodeIdentity`: its labels and the key properties with their values, in order. A label known when the
  code is built is `Cow::Borrowed`; one known only at run time (`DynamicNodeKey`, a Jira custom field's
  `JiraIssue_<field>`; `BackendJobIdentity`) is `Cow::Owned`, validated where it was made. The Cypher pattern
  (`cypher_match`), a batch's rows and the constraints are all derived from the identity.
- **A vocabulary lists its variants.** `NodeKeyVocabulary::every_variant` gives one of each variant with placeholder
  values, behind an exhaustive `match`, so a new variant does not compile until it is listed; `shapes_of::<V>()` reads
  the shapes (labels and key names) of the ones with fixed labels. Implemented by `KubeNodeKey`, `BuildNodeKey`,
  `ArtifactNodeKey`, `GitlabNodeKey`, `GitNodeKey`, `ScheduleKey` (`lib/src/graph/nodes/`), `JiraNodeKey`
  (`jira/common/src/graph.rs`) and `CredentialsNodeKey` (`credentials-agent/common/src/graph.rs`).
- **The compiler** (`compile_graph_op`) turns an op into its plan: the statements that write it, in order, run by
  `handle_op` in one transaction, all or nothing. `handle_op` returns how many it ran.
- **The constraints** (`lib/src/graph/schema.rs`) and **the repairs** (`lib/src/graph/repair.rs`), made and run by the
  graph controller at its first contact with the graph, before its first op.

## Every key binds its variable

Seen on u7c: 35,185 nodes with no label and no property, each the target of an `OF_TYPE` from one of kube-consumer's
state instances (PodContainerState, FluxKustomizationState, PodState, ...). `KubeNodeKey::State` matched `(:State)`,
ignoring the variable it was given, so in `UpdateState` `MERGE ({stype})` became an anonymous `MERGE (:State)`, `stype`
stayed unbound, and `MERGE (sinst)-[:OF_TYPE]->(stype)` (and the `HAS_STATE` after it) made a new empty node at every
transition. Upstream (Vaughn, `f604cc43`) had worked around it in the controller with `MERGE (stype {stype})`; `fb7275d1`
changed it back.

Fixed at the cause, by the trait's shape (`e179a4d5`): a key no longer writes its pattern. `NodeIdentity::pattern`
binds the variable and names every parameter `{prefix}_{property}`: `(stype:State)`, `(n:Pod { uid: $n_uid })`. So the
controller's uniform `MERGE ({stype})` is right for every key, and Vaughn's line must not come back with the upstream
merge (it would write `MERGE (stype stype:State)`; `remaining-work.md`, the upstream item).

`pattern_problem` states the rules every op relies on to compose patterns in one statement: the pattern binds its
variable and names a label; it is one node with at most one property map; every parameter is `{prefix}_…` and used;
and nothing else depends on the variable (under another variable the pattern differs only in the variable and the
parameter names). A property test holds every variant of every lib vocabulary to them for any variable, and
`DynamicNodeKey` and `BackendJob` for any valid label; the Jira and credentials vocabularies the same in their crates.

The identity corrected three more keys whose patterns depended on more than the variable:

- `PodState` named its properties after the variable (`{prefix}_uid`, `{prefix}_valid_from`, so `sinst_uid` in
  `UpdateState`): now `pod_uid` and `valid_from`, as `PodContainerState`. The nodes written before keep the old names.
- `DeploymentState` and `ReplicaSetState` lacked their closing parenthesis (harmless only where `UpdateState` trimmed
  it).
- `BuildExecution`, `Artifact` and `Finding` matched a literal `type` and bound a parameter they never used.

The state anchor itself is unchanged: one `(:State)` node, the target of every instance's `OF_TYPE` and of every
resource's `HAS_STATE` (see "Kept open").

## Batches

`UpsertNodes` compiled to one statement with a `MERGE` an item and indexed parameters (`n{i}_{k}`). Apache's 674
projects timed out after 30 s (`ConnectionTimedOut`, the patched neo4rs's receive bound), and an empty batch compiled
to `""`, which Neo4j refuses (`SyntaxError: Unexpected end of input`).

Now (`cab84865`) a node upsert, each node of a batch and an edge become a row of one statement for each label and key
shape:

```cypher
UNWIND $rows AS row
MERGE (n:JiraProject { project_id: row.k.project_id })
SET n += row.p
```

and for an edge, grouped by its two ends' shapes and its type:

```cypher
UNWIND $rows AS row
MERGE (a:JiraIssue { key: row.f.key })
MERGE (b:JiraIssueType { name: row.t.name })
MERGE (a)-[r:HAS_TYPE]->(b)
SET r += row.p
```

A row is `{k: key values, p: properties}` (an edge `{f, t, p}`), so the text is the same for every batch of a shape
whatever its size, and Neo4j plans it once. Within a statement the rows are applied in order, so a node given twice
takes its later properties, as before; `SET n += row.p` sets what is given and leaves the rest, a null removing the
property, as the old `SET n.k = $k` did. An empty batch compiles to no statement: nothing is sent, no transaction is
opened, `handle_op` returns 0, and the controller sends no `Available` for it. Every property name and relationship
type is validated before anything is sent, so an op is refused whole.

## Round trips

`handle_op` ran a `GraphOp::Transaction` one inner op a statement: an Apache issue with its fields, users, type,
project and history was 27 to 35 round trips. Now its ops are planned together, still one transaction an issue:

- Node upserts and edges between two **barriers** are gathered by shape: nodes first, then edges, each group where its
  first item was, its rows in their order.
- A **barrier** is an op whose meaning depends on what was written before it: `ReplaceEdge`, `RemoveEdges`,
  `UpdateState`, `RawMergeWrite`. It is written after everything before it and before everything after it.
- Why moving a node ahead of an edge that came first changes nothing: an edge `MERGE`s its two ends itself and sets
  nothing on them, so the node's properties land the same either way. A node written under two different shapes of one
  label in one transaction (`ContainerImage` by `normalized` and by `config_digest`) could have its two writes
  reordered; no writer does that today.
- Nested transactions flatten in place.

Property tests: a batch's rows carry back every item (labels, key names and values, properties, in order, one
statement a shape); a plan is deterministic; and for any mix and nesting of nodes, edges and barriers, every op is
written once, a node before an edge that follows it and touches it, each barrier after all before it and before all
after it, and never more statements than ops. Against a real Neo4j (`lib/tests/graph_write.rs`, a test binary of its
own so its graph is its own): 1,000 nodes in one statement well within the timeout, an empty batch sending nothing
(to a graph that is not there), a mixed batch leaving the graph the old one-statement-an-item form leaves; and in
jira-processor (`write_bench.rs`), Apache's issues written grouped leaving the graph their ops leave one by one.

The statements an issue fall only from 27/35/27 to 24/27/24 for the three Apache fixtures: each Jira custom field is a
label and an edge type of its own (`JiraIssue_<field>`, `HAS_<FIELD>`), so it is a group of its own. What would reduce
them further, not built: modelling a custom field as one label with the field's name as a property (a schema change for
jira-processor's readers), or writing a transaction's groups as unit subqueries (`CALL { … }`) of one statement, whose
text would then vary with each issue's fields and be planned again for most issues.

## Constraints and indexes

The u7c graph had none: `SHOW INDEXES` listed only Neo4j's two LOOKUP indexes, so every `MERGE` scanned its label.
The maintainer's decision: each agent's builder makes, at start, the uniqueness constraints for the labels the agent
declares it owns, on the properties that label's node key merges on, derived from the node key types.

- **Declared once.** `PolarAgent::GRAPH` says which labels the agent owns; `PolarAgent::GRAPH_KEYS` lists the
  vocabularies it writes with (`&[shapes_of::<JiraNodeKey>]`). `graph_key_indexes::<A>()`
  (`polar-agent-builder/src/agent.rs`) derives the constraints with `polar::graph::schema::key_indexes`;
  `Configured::graph_key_indexes` hands them to `GraphControllerArgs::with_key_indexes`.
- **The rule.** For each owned label, each distinct set of key properties a shape merges it on; a shape with no key
  (the `State` anchor) needs none, and a label owned but in no shape (raw Cypher, a run-time label) gets none. One key
  set: a property uniqueness constraint, `polar_unique_<Label>_<keys>`. Several key sets on one label: a range index
  for each, `polar_index_<Label>_<keys>`.
- **Made at start.** The graph controller makes them right after its canary succeeds, before its first op (the actor
  takes no message until `post_start` returns); a graph that cannot be asked keeps them pending for the next tick.
  `ensure_key_indexes` makes only what the graph does not have by name (`CREATE … IF NOT EXISTS`), so a second start
  makes nothing, and logs each one it makes at info.
- **Who.** jira-processor: unique on `JiraIssue.key`, `JiraProject.project_id`, `JiraUser.key`, `JiraGroup.name`,
  `JiraIssueType.name` and `JiraIssueChangeLog(baseId, id)`. kube-consumer now declares the 21 Kubernetes labels it
  writes as owned (not `State`, which build-processor writes too and which has no key), so its scaffold gains a `graph`
  section; unique on each single-key label and on the state instances' composite keys
  (`PodContainerState(pod_uid, name, valid_from)`, ...), and range indexes on `FluxOCIRepository(uid)` and
  `FluxOCIRepository(name, namespace)`. A reader (jira-observer) makes nothing.
- **A state instance's time.** A Kubernetes state key's `valid_from` is now the epoch milliseconds (`i64`) its
  `UpdateState` also SETs as `valid_from`. As text, the `MERGE` never found the node the SET had overwritten with the
  integer, so a state seen again (a container's unchanged status in the next pod event) made another node; under the
  composite uniqueness constraint that would fail the write. The state nodes already written hold the integer, so they
  are found.
- **Duplicates.** Where nodes already share a key, Neo4j refuses the uniqueness constraint. The groups and nodes are
  counted and logged at error ("N keys are each held by more than one node (M nodes in all), left as they are"),
  nothing is deleted, and the range index on the same keys is made instead so `MERGE` still seeks.
- **Neo4j's edition.** The chart deploys `neo4j:5.26.2` (`infra/layers/2-services/neo4j/values.dhall`), built from the
  Community source (`nix-neo4j`, `neo4j-community-*-unix.tar.gz`). Node key constraints are Enterprise and are not used;
  composite property uniqueness constraints are Community, and were made by the tests' Neo4j (5.26.31 Community).

Tests: the derivation is deterministic, independent of the order and repetition of the shapes and the owned labels,
exactly each key set of each owned label, unique exactly when one (property test); only owned labels get one, a
reader none (builder and jira-observer tests). Against a real Neo4j: owned labels get theirs and a second start makes
and changes nothing; `EXPLAIN` of a batch statement (over the HTTP API, `polar_test_graph::explain`, since the Bolt
driver keeps the plan to itself) shows `NodeUniqueIndexSeek` or `NodeIndexSeek` and no `NodeByLabelScan`, which it shows
without them; nodes sharing a key are counted, left, and an index made instead; the controller makes them at start; a
state seen three times is one instance under the constraint.

## Measured

Release builds, x86_64, against the polar-test-graph Neo4j (5.26.31 Community, podman), with
`cargo test --release -p jira-processor write_bench -- --ignored --nocapture --test-threads 1`. Apache's three
fixture issues (`jira/common/fixtures/apache.rs`) are copied as distinct issues (key and history ids suffixed), each
written as the one transaction jira-processor sends; the projects are one `UpsertNodes`.

| What | main (`40ba6c07`) | UNWIND (`cab84865`) | UNWIND and constraints (`46b7a7bd`) |
|---|---|---|---|
| 300 issues, ms an issue | 51.4 (15.4 s, 8,900 statements) | 16.3–17.7 (7,500 statements) | 9.0 (2.7 s) |
| 3,000 issues, ms an issue | not run | 24.8 (74.4 s) | 7.4 (22.1 s) |
| 100 projects | 0.655 s | 0.022–0.035 s | |
| 674 projects | ConnectionTimedOut at 30.0 s | 0.082–0.105 s, one statement | |
| 1,000 projects | ConnectionTimedOut at 30.0 s | 0.117 s, one statement | |

Without constraints the cost an issue grows with the graph (each `MERGE` scans its label); with them it does not.
Not measured: a cluster's Neo4j, whose disk and network differ, and aarch64.

## Cleaning up what was written

The empty nodes already in a graph are removed by kube-consumer at start (`8d6e20c8`), the writer that made them:
`polar::graph::repair::EMPTY_STATE_TYPE_NODES`, run by its graph controller right after the constraints
(`GraphControllerArgs::with_repairs`). The rule: a node with no label and no property, whose every relationship comes
into it and is an `OF_TYPE` or a `HAS_STATE`. Each such `OF_TYPE` and `HAS_STATE` is pointed at the one `:State` node
(`MERGE`, so a resource already pointing there keeps one), as the fixed transition writes them, and the node is removed;
nothing with a label, a property, an outgoing relationship or another incoming one is touched. Batches of 5,000, a
transaction each, until one removes fewer; the count is logged at info (debug when nothing). A `Repair`'s fields are
private, so an agent can only pick one of lib's constants, each a fixed statement reviewed there: the graph controller
deletes nothing a caller wrote.

Why at start rather than an admin Nu command: the writer that made them owns cleaning them, as Polar owns its
housekeeping; the same controller makes the constraints at the same moment; it needs no human step and no forward to
Neo4j; and it is idempotent, so it is harmless where nothing is left. It costs one scan of the graph's nodes a start,
and can be dropped once every graph has been through it. Not run against any cluster.

## The projects that never arrived

On u7c three project pulls (21:34, 22:04, 22:30) left neither an error nor the nodes; only the one after a respawn of
the observers (22:41) reached the processor and timed out. Read 2026-11-01 (the observer's projects path, the Cassini
client and broker, the processor and the controller); not explained without the cluster's logs. What the code says:

- **Once the processor's graph controller has the batch, a failure is never silent**: a failed op logs at error and
  sends `OpFailed` (`lib/src/graph/controller.rs`, `handle`), and nothing hangs (the patched neo4rs bounds each receive
  at 30 s, which is where 22:41's `ConnectionTimedOut` came from). A batch that does not decode warns twice
  (`jira/process/src/supervisor.rs`, `decode_payload`, `dispatch`); one arriving after `PrepareShutdown` warns "shutting
  down -- logging message on topic … will be lost on exit". The earlier batches were the same 674 projects, so had they
  reached the controller each would have timed out with an error as 22:41's did: they did not reach it.
- **Once handed to the Cassini client, a publish is kept until the broker acknowledges or refuses it**, and sent again
  at each registration. The broker queues a publish with no subscriber (info, `topic.rs`) and drops it after 180 s with
  a warning ("Dropped N stale message(s)"). A subscription the broker refused once is held back at debug on every
  re-subscribe (`cassini/client/src/facade.rs`) until authority changes; its first refusal warns. The payload is about
  40 KB (674 `JiraProject`s in rkyv), far under every frame and budget limit.
- **The one silent path: the observer's wait for outbox room.** `publish_when_ready` waits in
  `Credit::wait_for_outbox` (`cassini/client/src/credit.rs`) with no timeout and no log, and the wait is not first come
  first served: `settle` wakes every waiter and the first `try_outbox` to run wins, so a publisher calling it between
  the wake and the waiter's poll overtakes it. The projects, groups and issues observers share one client, and the
  issues observer publishes one message an issue. A projects observer parked there schedules no next pull (its tick is
  sent from `settle` after `pull` returns). But it can wait only while the outbox is full (1,000 messages or 64 MiB not
  acknowledged), an empty outbox admits anything, and the projects observer pulls before the issues observer has asked
  for its cursor, so the 21:34 publish went into an empty outbox. At 22:30 it is ruled out by what the cluster showed: a
  parked actor ignores a stop (ractor takes Stop only between messages), so the signed update's respawn at 22:41 would
  have found the name `jira.observer.projects` still registered and failed the supervisor, which did not happen.
- **No line says a pull happened.** A successful projects pull logs only at debug (`jira/observe/src/projects.rs`), and
  the processor's dispatch only at debug ("Dispatched UpsertNodes op for N projects"), so the logs cannot show that
  22:04 pulled at all.

What would tell, on the next cluster run: the broker's log for `jira:consumer:projects` queued or stale-dropped
messages and for a refused subscription; the processor's current and previous logs for the shutdown warning and for
"Failed to handle graph operation" near 21:34, 22:04 and 22:30; and the groups batch, published at the same moment over
the same connection, whose empty `UpsertNodes` failed at once with an error until this change: if that error is in the
processor's log at 21:34, 22:04 or 22:30, the processor was receiving and the loss is the projects' alone; if it first
appears at 22:41, nothing the observers published at their start reached the processor. Proposed, not built: an info
line at the observer's publish ("published N projects, B bytes") and at the processor's dispatch, and a warning from
`publish_when_ready` after a bounded wait; and a test (beside `publish_when_ready_waits_for_room_and_goes_when_the_broker_acknowledges`,
`lib/src/cassini.rs`) that a publish waiting for room is not overtaken by a later one, which fails today. These touch the
Cassini client, so they wait for the maintainer.

## Decided where the task was silent (to review)

1. **The shape of the fix**: the trait's shape changed (identity, with the pattern derived), not `KubeNodeKey::State`
   alone, since the same class of fault was in four other keys and a derived pattern cannot have it.
2. **PodState's key properties** are `pod_uid` and `valid_from`; the nodes already written keep `sinst_uid` and
   `sinst_valid_from`, not migrated.
3. **One plan for UpsertNodes and Transaction**, nodes before edges between barriers; a node written under two shapes of
   one label in one transaction could be reordered (no writer does it).
4. **Composite keys are unique too**, Community making composite uniqueness constraints; a label merged on more than one
   key set gets a range index on each instead, since one node may be written under each.
5. **Duplicates**: reported at error and left, and the range index made in place of the refused constraint.
6. **Readers make nothing**; kube-consumer now declares its 21 labels as owned (its scaffold changes, and with it the
   state agent's seeded schema the next time it is signed).
7. **A Kubernetes state key's time is an integer**, the value its SET writes.
8. **The cleanup is a repair kube-consumer runs at start**, that also re-points the edges to `:State`.

## Kept open

- `HAS_STATE` points at the one `:State` taxonomy node, not at the resource's current state instance, as `UpdateState`
  always wrote it (`WITH res, stype … MERGE (res)-[:HAS_STATE]->(stype)`); `kubernetes/states.md` describes
  `(:Pod)-[:HAS_STATE]->(:PodState)`. So the pointer says nothing a resource's `TRANSITIONED_TO` does not. A question for
  the maintainer, not changed here.
- The writers that declare no owned labels make no constraints yet: build-processor, gitlab-consumer, git-processor,
  the scheduler's processor and the credentials agent (their vocabularies are `NodeKeyVocabulary` already); soft-state
  writes `AgentHealth` through a `DynamicNodeKey`, 14 nodes.
- kube-consumer sends each `UpdateState` as its own op, a round trip each; not grouped here.
- The state nodes written before the fix keep their duplicates (a state seen again made another), which the composite
  constraints will report at the first start and index instead.
