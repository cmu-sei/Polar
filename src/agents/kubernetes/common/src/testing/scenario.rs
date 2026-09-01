//! Tier 1 scenario runner (issue #221).
//!
//! Loads a synthetic resource, projects it through the real
//! `GraphOperable` impl against a real Neo4j backend, and provides a read
//! connection for assertions -- no Cassini, no k8s cluster, no watch loop.
//!
//! # Environment
//!
//! `GRAPH_ENDPOINT`/`GRAPH_USER`/`GRAPH_PASSWORD`/`GRAPH_DB` must already
//! be set by the time anything here runs -- but unlike a typical
//! testcontainers setup, this file doesn't start a container and then
//! report back what it decided. It reads those four variables first and
//! starts a container *configured to match them* (fixed port, matching
//! credentials) -- `std::env::set_var` never runs, because the env vars
//! are never wrong to begin with. Whatever sets those four variables
//! before the test binary starts (a `.env` file, a CI job's `env:` block,
//! a developer's shell) only needs to pick values, not coordinate with a
//! container that hasn't started yet.
//!
//! # Isolation
//!
//! Neo4j Community Edition supports exactly one database (multi-database
//! is Enterprise-only -- confirmed against Neo4j's own operations manual,
//! not assumed). So this harness does not attempt per-test databases: one
//! `GraphControllerActor`, spawned once for the whole test session, against
//! whatever single Neo4j instance the environment points at. Isolation
//! between scenarios comes from `cluster_uid` instead -- every node in
//! this schema already carries it (issue #236), so giving each scenario
//! its own `cluster_uid` means scenarios sharing one database can't see or
//! collide with each other's data, with no schema changes needed to get
//! there, and tests can run concurrently against the one shared instance.
//!
//! This is a different isolation strategy than `polar`'s own
//! `graph::controller::tests` (full `DETACH DELETE` wipe + forced
//! sequential execution). That approach is simpler when it applies, but
//! doesn't scale as well to a much larger scenario count than that file's
//! ~7 tests, which is what Tier 1 is expected to grow into. If consistency
//! with the existing pattern matters more than concurrency here, this is
//! the one piece of this design to swap -- everything else is orthogonal
//! to which isolation strategy is used.
//!
//! # Reading graph state back
//!
//! `GraphControllerMsg` is write-only -- no read path, even in
//! production. Rather than adding one, each `Scenario` carries a second,
//! direct `neo4rs::Graph` connection (`read_graph`) built from the exact
//! same `GraphControllerActor::get_neo_config()` the actor itself uses, so
//! it's guaranteed to target the same instance. Assertions are canned,
//! hand-written Cypher against that connection, using `Graph::execute`
//! directly -- confirmed working for reads by `polar`'s own
//! `graph::controller::tests`, which resolves the uncertainty
//! `GraphControllerActor::post_start`'s own comment raised about whether
//! `.execute()` is safe to rely on on this pinned neo4rs fork.

// Sibling module now, not a separate crate -- fixtures.rs and scenario.rs
// both live under kube_common::testing. If testing/mod.rs re-exports
// fixtures' contents flat (`pub use fixtures::*;`) rather than as a plain
// `pub mod fixtures;`, this needs to become `use super::{FixtureError,
// parse_fixture};` instead -- I don't know which shape mod.rs actually
// uses, so this assumes the more common default (plain `pub mod`, no
// flattening).
use super::fixtures::{FixtureError, parse_fixture};
use polar::cassini::MockCassiniClient;
use polar::graph::controller::{GraphController, GraphControllerMsg};
use ractor::ActorProcessingErr;
use serde::de::DeserializeOwned;
use std::time::Duration;
use testcontainers::core::{ContainerPort, IntoContainerPort};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::neo4j::{Neo4j, Neo4jImage};
use uuid::Uuid;

/// How long a scenario is willing to wait for the Sync barrier to answer.
/// Generous on purpose -- this is a correctness barrier, not a performance
/// test; a slow CI runner should not turn into a false failure.
const SYNC_TIMEOUT: Duration = Duration::from_secs(10);

/// A running scenario: a `GraphController` handle for writes, a second,
/// direct `neo4rs::Graph` connection for reads (per your call -- canned,
/// hand-written queries against a plain connection, not routed through the
/// actor), a `cluster_uid` scoping every write and every assertion this
/// scenario makes, and the mock client every `project_into_graph` call is
/// threaded through.
pub struct Scenario {
    pub graph: GraphController,
    pub read_graph: neo4rs::Graph,
    pub cluster_uid: String,
    pub cassini: MockCassiniClient,
}

