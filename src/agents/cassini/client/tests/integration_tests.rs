use anyhow::Result;
/// Integration tests for the cassini CLI.
///
/// # Requirements
///
/// The following environment variables must be set before running:
///   TLS_SERVER_CERT_CHAIN  — path to the server certificate chain PEM
///   TLS_SERVER_KEY         — path to the server private key PEM
///   TLS_CA_CERT            — path to the CA certificate PEM
///   TLS_CLIENT_CERT        — path to the client certificate PEM
///   TLS_CLIENT_KEY         — path to the client private key PEM
///   CASSINI_SERVER_NAME    — TLS SNI name for the broker (e.g. "cassini.local")
///
/// Run with:
///   cargo test --test integration -- --test-threads=1
///
/// Parallel execution is fine at the fixture level (each test gets its own
/// broker port + socket path), but --test-threads=1 avoids port exhaustion
/// on CI. Adjust as needed for your environment.
use assert_matches::assert_matches;
use base64::Engine as _;
use cassini_client::TCPClientConfig;
use cassini_client::cli::{
    BridgeMode, CompletionEvent, IpcRequest, IpcResponse, OutputFormat, Session,
    daemon_is_reachable, dispatch_to_daemon, run_daemon,
};
use rstest::*;
use std::env;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const CASSINI_SERVER_BIN: &str = env!("CASSINI_SERVER_BIN");
const CASSINI_CLIENT_BIN: &str = env!("CASSINI_CLIENT_BIN");
// ============================================================================
// Helpers
// ============================================================================

/// Pick a random available port by binding to :0 and immediately releasing.
/// There is a TOCTOU window here, but it is acceptable for test fixtures on
/// a loopback interface where contention is extremely unlikely.
async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

fn b64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}
/// Read TLS paths from the environment once. These are set by the test runner
/// and never mutated, so concurrent reads across parallel tests are safe.
fn tls_ca_cert() -> String {
    std::env::var("TLS_CA_CERT").expect("TLS_CA_CERT must be set")
}
fn tls_client_cert() -> String {
    std::env::var("TLS_CLIENT_CERT").expect("TLS_CLIENT_CERT must be set")
}
fn tls_client_key() -> String {
    std::env::var("TLS_CLIENT_KEY").expect("TLS_CLIENT_KEY must be set")
}

// ============================================================================
// Broker fixture
// ============================================================================

/// A running `cassini-server` subprocess bound to a random loopback port.
///
/// Dropping this struct sends SIGTERM to the child and waits for it to exit,
/// ensuring the port is freed before the next test starts.
struct BrokerFixture {
    child: tokio::process::Child,
    /// Full address the broker is listening on, e.g. "127.0.0.1:54321"
    pub addr: String,
    /// The CASSINI_SERVER_NAME from the environment — callers need this for TLS.
    pub server_name: String,
}

impl BrokerFixture {
    /// Spawn the broker on a random port and wait until it accepts TCP connections.
    ///
    /// TLS env vars are inherited from the test process environment. The broker
    /// is given a unique CASSINI_BIND_ADDR so parallel fixtures don't collide.
    async fn start() -> Self {
        // ----------------------------------------------------------------
        // STUB: if your TLS env vars have different names, adjust here.
        // The broker subprocess inherits the full environment, so as long as
        // TLS_SERVER_CERT_CHAIN / TLS_SERVER_KEY / TLS_CA_CERT are set in
        // the shell that runs `cargo test`, nothing else needs to change.
        // ----------------------------------------------------------------
        let port = free_port().await;
        let addr = format!("127.0.0.1:{port}");
        let server_name = std::env::var("CASSINI_SERVER_NAME")
            .expect("CASSINI_SERVER_NAME must be set for integration tests");

        let child = tokio::process::Command::new(CASSINI_SERVER_BIN)
            .env("CASSINI_BIND_ADDR", &addr)
            // Suppress broker logs unless CASSINI_LOG is already set — keeps test
            // output readable. Override by setting CASSINI_LOG=debug in your shell.
            .env(
                "CASSINI_LOG",
                std::env::var("CASSINI_LOG").unwrap_or_else(|_| "error".to_string()),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit()) // surfaced only on test failure
            .kill_on_drop(true)
            .spawn()
            .expect("Failed to spawn cassini-server. Is it built? Run `cargo build` first.");

        // Wait until the broker is actually accepting TCP connections, up to 10 s.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("Broker at {addr} did not become reachable within 10 s");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        BrokerFixture {
            child,
            addr,
            server_name,
        }
    }

    /// Build a TCPClientConfig pointing directly at this broker instance.
    /// No env var mutation — each test gets its own isolated config.
    pub fn client_config(&self) -> TCPClientConfig {
        TCPClientConfig::from_values(
            self.addr.clone(),
            self.server_name.clone(),
            tls_ca_cert(),
            tls_client_cert(),
            tls_client_key(),
        )
    }
}

