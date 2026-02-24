
## 1. How to model Git as a traversable knowledge graph

The key mistake to avoid is trying to model *Git commands* instead of *Git invariants*. Git already gives us a near-perfect graph. our job is to **preserve its semantics without smearing them across time or transport**.

### Core entities (nodes)

we want **stable, content-addressed nodes** wherever possible.

#### Repository

* **Identity**: `RepoId` (our UUIDv5 is the right call)
* Properties:

  * canonical_url
  * first_seen_at
  * observation_policy_id (optional, later)

This node exists even if no commits exist yet.

#### Commit

* **Identity**: `(repo_id, oid)`
* Properties:

  * oid (string, but treat as opaque)
  * author_name
  * author_email (optional, but don’t drop it)
  * authored_time
  * message
  * committer info (us’ll want this later)

This is our most important invariant: **commits are immutable**. Once emitted, they never change.

#### Ref

* **Identity**: `(repo_id, ref_name)`
* Properties:

  * name (`refs/heads/main`)
  * kind (`branch`, `tag`, `remote-branch`)
  * first_seen_at

Refs are mutable pointers. Do not pretend otherwise.

---

### Relationships (edges)

Edges are where the graph becomes useful.

#### `COMMIT_PARENT`

* `(:Commit)-[:PARENT]->(:Commit)`
* Direction: child → parent
* This is the DAG. Do not encode order elsewhere.

#### `REF_POINTS_TO`

* `(:Ref)-[:POINTS_TO]->(:Commit)`
* This edge **changes over time**
* us may want temporal edges or versioned edges later

#### `REPO_HAS_REF`

* `(:Repository)-[:HAS_REF]->(:Ref)`

#### `REPO_HAS_COMMIT`

* Optional but often useful for scoping queries
* Can be derived, but explicit edges make traversal cheaper

---

### What us should *not* model yet

* Trees
* Blobs
* Diffs
* File paths

Those are second-order artifacts. we can always add them later once commit-level traversal proves useful.

---

## 2. How to emit events without poisoning the model

Emitting events should avoid mixing **three concerns**:

1. Commit existence (Discovery)
2. Commit graph structure (Persisten)
3. Ref movement

Those should be **separable**.

### Rule of thumb for event design

> Emit **facts**, not interpretations, and make them **idempotent**.

our downstream system should be able to process the same event twice without corrupting the graph.

---

## Recommended event taxonomy

Instead of “one enum to rule them all”, think in terms of **orthogonal facts**.

### 1. CommitDiscovered

Emitted **once per commit per repo**, ever.

```rust
GitRepositoryMessage::CommitDiscovered {
    repo: RepoId,
    oid: String,
    author: Author,
    committer: Committer,
    time: i64,
    message: String,
    parents: Vec<String>,
}
```

Semantics:

* “This commit exists”
* “Here is its immutable metadata”
* “Here is its position in the DAG”

Downstream:

* Create Commit node
* Create PARENT edges

Idempotency:

* `(repo, oid)` is the natural key

---

### 2. RefUpdated

Emitted **whenever a ref tip changes** (including first observation).

```rust
GitRepositoryMessage::RefUpdated {
    repo: RepoId,
    ref_name: String,
    old: Option<String>,
    new: String,
}
```

Semantics:

* “This pointer now points here”
* Force-pushes become explicit, not magical

Downstream:

* Update or version `REF_POINTS_TO`
* Optionally keep history of ref movement

Important:
This event should be emitted **after** commits are discovered, or at least be reorder-tolerant.

---

### 3. RepositoryObserved (optional, but useful)

```rust
GitRepositoryMessage::RepositoryObserved {
    repo: RepoId,
    observed_at: i64,
}
```

This becomes gold later for:

* Liveness
* Alerting
* Detecting stale repos

---

## How this changes our worker logic (conceptually)

Right now our worker does:

> walk commits → emit commit event (with ref info baked in)

Instead, the clean sequence is:

1. **Walk commits**

   * For each commit:

     * Emit `CommitDiscovered`
2. **After walking a ref**

   * Emit `RefUpdated` with:

     * previous `last_seen`
     * new tip

our `last_seen` map is already exactly what us need to do this cleanly.

---

## Minimal refactor of `on_commit`

our current function is doing too much. Split it.

```rust
fn emit_commit(
    repo_id: RepoId,
    commit: &git2::Commit,
    tcp_client: ActorRef<TcpClientMessage>,
) -> Result<(), ActorProcessingErr> {
    let event = GitRepositoryMessage::CommitDiscovered {
        repo: repo_id,
        oid: commit.id().to_string(),
        author: extract_author(commit),
        committer: extract_committer(commit),
        time: commit.time().seconds(),
        message: commit.message().unwrap_or_default().to_string(),
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
    };

    send_event(tcp_client, event, GIT_REPOSITORIES_TOPIC.to_string())
}
```

And separately, after a ref walk:

```rust
fn emit_ref_update(
    repo_id: RepoId,
    ref_name: &str,
    old: Option<Oid>,
    new: Oid,
    tcp_client: ActorRef<TcpClientMessage>,
) -> Result<(), ActorProcessingErr> {
    let event = GitRepositoryMessage::RefUpdated {
        repo: repo_id,
        ref_name: ref_name.to_string(),
        old: old.map(|o| o.to_string()),
        new: new.to_string(),
    };

    send_event(tcp_client, event, GIT_REPOSITORIES_TOPIC.to_string())
}
```

Now:

* Commits are repo-global facts
* Refs are mutable relationships
* our graph can answer real questions without guesswork

---

## Why this matters downstream

With this model, us can answer queries like:

* “Show me all commits reachable from main that were not reachable last week”
* “Which commits survived a force-push?”
* “Which downstream artifacts depend on commits that are no longer referenced by any branch?”
* “What repos stopped moving?”

Those are observability questions, not Git trivia. And our current direction absolutely supports them.

---

## Final blunt assessment

us’re doing the right thing by stopping here and asking this question *before* piling on more logic.

our worker architecture is solid.
our traversal strategy is correct.
The only thing that needed tightening was **semantic separation of facts**.

Do that, and the knowledge graph becomes an asset instead of a liability.

If us want, next we can:

* sketch Cypher-style queries against this model
* talk about ref history retention vs overwrite
* or align this with SBOM / artifact provenance edges

Just say where us want to push next.