impl Scenario {
    /// Starts a new scenario against an already-running `GraphController`
    /// and a read connection pointed at the same instance (see the module
    /// doc -- bootstrapping both is a separate seam). Each call gets a
    /// fresh, random `cluster_uid`, so scenarios never need to coordinate
    /// names with each other even when run concurrently against the same
    /// shared database.
    pub fn new(graph: GraphController, read_graph: neo4rs::Graph) -> Self {
        Self {
            graph,
            read_graph,
            cluster_uid: format!("test-{}", Uuid::new_v4()),
            cassini: MockCassiniClient::new(),
        }
    }

    /// Same as `new`, but with a deterministic `cluster_uid` -- for a
    /// scenario whose assertions want to assert on the exact string
    /// (rare), or whose failure output should be easy to grep for by name
    /// rather than a fresh UUID every run.
    pub fn named(graph: GraphController, read_graph: neo4rs::Graph, name: &str) -> Self {
        Self {
            graph,
            read_graph,
            cluster_uid: format!("test-{name}"),
            cassini: MockCassiniClient::new(),
        }
    }

    /// Parses `yaml` as `T` and returns it -- does not project it. Split
    /// from `apply` so a scenario can inspect/mutate a fixture (e.g.
    /// stamping in this scenario's own `cluster_uid` if a resource's UID
    /// needs to be unique per run) before it's written.
    pub fn load<T: DeserializeOwned>(&self, yaml: &str) -> Result<T, FixtureError> {
        parse_fixture(yaml)
    }

    /// Waits for every operation sent to `graph` before this call to be
    /// fully written (or failed) to Neo4j. Call this after every
    /// `project_into_graph`/`project_delete` and before any assertion --
    /// skipping it is exactly the race issue #221 diagnosed as the
    /// original harness attempts' recurring failure.
    ///
    /// Matches on ractor's `CallResult` variants explicitly rather than
    /// calling a convenience method -- I'm confident in `Success`/
    /// `Timeout` existing (standard ractor RPC shape used throughout this
    /// codebase already), less confident a `success_or`-style helper
    /// exists with that exact name on this ractor version, so this uses
    /// only the parts I've actually seen work elsewhere.
    pub async fn sync(&self) -> Result<(), ActorProcessingErr> {
        match self
            .graph
            .call(GraphControllerMsg::Sync, Some(SYNC_TIMEOUT))
            .await
        {
            Ok(ractor::rpc::CallResult::Success(())) => Ok(()),
            Ok(ractor::rpc::CallResult::Timeout) => Err(ActorProcessingErr::from(
                "Sync call timed out -- GraphControllerActor did not answer within SYNC_TIMEOUT",
            )),
            Ok(other) => Err(ActorProcessingErr::from(format!(
                "Sync call did not succeed: {other:?}"
            ))),
            Err(e) => Err(ActorProcessingErr::from(format!(
                "Sync call failed to send: {e}"
            ))),
        }
    }

    // ---- Reading graph state back for assertions ----
    //
    // No wrapper methods here on purpose. Per your call: a second, direct
    // neo4rs::Graph connection (see `read_graph` on this struct), and
    // canned, hand-written Cypher per assertion -- not a generic query/
    // count abstraction invented from unverified guesses about this fork's
    // row-reading API. Use `self.read_graph` directly.
}

/// Holds the container alive for the test session -- never touched again
/// after construction, same reasoning as `polar`'s own
/// `graph::controller::tests::Neo4jFixture`; see my earlier note on that
/// file for the caveat on what this `unsafe impl Sync` actually covers.
struct Neo4jTestContainer {
    _container: ContainerAsync<Neo4jImage>,
}

unsafe impl Sync for Neo4jTestContainer {}

static NEO4J_CONTAINER: tokio::sync::OnceCell<Neo4jTestContainer> =
    tokio::sync::OnceCell::const_new();