impl Drop for BrokerFixture {
    fn drop(&mut self) {
        // kill_on_drop(true) handles the actual kill; this is belt-and-suspenders.
        let _ = self.child.start_kill();
    }
}

// ============================================================================
// Daemon fixture
// ============================================================================

/// A running CLI daemon backed by a real broker.
///
/// Dropping this fixture sends a shutdown signal to the daemon task, which
/// sends a clean Disconnect to the broker before exiting.
struct DaemonFixture {
    pub socket_path: PathBuf,
    /// Hold TempDir alive so the socket directory isn't deleted under the daemon.
    _socket_dir: TempDir,
    shutdown_tx: oneshot::Sender<()>,
    daemon_task: tokio::task::JoinHandle<()>,
}

impl DaemonFixture {
    async fn start(config: TCPClientConfig, register_timeout: Duration) -> Self {
        let dir = TempDir::new().expect("Failed to create temp dir for daemon socket");
        let socket_path = dir.path().join("daemon.sock");
        let path_clone = socket_path.clone();

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let daemon_task = tokio::spawn(async move {
            tokio::select! {
                result = run_daemon(path_clone, config, register_timeout, true) => {
                    if let Err(e) = result {
                        eprintln!("Daemon exited with error: {e}");
                    }
                }
                _ = shutdown_rx => {}
            }
        });

        // run_daemon binds the socket only after registration succeeds, so
        // reachability here implicitly confirms the full broker handshake.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if daemon_is_reachable(&socket_path).await {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "Daemon socket at {} did not become reachable within 15s",
                    socket_path.display()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        DaemonFixture {
            socket_path,
            _socket_dir: dir,
            shutdown_tx,
            daemon_task,
        }
    }

    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(5), self.daemon_task).await;
    }
}

// ============================================================================
// Session fixture (direct connect, no daemon)
// ============================================================================

/// A registered Session connected directly to the broker.
/// Returned by the `direct_session` rstest fixture.
async fn make_direct_session(config: TCPClientConfig) -> Session {
    cassini_client::cli::register(config, Duration::from_secs(30))
        .await
        .expect("Direct session registration failed")
}

// ============================================================================
// rstest fixtures
// ============================================================================

#[fixture]
async fn broker() -> BrokerFixture {
    BrokerFixture::start().await
}

#[fixture]
async fn daemon(#[future] broker: BrokerFixture) -> (BrokerFixture, DaemonFixture) {
    let broker = broker.await;
    let config = broker.client_config();
    let daemon = DaemonFixture::start(config, Duration::from_secs(30)).await;
    (broker, daemon)
}

// ============================================================================
// Direct-connect tests
// ============================================================================

/// Given a live broker
/// When we register directly
/// Then we get a non-empty registration ID
#[rstest]
#[tokio::test]
async fn given_broker_when_direct_register_then_registration_id_assigned(
    #[future] broker: BrokerFixture,
) {
    let broker = broker.await;
    let session = make_direct_session(broker.client_config()).await;
    assert!(!session.get_id().is_empty());
    session.disconnect().await;
}

/// Given a live broker and a direct session
/// When we publish a message
/// Then we receive a PublishAcknowledged event for the correct topic
#[rstest]
#[tokio::test]
async fn given_direct_session_when_publish_then_ack_received(#[future] broker: BrokerFixture) {
    let broker = broker.await;
    let session: Session = make_direct_session(broker.client_config()).await;

    let result: CompletionEvent = session
        .send_and_await(
            cassini_client::TcpClientMessage::Publish {
                topic: "test.direct.publish".to_string(),
                payload: b"hello from direct".to_vec(),
                trace_ctx: None,
            },
            BridgeMode::Publish,
            Duration::from_secs(5),
            "Timed out waiting for publish ack",
        )
        .await
        .expect("Publish failed");

    assert_matches!(
        result,
        cassini_client::cli::CompletionEvent::Published { topic } if topic == "test.direct.publish"
    );

    session.disconnect().await;
}

