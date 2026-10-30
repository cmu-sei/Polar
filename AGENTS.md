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

# Polar Agents Guide

## Workspace

Rust workspace root: `src/agents/Cargo.toml`  
Environment template: `example.env` — check here for required env vars before running tests.

## Build & Test Commands

```bash
# Verify compilation across all crates
cargo check --workspace

# Run all tests
cargo test --workspace -- --nocapture

# Run tests for a specific package
cargo test --package <pkg> -- --nocapture

# Static analysis (run before any commit)
just static-analysis
```

## Repo Structure

```
src/agents/
├── cassini/          # Message broker (broker, client, types, tracing)
├── lib/              # Shared polar crate — start here for common types
├── gitlab/           # GitLab agents (observer, consumer, query, schema, common)
├── jira/             # Jira agents (observer, consumer, query, common)
├── kubernetes/       # K8s agents (observer, consumer, common)
├── git/              # Git agents (observer, processor, scheduler, common)
├── openapi/          # OpenAPI agents (observer, consumer, common)
├── provenance/       # Provenance agents (linker, resolver, common)
├── polar-scheduler/  # Scheduler agents (observer, processor, common)
├── config-ops/       # Configuration operations
└── logger/           # Event logger
```

Each agent crate follows: `lib.rs` (exports/constants) + `entrypoint.rs` (binary entry) + feature modules.

## Code Conventions

### Actor Pattern (ractor)

All agents are ractor actors. The canonical shape:

```rust
pub struct MyActor;
pub struct MyActorState { pub field: Type }
pub enum MyActorMsg { Variant(Payload) }

#[ractor::async_trait]
impl ractor::Actor for MyActor {
    type Msg = MyActorMsg;
    type State = MyActorState;
    type Arguments = MyActorArgs;

    async fn pre_start(&self, myself: ActorRef<Self::Msg>, args: Self::Arguments)
        -> Result<Self::State, ActorProcessingErr> { ... }

    async fn handle(&self, myself: ActorRef<Self::Msg>, msg: Self::Msg, state: &mut Self::State)
        -> Result<(), ActorProcessingErr> { match msg { ... } }
}
```

### Messages & Serialization

```rust
#[derive(Debug, Clone, Serialize, Archive, Deserialize)]
pub struct MyMessage { ... }
```

- Use `rkyv` for zero-copy deserialization in hot paths
- Use `Arc<Vec<u8>>` for payloads — do not clone raw bytes
- Use `Arc<OutputPort<(Vec<u8>, String)>>` for output ports (see type aliases in `lib/`)

### Error Handling

- Actor message handlers return `Result<(), ActorProcessingErr>`
- Construct errors with `anyhow::anyhow!("...")`
- Log with tracing macros: `debug!`, `info!`, `warn!`, `error!` — not `println!`
- Never hold an entered span (`span.enter()`, `span.entered()`) across an `.await`; instrument the future instead
  (`fut.instrument(span).await`, `use tracing::Instrument;`). Clippy enforces it: `await-holding-invalid-types` in
  every `clippy.toml`, so a crate that adds its own `clippy.toml` copies that entry

### Constants & Type Aliases

```rust
// Constants at module level
pub const ACTOR_NAME: &str = "my-actor";

// Type aliases for complex generics
pub type GraphController<T> = ActorRef<GraphControllerMsg<T>>;
```

### Testing

- Unit tests: `#[cfg(test)]` module in the same file
- Integration tests: use `testcontainers` for Neo4j and other external deps
- Use `rstest` for parameterized and async test helpers
- Tests that require env vars: check `example.env` first
- Write the test first, then the change; commit when the package's tests and the checks pass
- Write property tests (`proptest`, a workspace dependency) wherever a function has an invariant over its inputs, as
  well as the example tests. They earn their keep where an example can only spot-check:
  - parsers and encoders: a round trip (encode then parse gives back the value) and rejection of malformed input
    (a certificate's `p:` names, topic strings, signed payloads, wire types);
  - decisions: the outcome follows a stated rule for every input (Cedar first, then the floor; default deny; a
    refusal never contains a reserved phrase);
  - computed sets: deterministic, independent of input order, and the union of their parts (an agent's declared and
    implied topics);
  - state machines: an invariant holds after any sequence of events (sessions, retention, aging).
  A property states an invariant of the code under test; one that only restates the generator, or holds for any
  implementation, is not worth having. Keep generators small and shrinkable, and put them beside the tests
  (`tests/props.rs`, as `cassini/broker` and `polar-scheduler` do)

## Workflow

### Change Loop

1. `cargo check --workspace` — catch compile errors early, before scoping a fix
2. `cargo clippy --workspace -- -D warnings` — treat warnings as errors
3. `cargo fmt --all` — format before considering a change done
4. `cargo test --package <pkg> -- --nocapture` — test the affected package
5. `cargo test --workspace -- --nocapture` — full suite before marking complete

### Rules

- Make the smallest change that fixes the problem. Do not refactor adjacent code unless it is directly blocking the fix.
- If `cargo check` fails after a change, fix it before moving on — do not accumulate broken state.
- If a test requires external services (Neo4j, broker), note it and skip rather than fail silently.
- Do not add dependencies to `Cargo.toml` without a clear reason tied to the task.

## Git
- Conventional commits are enforced by a githook.
- Prefer a summary with a detailed body for commit messages.