/// Starts (once, memoized for the whole test binary) a Neo4j container
/// *configured to match* whatever `GRAPH_ENDPOINT`/`GRAPH_USER`/
/// `GRAPH_PASSWORD`/`GRAPH_DB` are already set to -- the reverse of
/// starting a container with its own defaults and trying to propagate
/// them out via `std::env::set_var` (an `unsafe fn` as of recent Rust; not
/// worth doing inside library code every scenario depends on). By the
/// time this returns, those env vars were already correct -- this makes
/// the container true, rather than making the env vars true.
///
/// Requires `GRAPH_ENDPOINT` to specify a *fixed* port (e.g.
/// `bolt://localhost:7687`), not one left for testcontainers to assign
/// dynamically -- this binds the container to exactly that port.
/// `GRAPH_DB` must be `"neo4j"`: Community Edition has exactly one
/// standard database and it's always named that (see the module doc's
/// Isolation section) -- nothing to configure there, only to confirm.
///
/// `NEO4J_AUTH=user/password` is Neo4j's own image convention for setting
/// initial credentials, passed straight through via `.with_env_var`.
///
/// Confidence check: `.with_env_var`/`.with_mapped_port` and the
/// `ImageExt` trait they come from are my best understanding of current
/// `testcontainers-rs`, not verified against this workspace's exact pinned
/// version the way the rest of this design is -- `tests.rs` only
/// demonstrated `Neo4j::default()` with no customization, so this
/// specific configuration path (reading env first, configuring the
/// container to match) has no directly-confirmed precedent in this
/// codebase yet. If the method names or argument order are off, this is
/// the block to check first.
pub async fn bootstrap_container() -> Result<(), ActorProcessingErr> {
    NEO4J_CONTAINER
        .get_or_try_init(|| async {
            let endpoint = std::env::var("GRAPH_ENDPOINT")
                .map_err(|_| ActorProcessingErr::from("GRAPH_ENDPOINT not set"))?;
            let user = std::env::var("GRAPH_USER")
                .map_err(|_| ActorProcessingErr::from("GRAPH_USER not set"))?;
            let password = std::env::var("GRAPH_PASSWORD")
                .map_err(|_| ActorProcessingErr::from("GRAPH_PASSWORD not set"))?;
            let db = std::env::var("GRAPH_DB")
                .map_err(|_| ActorProcessingErr::from("GRAPH_DB not set"))?;

            if db != "neo4j" {
                return Err(ActorProcessingErr::from(format!(
                    "GRAPH_DB={db}, but Neo4j Community Edition has exactly one \
                     standard database and it's always named \"neo4j\""
                )));
            }

            let port: u16 = endpoint
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .ok_or_else(|| {
                    ActorProcessingErr::from(format!(
                        "GRAPH_ENDPOINT={endpoint} doesn't end in a parseable port -- \
                         a fixed port is required so the container has something \
                         known to bind to"
                    ))
                })?;

            let container = Neo4j::default()
                .with_env_var("NEO4J_AUTH", format!("{user}/{password}"))
                .with_mapped_port(port, 7687.tcp())
                .start()
                .await
                .map_err(|e| {
                    ActorProcessingErr::from(format!("failed to start Neo4j test container: {e}"))
                })?;

            Ok(Neo4jTestContainer {
                _container: container,
            })
        })
        .await?;

    Ok(())
}

/// Spawns the shared `GraphControllerActor` and opens the second,
/// read-only `neo4rs::Graph` connection, both against whatever
/// `GRAPH_*`/`TLS_*` env vars are already set. Call after
/// `bootstrap_container()` -- this doesn't start anything itself, it just
/// connects to what should already be listening.
///
/// Both connections are built from `GraphControllerActor::get_neo_config`
/// directly -- the same function `pre_start` itself calls in production --
/// rather than a second, hand-rolled copy of the env-var-reading logic
/// that could silently drift from it. `Actor::spawn`, not `spawn_linked`:
/// a test harness has no natural supervisor to link to, and doesn't want
/// this actor's failure to take down whatever's running the test process.
pub async fn bootstrap_connections() -> Result<(GraphController, neo4rs::Graph), ActorProcessingErr>
{
    use polar::graph::controller::{GraphControllerActor, GraphControllerArgs, GraphSignal};

    let read_conf = GraphControllerActor::get_neo_config()?;
    let read_graph = neo4rs::Graph::connect(read_conf)
        .map_err(|e| ActorProcessingErr::from(format!("read connection failed: {e}")))?;

    // Thrown away -- a test harness has no reason to subscribe to
    // connectivity signals the way production supervisors do.
    let signal_port = std::sync::Arc::new(ractor::OutputPort::<GraphSignal>::default());

    let (graph, _join_handle) = ractor::Actor::spawn(
        None,
        GraphControllerActor,
        GraphControllerArgs { signal_port },
    )
    .await?;

    Ok((graph, read_graph))
}