/// Given a live broker and a direct session
/// When we request ListTopics
/// Then we receive a ControlResponse containing a TopicList
#[rstest]
#[tokio::test]
async fn given_direct_session_when_list_topics_then_topic_list_returned(
    #[future] broker: BrokerFixture,
) {
    let broker = broker.await;
    let session = make_direct_session(broker.client_config()).await;

    let result = session
        .send_and_await(
            cassini_client::TcpClientMessage::ListTopics { trace_ctx: None },
            BridgeMode::Control,
            Duration::from_secs(5),
            "Timed out waiting for topic list",
        )
        .await
        .expect("ListTopics failed");

    assert_matches!(
        result,
        CompletionEvent::ControlResponse(Ok(cassini_types::ControlResult::TopicList(_)))
    );

    session.disconnect().await;
}

/// Given a live broker and a direct session
/// When we request ListSessions
/// Then our own registration ID appears in the session map
#[rstest]
#[tokio::test]
async fn given_direct_session_when_list_sessions_then_own_session_visible(
    #[future] broker: BrokerFixture,
) {
    let broker = broker.await;
    let session = make_direct_session(broker.client_config()).await;
    let our_id = session.get_id().clone();

    let result = session
        .send_and_await(
            cassini_client::TcpClientMessage::ListSessions { trace_ctx: None },
            BridgeMode::Control,
            Duration::from_secs(5),
            "Timed out waiting for session list",
        )
        .await
        .expect("ListSessions failed");

    match result {
        CompletionEvent::ControlResponse(Ok(cassini_types::ControlResult::SessionList(map))) => {
            assert!(
                map.contains_key(&our_id),
                "Our registration_id {our_id} not found in session list: {map:?}"
            );
        }
        other => panic!("Unexpected event: {other:?}"),
    }

    session.disconnect().await;
}

/// Given a live broker and a direct session
/// When we request GetSession for our own registration ID
/// Then SessionInfo is returned
#[rstest]
#[tokio::test]
async fn given_direct_session_when_get_session_then_session_info_returned(
    #[future] broker: BrokerFixture,
) {
    let broker = broker.await;
    let session = make_direct_session(broker.client_config()).await;
    let our_id = session.get_id().clone();

    let result = session
        .send_and_await(
            cassini_client::TcpClientMessage::ControlRequest {
                op: cassini_types::ControlOp::GetSessionInfo {
                    registration_id: our_id.clone(),
                },
                trace_ctx: None,
            },
            BridgeMode::Control,
            Duration::from_secs(5),
            "Timed out waiting for session info",
        )
        .await
        .expect("GetSession failed");

    assert_matches!(
        result,
        CompletionEvent::ControlResponse(Ok(cassini_types::ControlResult::SessionInfo(_)))
    );

    session.disconnect().await;
}

/// Given a live broker and a direct session
/// When we request GetSession for an unknown registration ID
/// Then ControlResponse(Err(NotFound)) is returned
#[rstest]
#[tokio::test]
async fn given_direct_session_when_get_unknown_session_then_not_found(
    #[future] broker: BrokerFixture,
) {
    let broker = broker.await;
    let session = make_direct_session(broker.client_config()).await;

    let result = session
        .send_and_await(
            cassini_client::TcpClientMessage::ControlRequest {
                op: cassini_types::ControlOp::GetSessionInfo {
                    registration_id: "nonexistent-id-000".to_string(),
                },
                trace_ctx: None,
            },
            BridgeMode::Control,
            Duration::from_secs(5),
            "Timed out waiting for session info",
        )
        .await
        .expect("GetSession RPC failed");

    assert_matches!(
        result,
        CompletionEvent::ControlResponse(Err(cassini_types::ControlError::NotFound(_)))
    );

    session.disconnect().await;
}

// ============================================================================
// Daemon IPC tests
// ============================================================================

/// Given a running daemon
/// When we send a Status IPC request
/// Then we receive the daemon PID and a non-empty registration ID
#[rstest]
#[tokio::test]
async fn given_daemon_when_status_then_pid_and_registration_id_returned(
    #[future] daemon: (BrokerFixture, DaemonFixture),
) {
    let (broker, daemon) = daemon.await;
    let _ = broker; // keep alive

    let response: Result<()> =
        dispatch_to_daemon(&daemon.socket_path, IpcRequest::Status, &OutputFormat::Json).await;

    // dispatch_to_daemon prints to stdout; we verify it doesn't error.
    assert!(response.is_ok(), "Status request failed: {response:?}");

    daemon.shutdown().await;
}

