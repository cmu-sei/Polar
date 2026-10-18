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
```

### Nushell

The CI/CD pipeline library (`scripts/pipeline/core/`), Polar's own pipeline
(`scripts/pipeline/ci.nu`), the analyzers (`src/analyzers/`) and other repo
scripts are nushell. They are held to the same bar as the Rust: formatted with
`nufmt`, linted with `nu-lint`.

```bash
# Format (nufmt defaults; there is deliberately no nufmt.nuon — see Rules)
nufmt <files or directories>

# Check formatting without writing (non-zero exit if a file would change)
nufmt --dry-run <files or directories>

# Lint. Pass the config explicitly: nu-lint 1.2.1 does not pick up
# .nu-lint.toml on its own. Exits non-zero on warnings or errors; hints are
# advisory and don't fail the gate.
nu-lint -c .nu-lint.toml <files or directories>

# Parse check. Library files are modules; scripts with top-level code
# (ci.nu, polar-init.nu) are checked without --as-module.
nu -c 'nu-check --as-module <file>'
```

## Repo Structure

```
src/agents/               Rust workspace (Cargo.toml lists every member crate)
├── lib/                  Shared `polar` crate: events, graph controller and node keys,
│                         topics, Cassini client wrapper. Start here for common types
├── cassini/              Message broker (broker, client, types, tracing, cassini-c2, test harness)
├── build-orchestrator/   Cyclops, the authoritative builder: agent, core, k8s and podman
│                         backends, analysis scheduler
├── build-processor/      Projects build, artifact, finding and registry events into the graph
├── polar-operator/       The PolarAgent CRD (common), the operator that keeps agents alive
│                         (operator), and the agent processor that projects their lifecycle
├── resolver/             Resolves discovered image refs to verified registry manifests
├── git/                  Git agents (observe, process, scheduler, common)
├── gitlab/               GitLab agents (observe, consume, query, schema, common)
├── jira/                 Jira agents (observe, process, query, common)
├── kubernetes/           K8s agents (observe, consume, common)
├── openapi/              OpenAPI agents (observe, process, common)
├── polar-scheduler/      Schedule agents (observer, processor, common, test mock agents)
├── cert-issuer/          Short-lived mTLS client certs for agents (server, client, common)
├── polar-health/         Shared health types and the HealthCheckActor
├── polar-healthcheck/    Liveness probe binary
├── polar-init/           Init container binary for agents
├── nu-init/              Minimal nushell init container (image only, no crate)
├── git-server/           Local git HTTP server for development (image only, no crate)
├── policy-config/        Git-backed configuration repo operations
└── logger/               Event logger

src/analyzers/            Hardened analyzer images the orchestrator runs against package sources
src/flake/, src/containers/, src/deploy/, src/conf/   Nix/Dhall build, container and deploy definitions
scripts/pipeline/         Polar's own CI pipeline (ci.nu) and the instrumented-pipeline
                          library (core/) that any pipeline can use to emit events
infra/                    Layered infrastructure rendering (render.nu, Dhall layers, targets)
docs/architecture/        Architecture docs, one folder per subsystem
```

Agent crates have a `main.rs` (a few use `entrypoint.rs`) as the binary
entry, usually a `lib.rs` for exports and constants, and feature modules
beside them. Binaries with several parts split them into sibling crates under
one directory (e.g. `git/observe`, `git/process`).

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

### Nushell

- Import modules by a path relative to the importing file
  (`use ../../../scripts/pipeline/core/events.nu [...]`), never through an
  include path (`nu -I`). Relative imports resolve identically in the repo, in
  images (which mirror the repo layout), and under `nu-check`/`nu-lint`.
- Declare input and output types on every `def`; avoid `any` where the type is
  known (`nothing -> oneof<string, nothing>`, not `nothing -> any`).
- Build lists with `++=`, not `$x = ($x | append ...)`.
- A deliberate exception to a lint rule goes on that line,
  `# nu-lint-ignore: <rule_id>`, with a comment saying why. Lowering a rule for
  the whole repo in `.nu-lint.toml` needs a stated convention it conflicts
  with, recorded next to the setting.
- Event payloads must match the `ProvenanceEvent` shapes in
  `src/agents/lib/src/events.rs` exactly. When changing an `emit-*` function,
  capture its real JSON (run it with a stub `cassini-client` on `PATH` that
  appends stdin to a file) and pin it in a Rust deserialization test.

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

When the change touches nushell:

1. `nufmt <changed .nu files>`, then `nufmt --dry-run` on them must report no
   changes. Run it twice if the first pass still reports changes: nufmt 0.1.x
   occasionally needs a second pass to settle.
2. `nu-lint -c .nu-lint.toml <changed .nu files>` must exit zero. Don't
   introduce warnings into files that are currently clean; `src/analyzers/` is
   clean and must stay clean.
3. `nu-check` every changed file (see above).
4. Exercise what changed. Formatting and lint fixes are not exempt: compare
   outputs before and after (e.g. emitted payloads, analyzer findings on real
   crates).

### Rules

- Make the smallest change that fixes the problem. Do not refactor adjacent code unless it is directly blocking the fix.
- If `cargo check` fails after a change, fix it before moving on — do not accumulate broken state.
- If a test requires external services (Neo4j, broker), note it and skip rather than fail silently.
- Do not add dependencies to `Cargo.toml` without a clear reason tied to the task.
- Do not add a `nufmt.nuon`. In nufmt 0.1.4, writing out the documented
  defaults is not equivalent to having no config (an explicit `margin: 1` adds
  blank lines between every top-level item, including consecutive `use`
  lines), and the result isn't idempotent. The defaults are what the repo is
  formatted with.
