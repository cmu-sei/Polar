# Cassini Client

A CLI client for the Cassini message broker. Connects over mTLS, registers a session, and exposes broker operations as subcommands. Supports two transport modes: direct connect (one registration per invocation) and daemon mode (one persistent registration shared across many invocations via a Unix socket).

This package also includes the libary, which contains the implementation.

---

## Architecture

### Actor model

The client is built on [ractor](https://github.com/slawlor/ractor). The core actor is `TcpClientActor` from `cassini-client` (the library crate), which manages the TLS connection lifecycle, session registration, and message dispatch. It is stateful — messages are only accepted once the actor has transitioned to `RegistrationState::Registered`.

The CLI layer sits above this via a `CompletionBridge` actor. Because the `TcpClientActor` read loop captures its event handler by clone at spawn time (making runtime handler swaps ineffective), the bridge uses `Arc<Mutex<...>>` shared state to rearm between operation phases without touching the actor system. A single bridge instance lives for the full connection lifetime and is rearmed by the `BridgeHandle` between registration, publish, and control phases.

```
CLI main
  └── Session
        ├── TcpClientActor          (ractor actor — TLS connection + broker wire protocol)
        ├── CompletionBridge        (ractor actor — routes ClientEvents to oneshot channels)
        └── BridgeHandle            (owned by main — rearmed between operation phases)
```

### Transport modes

**Direct connect** — the default. Each invocation registers a fresh session with the broker, performs its operation, disconnects, and exits. Registration involves a TLS handshake and broker-side session allocation, so latency is proportional to network conditions.

**Daemon mode** — a long-lived process that registers once and listens on a Unix domain socket for IPC requests from subsequent CLI invocations. Each IPC connection is a single framed JSON request/response. The daemon serializes all broker operations through a `tokio::sync::Mutex<Session>` since the broker session is not multiplexable — responses carry no correlation ID.

**Transport selection (Option B):** if `CASSINI_DAEMON_SOCK` or `--socket` is explicitly set and the socket is unreachable, the CLI fails loudly rather than silently falling back to direct connect. If neither is configured and the default socket path does not exist, direct connect is used transparently.

### IPC protocol

Framing: `[u32 big-endian length][JSON body]` on both request and response.

```json
// Request
{ "op": "publish", "topic": "my.topic", "payload_b64": "<base64>", "timeout_secs": 10 }

// Response (success)
{ "status": "ok", "result": { "topic": "my.topic" } }

// Response (error)
{ "status": "error", "reason": "Transport error: ..." }
```

Supported ops: `publish`, `list_sessions`, `list_topics`, `get_session`, `status`.

---

## Environment variables

These must be set for both the CLI and the broker subprocess in tests.

| Variable | Required | Description |
|---|---|---|
| `BROKER_ADDR` | Yes | Broker TCP address, e.g. `127.0.0.1:8080` |
| `CASSINI_SERVER_NAME` | Yes | TLS SNI name for the broker |
| `TLS_CA_CERT` | Yes | Path to CA certificate PEM |
| `TLS_CLIENT_CERT` | Yes | Path to client certificate PEM |
| `TLS_CLIENT_KEY` | Yes | Path to client private key PEM |
| `CASSINI_DAEMON_SOCK` | No | Override Unix socket path for daemon IPC |
| `CASSINI_REGISTER_TIMEOUT_SECS` | No | Registration timeout override (default: 30) |
| `CASSINI_LOG` | No | Log filter for the client (default: `warn`) |

---

## Usage

### Direct connect

```bash
# Publish a message
cassini-client publish my.topic "hello world"

# List all active broker sessions
cassini-client list-sessions

# List all topics
cassini-client list-topics

# Get details for a specific session
cassini-client get-session <registration-id>

# JSON output (all subcommands support --format)
cassini-client --format json list-sessions
```

### Daemon mode

```bash
# Start the daemon (backgrounds itself)
cassini-client --daemon

# Start in foreground (useful for systemd or supervision)
cassini-client --daemon --foreground

# Use a custom socket path
cassini-client --daemon --socket /run/cassini/client.sock

# Check daemon status
cassini-client status

# All subcommands route through the daemon automatically
# when the socket is reachable
cassini-client publish my.topic "hello"
cassini-client list-topics
```

The daemon cleans up its socket and pidfile on `SIGTERM` and `SIGINT`. If the daemon crashes and leaves a stale socket, the next `--daemon` invocation detects it (connect fails), removes it, and starts fresh.

### Timeout overrides

Every subcommand accepts a `--timeout` flag (or `--publish-timeout` for publish). The registration timeout is controlled globally via `--register-timeout` or `CASSINI_REGISTER_TIMEOUT_SECS`.

```bash
cassini-client --register-timeout 10 publish my.topic "payload" --publish-timeout 5
```

---

## Building

From the workspace root (`Polar/src/agents`):

```bash
# Build both binaries
cargo build -p cassini-client -p cassini-server

# Release build
cargo build -p cassini-client -p cassini-server --release
```

---

## Testing

### Prerequisites

The integration tests spawn a real `cassini-server` subprocess and connect to it over mTLS. Before running tests, ensure:

1. Both binaries are built: `cargo build -p cassini-client -p cassini-server`
2. TLS environment variables are exported pointing at valid test certificates (the workspace-level `certs/` directory contains these):

```bash
export TLS_CA_CERT=../../certs/ca_certificates/ca.pem
export TLS_CLIENT_CERT=../../certs/client/client.pem
export TLS_CLIENT_KEY=../../certs/client/client.key
export TLS_SERVER_CERT_CHAIN=../../certs/server/server.pem
export TLS_SERVER_KEY=../../certs/server/server.key
export CASSINI_SERVER_NAME=cassini.local   # or whatever your test CN is
```

3. The `build.rs` in this crate resolves the binary paths automatically via `CASSINI_SERVER_BIN` and `CASSINI_CLIENT_BIN` compile-time constants — no manual path configuration needed.

### Running the integration tests

```bash
# From cassini/client/
cargo test --test integration_tests

# With broker logs visible (useful when debugging test failures)
CASSINI_LOG=debug cargo test --test integration_tests -- --nocapture
```

> **Note on parallelism:** Each test fixture starts its own broker instance on a random port and constructs an isolated `TCPClientConfig` pointing at it — no global env var mutation occurs during test execution, so tests are safe to run in parallel. However, the current fixture teardown does not yet enforce ordering between fixtures sharing the same tokio runtime. If you observe flaky failures under high parallelism, add `--test-threads=1` as a short-term mitigation while the fixture harness is hardened.

### Test structure

```
tests/
  integration_tests.rs    — full integration suite
    BrokerFixture         — spawns cassini-server on a random port, kills on drop
    DaemonFixture         — starts CLI daemon in-process, shutdown via oneshot channel
    Direct-connect tests  — register/publish/control ops over a raw Session
    Daemon IPC tests      — publish/control/status via Unix socket framing
    Option B tests        — subprocess tests for transport selection and exit codes
```

The BDD naming convention (`given_X_when_Y_then_Z`) is intentional — each test name is a complete specification of its precondition, action, and expected outcome. New tests should follow this convention.

### Test coverage

| Scenario | Type | Covered |
|---|---|---|
| Direct registration produces non-empty ID | Direct | ✓ |
| Publish ack received on correct topic | Direct | ✓ |
| ListTopics returns TopicList variant | Direct | ✓ |
| ListSessions includes own session | Direct | ✓ |
| GetSession returns SessionInfo for known ID | Direct | ✓ |
| GetSession returns NotFound for unknown ID | Direct | ✓ |
| Daemon status returns pid + registration ID | Daemon IPC | ✓ |
| Daemon publish ack contains topic | Daemon IPC | ✓ |
| 20 concurrent publishes all acked | Daemon IPC | ✓ |
| ListTopics via IPC | Daemon IPC | ✓ |
| ListSessions via IPC includes daemon session | Daemon IPC | ✓ |
| GetSession via IPC for daemon's own session | Daemon IPC | ✓ |
| Explicit socket unreachable → fail loudly | Subprocess | ✓ |
| No socket configured → direct connect attempted | Subprocess | ✓ |
| Second daemon on same socket → already running | Subprocess | ✓ |