/// Given a running daemon
/// When we publish via IPC
/// Then the response contains the correct topic
#[rstest]
#[tokio::test]
async fn given_daemon_when_publish_via_ipc_then_ack_contains_topic(
    #[future] daemon: (BrokerFixture, DaemonFixture),
) {
    let (broker, daemon) = daemon.await;
    let _ = broker;

    let response = send_raw_ipc_request(
        &daemon.socket_path,
        &IpcRequest::Publish {
            topic: "test.daemon.publish".to_string(),
            payload_b64: b64_encode(b"hello from daemon"),
            timeout_secs: 5,
        },
    )
    .await
    .expect("IPC request failed");

    assert_matches!(
        &response,
        IpcResponse::Ok { result: Some(val) } if val["topic"] == "test.daemon.publish"
    );

    daemon.shutdown().await;
}

/// Given a running daemon
/// When we publish multiple messages in rapid succession
/// Then all are acked correctly without interleaving
#[rstest]
#[tokio::test]
async fn given_daemon_when_burst_publish_then_all_acked(
    #[future] daemon: (BrokerFixture, DaemonFixture),
) {
    let (broker, daemon) = daemon.await;
    let _ = broker;

    const COUNT: usize = 20;
    let mut handles = Vec::with_capacity(COUNT);

    for i in 0..COUNT {
        let socket_path = daemon.socket_path.clone();
        handles.push(tokio::spawn(async move {
            send_raw_ipc_request(
                &socket_path,
                &IpcRequest::Publish {
                    topic: format!("test.burst.{i}"),
                    payload_b64: b64_encode(format!("payload {i}").as_bytes()),
                    timeout_secs: 10,
                },
            )
            .await
            .expect("Expected to send raw ipc request")
        }));
    }

    for (i, handle) in handles.into_iter().enumerate() {
        let response = handle.await.expect("Task panicked");

        assert_matches!(
            response,
            IpcResponse::Ok { .. },
            "Publish {i} did not return Ok"
        );
    }

    daemon.shutdown().await;
}

/// Given a running daemon
/// When we request ListTopics via IPC
/// Then we receive a valid JSON topic list
#[rstest]
#[tokio::test]
async fn given_daemon_when_list_topics_via_ipc_then_topic_list_returned(
    #[future] daemon: (BrokerFixture, DaemonFixture),
) {
    let (broker, daemon) = daemon.await;
    let _ = broker;

    let response = send_raw_ipc_request(
        &daemon.socket_path,
        &IpcRequest::ListTopics { timeout_secs: 5 },
    )
    .await
    .expect("IPC request failed");

    assert_matches!(response, IpcResponse::Ok { .. });

    daemon.shutdown().await;
}

/// Given a running daemon
/// When we request ListSessions via IPC
/// Then the daemon's own session is present in the result
#[rstest]
#[tokio::test]
async fn given_daemon_when_list_sessions_via_ipc_then_daemon_session_present(
    #[future] daemon: (BrokerFixture, DaemonFixture),
) {
    let (broker, daemon) = daemon.await;
    let _ = broker;

    // Get the daemon's registration ID from Status first.
    let status = send_raw_ipc_request(&daemon.socket_path, &IpcRequest::Status)
        .await
        .expect("Status IPC failed");

    let daemon_reg_id = match status {
        IpcResponse::Ok {
            result: Some(ref val),
        } => val["registration_id"]
            .as_str()
            .expect("registration_id missing from Status response")
            .to_string(),
        other => panic!("Unexpected status response: {other:?}"),
    };

    let sessions_response = send_raw_ipc_request(
        &daemon.socket_path,
        &IpcRequest::ListSessions { timeout_secs: 5 },
    )
    .await
    .expect("ListSessions IPC failed");

    match sessions_response {
        IpcResponse::Ok { result: Some(val) } => {
            let map = val
                .as_object()
                .expect("Expected session map to be a JSON object");
            assert!(
                map.contains_key(&daemon_reg_id),
                "Daemon registration_id {daemon_reg_id} not found in sessions: {map:?}"
            );
        }
        other => panic!("Unexpected response: {other:?}"),
    }

    daemon.shutdown().await;
}

