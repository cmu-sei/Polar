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
./scripts/static-tools.sh
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
├── policy-config/    # Policy configuration
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
