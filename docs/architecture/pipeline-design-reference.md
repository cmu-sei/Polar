# Polar CI Provenance Pipeline — System Design Reference

## 1. Purpose

This pipeline instruments a Cargo build to emit structured provenance events —
what was built, from which source, against which dependencies, with which
known vulnerabilities — and projects them into a Neo4j graph. It is not a
dashboard and not a risk-scoring tool. It is a knowledge graph. A
queryable, append-oriented history of what was observed, by which tool, under
which configuration, at which point in time.

That framing is load-bearing and worth stating before anything else, because
it explains nearly every modeling decision below that isn't the obvious
simplest choice. A risk tool wants one collapsed, actionable number per
package. A forensic tool wants to answer, months later: *what did we know,
from which scanner, under which configuration, and does a missing finding
mean "not vulnerable" or "not yet known to be vulnerable"?* Every place this
schema is more granular than it strictly needs to be — keeping a scanner's
`kind` distinct from its `severity`, refusing to collapse `CVE`/`GHSA`/
`RUSTSEC` into one identity, letting an unresolved finding persist with no
package edge rather than discarding it — is that principle in practice.

## 2. Pipeline Walkthrough

```
nushell CI (ci.nu)                     Rust ingestion (build-processor)
─────────────────────                  ────────────────────────────────
Phase 0  tool preflight
Phase 1  cargo-cyclonedx SBOMs   ──┐
         → sbom.nu parses          │    Cassini broker
         → state.nu records        │    (BUILD_EVENTS_TOPIC)
           purl index (stor)        │           │
Phase 1.5 cargo-audit scan          │           ▼
         → scanning.nu resolves ───┼──►  BuildProcessorSupervisor
           findings against the    │      deserialize_and_dispatch
           purl index (never       │      (rkyv first, JSON fallback)
           reconstructs a purl)    │           │
         → events.nu emits         │           ▼
           typed JSON/rkyv events  │      dispatch → routes by variant
Phase 2  cargo build + link        │      ┌──────────────┬─────────────┐
         → BinaryLinked events ────┤      lifecycle        artifact domain
Phase 3  container image build     │      (BuildActor /   (ProvenanceLinker)
         → purl cross-check        │       project_event)  SBOM, Binary,
           against Phase 1's       │                       Package, Security-
           recorded index          │                       Advisory, Package-
         → ContainerImageCreated ──┘                       Status, OCI*
                                                │               │
                                                └───────┬───────┘
                                                         ▼
                                                  GraphController actor
                                                  (UpsertNode / EnsureEdge /
                                                   RawQuery) ──► Neo4j (bolt)
```

The pipeline is written so that later "phases" can trust earlier ones.

Please note that in the context of this documentation, we use the term "phase" to describe an arbitrary section of the nushell script that gets executed. It is not to be likened to "stages" or "jobs". The Polar pipeline is intended ot be executed as one cohesive process.


Phase 1.5 resolves every vulnerability finding's package identity against the exact
purl index Phase 1 just built from the SBOM — it never reconstructs a purl
string by hand, which is the single decision that makes the resulting
`AFFECTS` edges trustworthy rather than a source of silent orphan nodes (see
§5.1). Phase 3 uses that same recorded index to catch drift between a
container manifest's hardcoded purl and what was actually analyzed.

## 3. Event & Wire Model