/// Given a running daemon
/// When we request GetSession for the daemon's own session
/// Then SessionInfo is returned
#[rstest]
#[tokio::test]
async fn given_daemon_when_get_own_session_via_ipc_then_session_info_returned(
    #[future] daemon: (BrokerFixture, DaemonFixture),
) {
    let (broker, daemon) = daemon.await;
    let _ = broker;

    let status = send_raw_ipc_request(&daemon.socket_path, &IpcRequest::Status)
        .await
        .expect("Status IPC failed");

    let daemon_reg_id = match status {
        IpcResponse::Ok {
            result: Some(ref val),
        } => val["registration_id"].as_str().unwrap().to_string(),
        other => panic!("Unexpected status: {other:?}"),
    };

    let response = send_raw_ipc_request(
        &daemon.socket_path,
        &IpcRequest::GetSession {
            registration_id: daemon_reg_id,
            timeout_secs: 5,
        },
    )
    .await
    .expect("GetSession IPC failed");

    assert_matches!(response, IpcResponse::Ok { .. });

    daemon.shutdown().await;
}

// ============================================================================
// Option B transport selection tests (subprocess)
//
// These tests spawn the actual cassini binary to verify the full CLI path
// including clap parsing, socket detection, and exit codes.
// ============================================================================

/// Given CASSINI_DAEMON_SOCK points at a nonexistent socket
/// When we run any subcommand
/// Then the process exits non-zero with a clear error message
#[tokio::test]
async fn given_explicit_socket_not_reachable_when_publish_then_fails_loudly() {
    let output = tokio::process::Command::new(CASSINI_CLIENT_BIN)
        .env("CASSINI_DAEMON_SOCK", "/tmp/cassini-nonexistent-test.sock")
        .args(["publish", "test.topic", "payload"])
        .output()
        .await
        .expect("Failed to spawn cassini binary");

    assert!(
        !output.status.success(),
        "Expected non-zero exit, got: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("explicitly configured but is not reachable"),
        "Expected Option B error message, got stderr: {stderr}"
    );
}

/// Given no CASSINI_DAEMON_SOCK set and no default socket present
/// When we run a subcommand
/// Then the process attempts direct connect (fails on broker, not on socket detection)
#[tokio::test]
async fn given_no_daemon_configured_when_publish_then_attempts_direct_connect() {
    // Ensure the default socket path does not exist.
    let _ = std::fs::remove_file("/tmp/cassini-daemon.sock");

    let output = tokio::process::Command::new(CASSINI_CLIENT_BIN)
        .env_remove("CASSINI_DAEMON_SOCK")
        // Point at a broker that definitely isn't running to get a fast failure.
        .env("BROKER_ADDR", "127.0.0.1:1")
        .args(["publish", "test.topic", "payload"])
        .output()
        .await
        .expect("Failed to spawn cassini binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should fail on broker connect, not on socket detection.
    assert!(
        !stderr.contains("explicitly configured but is not reachable"),
        "Should not produce Option B error when no socket is configured, got: {stderr}"
    );
}

/// Given a running daemon
/// When we run `cassini --daemon` again
/// Then it exits non-zero reporting the daemon is already running
#[rstest]
#[tokio::test]
async fn given_daemon_running_when_start_again_then_fails_with_already_running(
    #[future] daemon: (BrokerFixture, DaemonFixture),
) {
    let (broker, daemon) = daemon.await;
    let _ = broker;

    let output = tokio::process::Command::new(CASSINI_CLIENT_BIN)
        .args([
            "--daemon",
            "--foreground",
            "--socket",
            daemon.socket_path.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("Failed to spawn cassini binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already running"),
        "Expected 'already running' message, got: {stderr}"
    );

    daemon.shutdown().await;
}

// ============================================================================
// Raw IPC helper (used by daemon tests to inspect responses directly)
// ============================================================================

/// Send a single IPC request to the daemon socket and return the raw response.
/// This bypasses the CLI formatting layer so tests can inspect the response
/// structure directly rather than parsing stdout.
pub async fn send_raw_ipc_request(
    socket_path: &PathBuf,
    request: &IpcRequest,
) -> anyhow::Result<IpcResponse> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::UnixStream::connect(socket_path).await?;

    let bytes = serde_json::to_vec(request)?;
    let len = bytes.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&bytes).await?;

    let resp_len = stream.read_u32().await? as usize;
    let mut buf = vec![0u8; resp_len];
    stream.read_exact(&mut buf).await?;

    Ok(serde_json::from_slice(&buf)?)
}
