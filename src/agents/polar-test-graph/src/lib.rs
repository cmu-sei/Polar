/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! A shared, per-process Neo4j test fixture -- one real testcontainers
//! Neo4j instance per test-binary process (not per-#[tokio::test]
//! function), genuinely fixing the cross-runtime bug the original,
//! per-crate `NEO4J_FIXTURE: tokio::sync::OnceCell` pattern had: each
//! `#[tokio::test]` function gets its own, separate Tokio runtime by
//! default, but the shared fixture's own async resources (the
//! neo4rs::Graph connection, the testcontainers container handle's
//! own background cleanup task) got bound to whichever test's runtime
//! happened to initialize the fixture first. Once that first test's
//! runtime shut down, later tests reusing the cached fixture from a
//! different runtime hit "A Tokio 1.x context was found, but it is
//! being shutdown."
//!
//! The fix: a single, dedicated background thread with its own,
//! long-lived Tokio runtime, created once via a plain
//! `std::sync::OnceLock` (not tokio::sync::OnceCell -- this fixture
//! is no longer tied to any individual test's own runtime at all).
//! The container and connection are created on that runtime; the
//! background thread then simply keeps that same runtime alive for
//! the rest of the process's own lifetime, so the runtime's own
//! worker threads -- and whatever background tasks neo4rs/
//! testcontainers spawned on them -- stay genuinely alive and
//! scheduled regardless of which thread later calls into the graph.
//!
//! This also means cleanup now works correctly, unlike the original
//! bug: testcontainers's own Drop impl spawns a real async task
//! (`client.rm(&id).await`) to actually remove the container, which
//! needs an active Tokio runtime to run. Because our background
//! runtime is still genuinely alive at normal process exit (it's
//! only ever dropped when the 'static OnceLock itself drops, which
//! happens during process teardown, before the runtime's own worker
//! threads are torn down), that cleanup task now has somewhere real
//! to run -- unlike the original design, where the fixture only ever
//! dropped after every runtime, including its own, was already gone.
//!
//! Scope, stated plainly rather than overclaimed: this is one
//! container per test-binary *process* (`cargo test` spawns one
//! separate process per crate), not one container shared across an
//! entire, multi-crate `cargo test` invocation. A single container
//! across the whole invocation would need real cross-process
//! coordination (a well-known port or lock file, plus a reliable way
//! for whichever process happens to finish last to own teardown),
//! which isn't feasible to make airtight without controlling how
//! `cargo test` itself gets invoked -- something this crate
//! deliberately doesn't assume.

use neo4rs::Graph;
use std::sync::{Mutex, OnceLock};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::neo4j::{Neo4j, Neo4jImage};

/// The running container's own ID, readable by the raw
/// `extern "C" fn()` atexit callback below -- that callback can't
/// capture any closure state at all, so this is the only way for it
/// to know which real container to remove.
static CONTAINER_ID: Mutex<Option<String>> = Mutex::new(None);

/// Registered via `libc::atexit` -- the one mechanism that genuinely
/// survives `cargo test`'s own harness calling `std::process::exit()`
/// at the end of a run. `process::exit` is documented to deliberately
/// skip Rust's own static/global destructors (including our
/// `Neo4jFixture`'s own Drop), but still goes through libc-level
/// atexit hooks registered this way.
///
/// Deliberately synchronous and libc-safe, not async/Tokio-dependent
/// -- Rust gives no safety guarantees about what's available at this
/// point in the process lifecycle (see this crate's own commit
/// history for the discussion), so this runs a plain, blocking
/// `std::process::Command` rather than attempting any real async
/// cleanup through neo4rs/testcontainers's own, Tokio-dependent Drop
/// path at all.
extern "C" fn cleanup_neo4j_container() {
    let id = match CONTAINER_ID.lock() {
        Ok(mut guard) => guard.take(),
        Err(_) => None,
    };
    let Some(id) = id else {
        return;
    };

    // Try podman first (this environment's own, actual container
    // runtime), then docker -- we can't be sure which CLI is
    // genuinely available on any given machine this runs on.
    for cli in ["podman", "docker"] {
        if std::process::Command::new(cli)
            .args(["rm", "-f", &id])
            .status()
            .is_ok_and(|status| status.success())
        {
            break;
        }
    }
}

struct Neo4jFixture {
    graph: Graph,
    /// Held only to keep the container alive and to run its own,
    /// real async Drop cleanup when this fixture itself drops (at
    /// process exit) -- never read directly by callers.
    _container: ContainerAsync<Neo4jImage>,
    /// The dedicated background thread's own JoinHandle, and the
    /// Runtime it owns -- both held here purely to keep the runtime
    /// (and, through it, the container's own background tasks)
    /// genuinely alive for the rest of this process's lifetime. The
    /// thread itself simply parks forever, via
    /// `runtime.block_on(pending())`, until the process exits.
    _background_thread: std::thread::JoinHandle<()>,
}