`ProvenanceEvent` is the single canonical event type every producer emits and
every consumer deserializes — nushell CI stages, Kubernetes/GitLab observer
agents, and the resolver all speak it. Two wire formats are accepted:
**rkyv** (the hot path for Rust-native producers) attempted first, **JSON**
(what `events.nu`'s `emit-*` functions produce) as fallback. Both decode into
the identical `ProvenanceEvent` enum — there is no separate JSON-specific
schema to keep in sync.

`ProvenanceEvent` is `#[serde(tag = "type", rename_all = "snake_case")]` —
internally tagged, and critically, **not** `deny_unknown_fields`. That means
a nushell emitter can add a new optional field to an event's JSON payload and
ship it immediately; the Rust side simply ignores fields it doesn't yet
destructure, until the corresponding struct field is added. This is a
deliberate property, not an accident — it's what let the SBOM/vulnerability
integration work in this pipeline ship the nushell emitter changes well
ahead of the Rust projection logic, repeatedly, without a synchronized
two-sided deploy.

High-volume events (SBOM component sets, per-finding scan results) are typed
payload structs (`SbomGraphFragment`, `SecurityAdvisoryFoundPayload`,
`PackageStatusFoundPayload`, etc.) rather than flat inline fields on the
enum variant — this keeps `handle()` match arms to `(state, payload)` instead
of long positional-argument lists, and avoids the field-reordering-silently-
misbinds risk that positional destructuring invites.

## 4. Graph Data Model

### 4.1 The type-marker singleton idiom

Two singleton nodes exist purely as polymorphic query anchors, not as
instances of anything: `Artifact` and `Finding`. Every produced-thing
(`Binary`, `BuildArtifact`, `ContainerImage`, `Sbom`, `OCIArtifact`) carries
an `(x)-[:IS]->(Artifact)` edge; every security-relevant fact
(`SecurityAdvisory`, `PackageStatus`) carries `(x)-[:IS]->(Finding)`. This
lets a query pull "everything produced" or "everything finding-shaped"
regardless of concrete label, without forcing heterogeneous concrete types
(a `Binary` keyed on content hash, a `Sbom` keyed on a different content
hash, a `SecurityAdvisory` keyed on a RUSTSEC identifier) into one shared
property schema. It's used twice, deliberately the same way both times, and
should be reached for again rather than reinvented if a third heterogeneous
category needs the same treatment.

### 4.2 Node ownership

Each processor owns writes to a specific subset of node types and only ever
creates *edges* (never upserts properties) to node types owned elsewhere —
this avoids one processor clobbering data another processor is
authoritative for.

| Owner | Owns (upserts freely) | References only (via EnsureEdge) |
|---|---|---|
| Lifecycle projector | `BuildJob`, `BuildStage`, `SecurityAdvisory`, `PackageStatus` | `GitCommit`, `BackendJob`, `Package` |
| Artifact linker | `Sbom`, `Binary`, `Package`, `ContainerImage`, OCI* | — |

### 4.3 Domain inventory

**Build lifecycle** (`BuildNodeKey`): `BuildExecution` (singleton IS-anchor,
build's own version of the pattern in §4.1), `BuildJob { build_id }`,
`BuildJobState { build_id, valid_from }` (temporal snapshot, one per
transition), `BuildStage { build_id, stage_id }`, `BackendJob` (self-
describing — label and merge key come from the backend's own identity, so
this variant works for any CI backend without the processor hardcoding
which one), `BuildArtifact { content_hash }`.

**Artifact/package domain** (`ArtifactNodeKey`): `Artifact` and `Finding`
(the two markers), `OCIRegistry { hostname }`, `ContainerImageRef`
(human-facing tag/digest, not identity), `OCIArtifact`/`OCILayer`/
`OCIConfig` (content-addressed, keyed on digest), `Binary { content_hash }`,
`Sbom { artifact_content_hash }`, `Package { purl }`, `ContainerImage
{ config_digest }`, `SecurityAdvisory { identifier }`, `PackageStatus
{ identifier }`.

*Known wrinkle, documented rather than smoothed over:* `BuildArtifact
{ content_hash }` currently exists as a variant in **both** `BuildNodeKey`
and `ArtifactNodeKey`. Whether these converge to the same graph node depends
entirely on `cypher_match` rendering an identical label and key for both —
unverified as of this writing, and worth resolving explicitly (collapse to
one canonical definition) rather than leaving two enums that can each mint a
key for what's supposed to be the same node.

**Finding domain**, the newest addition — see the spike this document
summarizes for full rationale: `SecurityAdvisory` (a specific identified
issue — cargo-audit's `vulnerability`/`unsound`/`notice` kinds) and
`PackageStatus` (a maintenance-state fact — `unmaintained` — deliberately
*not* modeled as a `SecurityAdvisory`, since it carries no severity and
isn't a discrete flaw). Both key on `identifier` (the scanner-native ID —
RUSTSEC for cargo-audit), both `IS`-link to `Finding`, both link back to the
`BuildJob` that reported them via `REPORTED`, and both conditionally link to
`Package` — `SecurityAdvisory` via `AFFECTS`, `PackageStatus` via
`CONCERNS` (a different edge name specifically so "affects" doesn't imply
an active exploitable flaw where none exists). The `Package` edge is
**conditional**: it's written only when the scanner's reported (name,
version) resolved against the SBOM's own purl index. An unresolved finding
still gets its node, its `Finding` edge, and its `REPORTED` edge — just no
package link. That's an honest gap (dependency-scope mismatch between what
`cargo-audit` scans and what the SBOM enumerated), not a bug to paper over.

### 4.4 Relationship vocabulary

`IS` (type hierarchy, shared across both marker domains), `BUILT_BY`,
`BUILT_FROM` (`Binary → Package` — the edge that makes "what's actually
compiled into this binary" answerable), `PRODUCED`, `DEPENDS_ON` (`Package →
Package`, the SBOM dependency tree), `DESCRIBES` (`Sbom → root Package`),
`REPORTED` (`BuildJob → Finding`, timestamp is a `SET` property on the edge,
*not* part of the MERGE pattern — see §5.5), `AFFECTS` / `CONCERNS`
(`Finding → Package`, conditional), `FOUND_IN` (`Finding → BuildArtifact`,
fallback when no package resolved).

## 5. Design Principles

These are the decisions that would look like unnecessary complexity in
isolation and only make sense against the forensic framing in §1. Recorded
here so they don't get "simplified" away by someone who wasn't in the room.

**5.1 Resolve, don't reconstruct.** A scanner's raw (name, version) is never
hand-assembled into a purl string. It's looked up against the SBOM's own
recorded component index (`state.nu`'s `sbom_packages` table), so a resolved
`AFFECTS` edge always lands on a `Package` node that already exists — it
cannot silently MERGE an orphan from a purl string that merely *looks* right.
This was motivated by a real, confirmed multi-ecosystem risk (a container
scanner's artifact-type label doesn't map cleanly onto purl's type
namespace) that turned out not to apply to the cargo-only case, but the
resolve-first design is kept regardless because it's strictly safer and
costs nothing.

**5.2 Kind over severity.** Real cargo-audit output showed CVSS absent on
the majority of genuine findings — RUSTSEC scores almost nothing, GHSA
dominates over CVE as the cross-reference namespace 7-to-1 in one
representative scan. A schema (or a habit of writing `WHERE severity IN
['high','critical']`) that treats severity as the primary filter silently
hides most real findings. `kind` (vulnerability / unsound / notice /
unmaintained) is populated on effectively everything and is the durable
categorical signal; severity is an enrichment, present when available,
honestly `"unknown"` otherwise.

**5.3 Identity is never assumed to transfer across ecosystems.** Purl
reliability for `pkg:cargo` rests on a specific, checked crates.io property
(hyphen/underscore name collisions are actively rejected at publish time).
That property does not automatically hold for a hypothetical future
`pkg:deb`/`pkg:rpm` addition, and the schema is not designed to pretend it
does — `Package` stays narrowly scoped to what's actually been verified,
with the marker-plus-concrete-subtype pattern from §4.1 as the known,
deliberately-not-yet-built path for when a second ecosystem is real.

**5.4 Defer generalization until a second concrete case exists.**
Cross-scanner vulnerability identity reconciliation (the same CVE reported
under a RUSTSEC ID by one scanner and a native CVE ID by another) is a real,
industry-wide unsolved problem — not something to solve speculatively with
one scanner in production. The schema reserves a landing spot for a future
canonical-identity node without building the reconciliation logic that would
populate it. Same treatment for `yanked` crates (no advisory to key a node
on, deferred) and for container/OS package modeling (out of scope until
containers are actually in scope).

**5.5 Batch where fan-out is real; keep atomicity everywhere else.** One
`SbomAnalyzed` event can carry hundreds of components — batched into a
handful of `UNWIND`-based `RawQuery` transactions instead of one transaction
per node/edge, because that fan-out is real per-event. A `SecurityAdvisory
Found` event carries exactly one finding — there's nothing to batch within
it, so it's one atomic `RawQuery` per event instead, chosen specifically to
avoid the failure mode where a finding's node lands without its `IS`/
`REPORTED` edges because each was a separate transaction. Batching is not a
blanket policy; it's applied only where measurement (or, in the SBOM case,
obvious transaction-count arithmetic) justifies it. Edge timestamps
(`REPORTED.at`, `AFFECTS.at`) are written as a post-`MERGE` `SET`, not
embedded in the `MERGE` pattern itself — putting a timestamp inside a MERGE
pattern makes it part of the relationship's *identity*, so every re-report
would mint a new edge instead of updating one. The one exception, confirmed
correct rather than a bug: a `REPORTED` edge legitimately multiplies once
per distinct `build_id`, since a second build finding the same advisory is a
genuinely separate provenance fact, not a duplicate.

## 6. What This Enables — Example Queries

Orphan detection (unresolved findings, and why they're unresolved):
```cypher
MATCH (a:SecurityAdvisory)
WHERE NOT EXISTS { (a)-[:AFFECTS]->(:Package) }
RETURN a.identifier, a.kind, a.severity, a.cve_id, a.ghsa_id
```

Everything finding-shaped, regardless of concrete type:
```cypher
MATCH (f)-[:IS]->(:Finding) RETURN labels(f), f.identifier, f.kind
```

What's actually compiled into a shipped binary versus merely present in a
dependency tree (once §7's scope-property work lands — see below):
```cypher
MATCH (b:Binary)-[:BUILT_FROM]->(p:Package)<-[:AFFECTS]-(a:SecurityAdvisory)
RETURN b.content_hash, p.purl, a.identifier, a.severity
```

Provenance time series — every build that has ever reported a given
advisory:
```cypher
MATCH (j:BuildJob)-[r:REPORTED]->(a:SecurityAdvisory { identifier: $id })
RETURN j.build_id, r.at ORDER BY r.at
```

## 7. Current State

Written to be honest about the difference between *confirmed working*,
*designed but not built*, and *deliberately deferred* — these get conflated
easily over a long-running effort, and that conflation is exactly how
documentation drifts from what's actually true.

**Confirmed working, verified against a live graph:** SBOM parsing into the
stor-backed purl index; cargo-audit resolution through that index (never
reconstructed); the `SecurityAdvisory`/`PackageStatus` split, both wired
through corrected, flat-parameter `RawQuery` handlers (no nested-map
marshalling, no edge-timestamp-in-MERGE bug); the batched `UNWIND`-based SBOM
projection; the `REPORTED` per-build fan-out behavior.

**Designed, not yet built:** a merged single projection actor
(`ProvenanceProjector`) replacing the current `BuildActor`/`ProvenanceLinker`
split — drafted, then explicitly paused to resolve the batching approach
first, and not returned to since. `ScanContext` (persisting scanner
version, advisory-database commit, and SBOM-tool flags as their own
forensic fact) — proposed as an issue, not implemented. CycloneDX `scope`
capture (distinguishing a package's runtime-vs-build-vs-dev relationship to
a *specific* SBOM, correctly modeled as an edge property rather than a
`Package` node property) — designed, not implemented. Widening SBOM scope to
match cargo-audit's full lockfile closure — root-caused to a concrete,
confirmed example (a registry crate present in `Cargo.lock` but absent from
every generated SBOM), mechanics researched (dev-dependencies default-
excluded with no confirmed override flag; build-dependencies included by
default but tagged non-runtime; platform-conditional dependencies
structurally unclosable by a single invocation) — not implemented.

**Deliberately deferred, by design, not oversight:** cross-scanner
vulnerability identity reconciliation (RUSTSEC/CVE/GHSA convergence) —
reserved a schema landing spot, no reconciliation logic; `yanked` crate
handling — no advisory to key a node on; container/OS package modeling — out
of scope until containers are; the hash-chained append-only event log — a
separate, pre-existing issue, explicitly not tackled as part of this effort,
though the forensic framing in §1 raises its priority relative to when it
was first set aside.

## 8. Open Threads

In roughly the order they'd naturally get picked up: confirm and fix the
`build_id` gap (§7), since it undermines timeline integrity broadly, not
just for findings; decide and implement SBOM scope widening plus `scope`
capture together, since they're the same piece of work; complete or
abandon the `ProvenanceProjector` merge now that the batching question that
paused it has an answer (per-event atomicity for findings, per-SBOM batching
for components); build `ScanContext` if audit/compliance evidentiary use
becomes a live priority; resolve the `BuildArtifact` dual-enum wrinkle in
§4.3 before it causes a real divergent-node bug rather than a documentation
footnote.

## 9. Glossary

**purl** — Package URL, a scheme for encoding package identity
(`pkg:type/name@version`). Reliability varies by ecosystem; verified for
`pkg:cargo` here, not assumed elsewhere. **SBOM** — Software Bill of
Materials; here, CycloneDX JSON produced by `cargo-cyclonedx` per workspace
member. **RUSTSEC** — the Rust ecosystem's own advisory database
(volunteer-maintained, not centrally scored). **GHSA** — GitHub Security
Advisory, the de facto advisory-of-record for much of the open-source
ecosystem, often present where a CVE never was requested. **CVE** — the
MITRE-assigned identifier most people mean by default; frequently absent or
backfilled later for Rust-specific issues. **scope** (CycloneDX) — a
component's `required`/`optional`/`excluded` relationship to a *specific*
build — not an intrinsic property of the package itself.
