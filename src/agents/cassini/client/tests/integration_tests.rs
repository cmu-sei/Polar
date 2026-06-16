//! Integration test for `cassini-client --daemon` (background mode).
//!
//! This directly covers the bug: `--daemon` without `--foreground` used to
//! re-exec a child with an argv that failed clap validation (`--foreground`
//! requires `--daemon`, and `--daemon` had been stripped), silently exit
//! inside `Stdio::null()`, while the parent reported success regardless.
//!
//! Assumptions — adjust if these don't match the actual workspace layout:
//!   - The binary target is named `cassini-client`
//!     (`env!("CARGO_BIN_EXE_cassini-client")`).
//!   - The library crate is `cassini_client` (per the `use` statements in
//!     `main.rs`), and `cli::{daemon_is_reachable, ipc_read_response,
//!     IpcRequest, IpcResponse}` are reachable from `cassini_client::cli`.
//!   - `tempfile` is available as a dev-dependency.
//!   - Unix-only: the IPC transport is a Unix domain socket and shutdown is
//!     verified via SIGTERM, neither of which exist on Windows.

use cassini_client::cli::{IpcRequest, IpcResponse, daemon_is_reachable, ipc_read_response};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll until `path` exists, or fail with a message that includes the daemon
/// log so a CI failure is actually debuggable. No fixed sleeps — bind time
/// depends on broker-connect attempts and machine load.
async fn wait_for_exists(path: &Path, what: &str, log_path: &Path) {
    let start = std::time::Instant::now();
    while !path.exists() {
        if start.elapsed() > POLL_TIMEOUT {
            panic!(
                "timed out after {POLL_TIMEOUT:?} waiting for {what} at {}\n--- daemon log ({}) ---\n{}",
                path.display(),
                log_path.display(),
                read_log(log_path),
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll until `path` no longer exists (used to confirm SIGTERM cleanup).
async fn wait_for_removed(path: &Path, what: &str) {
    let start = std::time::Instant::now();
    while path.exists() {
        if start.elapsed() > POLL_TIMEOUT {
            panic!(
                "timed out after {POLL_TIMEOUT:?} waiting for {what} at {} to be removed",
                path.display()
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| format!("<could not read log file: {e}>"))
}

#[tokio::test]
async fn daemon_background_start_status_and_sigterm() {
    let tmp = tempfile::tempdir().expect("create tempdir for daemon socket");
    let socket_path = tmp.path().join("daemon.sock");
    let pid_path = socket_path.with_extension("pid");
    let log_path = socket_path.with_extension("log");

    // This is the exact invocation the bug report says is silently a no-op:
    // `--daemon` without `--foreground`.
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_cassini-client"))
        .arg("--daemon")
        .arg("--socket")
        .arg(&socket_path)
        .status()
        .expect("failed to spawn `cassini-client --daemon`");

    assert!(
        status.success(),
        "`cassini-client --daemon` exited with {status:?} (expected the parent to return Ok \
         immediately after spawning the detached daemon)"
    );

    // The parent returns almost immediately; the real daemon is a detached
    // grandchild. Wait for it to bind its socket and write its pidfile.
    wait_for_exists(&socket_path, "daemon socket", &log_path).await;
    wait_for_exists(&pid_path, "daemon pidfile", &log_path).await;

    assert!(
        daemon_is_reachable(&socket_path).await,
        "socket file exists but the daemon is not accepting connections\n\
         --- daemon log ({}) ---\n{}",
        log_path.display(),
        read_log(&log_path),
    );

    // Send a Status IPC request and confirm a well-formed Ok response.
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to daemon IPC socket");

    let request_bytes = serde_json::to_vec(&IpcRequest::Status).expect("serialize Status request");
    stream
        .write_all(&(request_bytes.len() as u32).to_be_bytes())
        .await
        .expect("write request length prefix");
    stream
        .write_all(&request_bytes)
        .await
        .expect("write request body");

    match ipc_read_response(&mut stream)
        .await
        .expect("read IPC response from daemon")
    {
        IpcResponse::Ok { result } => {
            let result = result.expect("Status response should carry a result payload");
            assert!(
                result.get("pid").is_some(),
                "Status response missing `pid` field: {result:?}"
            );
        }
        IpcResponse::Error { reason } => panic!("daemon returned an error for Status: {reason}"),
    }

    drop(stream);

    // SIGTERM the daemon (pid comes from the pidfile, since the spawned
    // parent process is not the daemon itself) and confirm cleanup.
    let pid = std::fs::read_to_string(&pid_path)
        .expect("read daemon pidfile")
        .trim()
        .to_string();

    let kill_status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(&pid)
        .status()
        .expect("invoke `kill`");
    assert!(
        kill_status.success(),
        "failed to send SIGTERM to daemon pid {pid}"
    );

    wait_for_removed(&socket_path, "daemon socket").await;
    wait_for_removed(&pid_path, "daemon pidfile").await;
}