// Safety: ContainerAsync is not Sync by default, but nothing here
// ever accesses it concurrently -- it's held only to prevent Drop
// firing early. The graph field is the only thing callers actually
// interact with, and neo4rs::Graph is genuinely Send + Sync on its
// own.
unsafe impl Sync for Neo4jFixture {}

static NEO4J_FIXTURE: OnceLock<Neo4jFixture> = OnceLock::new();
static NEO4J_RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// The shared, per-process Neo4j graph handle -- genuinely
/// synchronous to call (unlike the original, per-crate fixture this
/// replaces), since all the real async setup work already happened
/// on the dedicated background runtime the first time this was
/// called.
pub fn neo4j() -> &'static Graph {
    &NEO4J_FIXTURE
        .get_or_init(|| {
            // The entire runtime creation and setup below must
            // genuinely happen inside the spawned background thread
            // itself, never on the calling thread -- the first
            // caller here is always some #[tokio::test] function,
            // which is already running inside its own, active Tokio
            // runtime. Tokio's own Runtime::block_on explicitly,
            // deliberately panics ("cannot start a runtime from
            // within a runtime") if called from a thread that's
            // already driving one. A plain std::sync::mpsc channel
            // (an ordinary, non-Tokio blocking operation, safe to
            // call from any context) is how the calling thread
            // retrieves the real result once the background thread's
            // own setup genuinely completes.
            let (tx, rx) = std::sync::mpsc::channel();

            let background_thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build the shared, per-process Neo4j test runtime");

                let _ = NEO4J_RUNTIME_HANDLE.set(runtime.handle().clone());

                runtime.block_on(async move {
                    let _ = rustls::crypto::ring::default_provider().install_default();
                    polar::init_logging("polar.test-graph".to_string());

                    let container = Neo4j::default()
                        .start()
                        .await
                        .expect("failed to start Neo4j test container");

                    if let Ok(mut guard) = CONTAINER_ID.lock() {
                        *guard = Some(container.id().to_string());
                    }
                    // Registered once, right after the container's
                    // own ID is genuinely available -- see
                    // cleanup_neo4j_container's own doc comment for
                    // why this, rather than Drop, is what actually
                    // runs reliably at process exit.
                    unsafe {
                        libc::atexit(cleanup_neo4j_container);
                    }

                    let config = neo4rs::ConfigBuilder::new()
                        .uri(format!(
                            "bolt://{}:{}",
                            container.get_host().await.expect("container host"),
                            container.image().bolt_port_ipv4().expect("bolt port"),
                        ))
                        .user(container.image().user().expect("user"))
                        .password(container.image().password().expect("password"))
                        .build()
                        .expect("neo4rs config");

                    let graph = Graph::connect(config).expect("neo4rs connect");

                    // Hand the real result back to whichever test
                    // thread is waiting on rx.recv() below, then keep
                    // this same runtime -- and, through it, the
                    // container's and neo4rs's own background tasks
                    // -- genuinely alive by blocking on a future that
                    // never resolves, for the rest of this process's
                    // lifetime.
                    let _ = tx.send((container, graph));
                    std::future::pending::<()>().await;
                });
            });

            let (container, graph) = rx
                .recv()
                .expect("the background Neo4j setup thread panicked or dropped its sender");

            Neo4jFixture {
                graph,
                _container: container,
                _background_thread: background_thread,
            }
        })
        .graph
}

/// Enters the shared, dedicated background runtime's own context for
/// the duration of the returned guard's own scope -- callers should
/// hold this for their entire test function body (e.g.
/// `let _guard = enter();` as the first line), so that anything
/// neo4rs's own connection pool lazily spawns during query execution
/// (not just at the original `Graph::connect()` call) binds to our
/// shared runtime specifically, rather than to whichever runtime
/// happens to be ambiently current at the call site (each
/// #[tokio::test] function's own, separate one). Nesting guards this
/// way is explicitly supported by tokio's own Handle::enter -- as
/// long as this guard drops before the calling test's own, outer
/// #[tokio::test] guard does (which Rust's ordinary, scope-based Drop
/// order already guarantees here, since this is always the innermost
/// guard), no panic results.
///
/// Must be called after neo4j() has been called at least once (so
/// the shared runtime genuinely exists to enter) -- panics otherwise.
pub fn enter() -> tokio::runtime::EnterGuard<'static> {
    NEO4J_RUNTIME_HANDLE
        .get()
        .expect("enter() called before neo4j() -- call neo4j() first to initialize the shared runtime")
        .enter()
}

/// Clears every node and relationship from the shared graph --
/// callers should call this at the start of any test that needs a
/// genuinely empty graph, since the same, real Neo4j instance is
/// reused across every test in this process.
pub async fn clear_graph(graph: &Graph) {
    graph
        .run(neo4rs::Query::new("MATCH (n) DETACH DELETE n".to_string()))
        .await
        .expect("failed to clear graph between tests");
}
