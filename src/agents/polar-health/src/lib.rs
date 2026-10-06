//! polar-health
//!
//! Shared health state types and the HealthCheckActor for Polar agents.
//!
//! Each agent supervisor spawns a HealthCheckActor as a linked child.
//! The actor owns all health concerns:
//!   - Periodic cert expiry checks via x509-parser
//!   - Tracking of connection state (Cassini, graph)
//!   - Writing the health file read by the polar-healthcheck liveness probe binary
//!   - Rejuvenation state machine: when approaching cert expiry, waits for dep
//!     certs to be healthy, applies jitter, then terminates itself directly
//!     (myself.stop(Some(reason))) -- ordinary, real Ractor supervision is
//!     the whole, sufficient mechanism here; the supervisor sees this via a
//!     genuine SupervisionEvent::ActorTerminated, carrying this same, real
//!     reason string, and reacts by exiting (or, for a supervisor composed
//!     from polar-agent-builder's own AgentBuilder, by beginning its own,
//!     real, coordinated shutdown sequence). No separate port/ack handshake
//!     is needed at all -- an earlier version of this actor had one
//!     (OutputPort<()> + a real ShutdownAck message), removed as genuine,
//!     unnecessary machinery once it became clear plain supervision already,
//!     directly provided everything it existed for.
//!   - Graph availability deadline: for agents that expect a graph connection,
//!     tracks time since the last real (canary or app-level) successful graph
//!     operation. If that exceeds graph_deadline_secs -- whether the graph
//!     never became available at all, or was available and then went dark --
//!     the actor fails itself, which the supervisor treats as fatal.

use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use x509_parser::prelude::*;

mod certs;
pub mod config;
#[cfg(test)]
mod test_support;

pub use config::HealthConfig;

// ---------------------------------------------------------------------------
// Environment variable constants
// ---------------------------------------------------------------------------

pub const POLAR_HEALTH_FILE_ENV: &str = "POLAR_HEALTH_FILE";
pub const POLAR_HEALTH_FILE_DEFAULT: &str = "/tmp/polar-health.json";
pub const POLAR_HEALTH_CERTS_ENV: &str = "POLAR_HEALTH_CERTS";
pub const POLAR_HEALTH_EXPIRY_SECS_ENV: &str = "POLAR_HEALTH_EXPIRY_SECS";
pub const POLAR_HEALTH_STALE_SECS_ENV: &str = "POLAR_HEALTH_STALE_SECS";
pub const POLAR_HEALTH_TICK_SECS_ENV: &str = "POLAR_HEALTH_TICK_SECS";
pub const POLAR_HEALTH_DEP_ENDPOINTS_ENV: &str = "POLAR_HEALTH_DEP_ENDPOINTS";
pub const POLAR_HEALTH_REJUVENATION_SECS_ENV: &str = "POLAR_HEALTH_REJUVENATION_SECS";
pub const POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV: &str = "POLAR_HEALTH_GRAPH_DEADLINE_SECS";

pub const DEFAULT_EXPIRY_THRESHOLD_SECS: i64 = 60;
pub const DEFAULT_TICK_SECS: u64 = 30;
pub const DEFAULT_REJUVENATION_THRESHOLD_SECS: i64 = 300;
/// Jitter window width in seconds. Must comfortably exceed DEFAULT_TICK_SECS
/// (and any agent's configured tick_secs) or jitter effectively rounds up to
/// the next tick boundary and every agent in WaitingForDeps at the same time
/// fires within one tick of each other -- a thundering herd on cert-issuer.
pub const DEFAULT_JITTER_WINDOW_SECS: u64 = 180;
/// How long a graph-expecting agent may go without a real successful graph
/// operation (canary or app-level write) before HealthCheckActor fails itself.
/// Covers both "never connected" and "was connected, then went dark."
pub const DEFAULT_GRAPH_DEADLINE_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// HealthStatus — typed status enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "healthy"),
            HealthStatus::Unhealthy => write!(f, "unhealthy"),
        }
    }
}

// ---------------------------------------------------------------------------
// HealthState — the JSON file format read by polar-healthcheck
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthState {
    pub status: HealthStatus,
    /// Unix timestamp (seconds) when this state was written.
    pub timestamp: u64,
    #[serde(default)]
    pub cassini_connected: bool,
    pub graph_connected: bool,
}

impl HealthState {
    pub fn healthy(cassini_connected: bool, graph_connected: bool) -> Self {
        Self {
            status: HealthStatus::Healthy,
            timestamp: Self::now(),
            cassini_connected,
            graph_connected,
        }
    }

    pub fn unhealthy(cassini_connected: bool, graph_connected: bool) -> Self {
        Self {
            status: HealthStatus::Unhealthy,
            timestamp: Self::now(),
            cassini_connected,
            graph_connected,
        }
    }

    /// Write the health state atomically to the configured path.
    /// Uses a tmp file + rename to avoid partial reads by the healthcheck binary.
    pub fn write(&self) {
        let path = std::env::var(POLAR_HEALTH_FILE_ENV)
            .unwrap_or_else(|_| POLAR_HEALTH_FILE_DEFAULT.to_string());

        let json = match serde_json::to_string(self) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("Failed to serialize health state: {e}");
                return;
            }
        };

        let tmp = format!("{path}.tmp");
        if let Err(e) = fs::write(&tmp, &json) {
            tracing::warn!("Failed to write health state tmp file {tmp}: {e}");
            return;
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            tracing::warn!("Failed to rename health state tmp file to {path}: {e}");
        }
    }

    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|e| {
                tracing::warn!("SystemTime before UNIX_EPOCH: {e}");
                Duration::ZERO
            })
            .as_secs()
    }
}

// ---------------------------------------------------------------------------
// DepCertEndpoint — a dependency TLS endpoint to check during rejuvenation
// ---------------------------------------------------------------------------

/// Valid hostname label characters: ASCII alphanumeric and hyphen.
/// Labels must not start or end with a hyphen, must not be empty.
fn validate_hostname(hostname: &str) -> Result<(), String> {
    if hostname.is_empty() {
        return Err("hostname must not be empty".to_string());
    }
    if hostname.len() > 253 {
        return Err(format!("hostname '{}' exceeds 253 characters", hostname));
    }
    for label in hostname.split('.') {
        if label.is_empty() {
            return Err(format!(
                "hostname '{}' contains an empty label (double dot or trailing dot)",
                hostname
            ));
        }
        if label.len() > 63 {
            return Err(format!(
                "hostname '{}' has label '{}' exceeding 63 characters",
                hostname, label
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!(
                "hostname '{}' has label '{}' with leading or trailing hyphen",
                hostname, label
            ));
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!(
                "hostname '{}' has label '{}' with invalid characters (only [a-zA-Z0-9-] allowed)",
                hostname, label
            ));
        }
    }
    Ok(())
}

/// A dependency TLS endpoint whose server certificate TTL we check
/// before and during rejuvenation.
///
/// Parsed from the string format `"host:port:min_ttl_seconds"`.
#[derive(Debug, Clone)]
pub struct DepCertEndpoint {
    pub host: String,
    pub port: u16,
    pub min_ttl_secs: i64,
}

impl DepCertEndpoint {
    /// Parse and validate a dep cert endpoint string.
    /// Format: `"host:port:min_ttl_seconds"`
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.splitn(3, ':').collect();
        if parts.len() != 3 {
            return Err(format!(
                "invalid dep cert endpoint '{}': expected host:port:min_ttl_seconds",
                s
            ));
        }

        let host = parts[0].trim().to_string();
        validate_hostname(&host)?;

        let port = parts[1]
            .trim()
            .parse::<u16>()
            .map_err(|e| format!("invalid port in '{}': {}", s, e))?;
        if port == 0 {
            return Err(format!("invalid port in '{}': port 0 is not allowed", s));
        }

        let min_ttl_secs = parts[2]
            .trim()
            .parse::<i64>()
            .map_err(|e| format!("invalid min_ttl_secs in '{}': {}", s, e))?;
        if min_ttl_secs < 0 {
            return Err(format!(
                "invalid min_ttl_secs in '{}': must be non-negative",
                s
            ));
        }

        Ok(DepCertEndpoint {
            host,
            port,
            min_ttl_secs,
        })
    }

    /// Connect to the endpoint, retrieve the server's TLS certificate,
    /// and check whether it has at least `min_ttl_secs` remaining.
    ///
    /// Returns `Ok(remaining_secs)` if healthy, `Err(reason)` if not.
    ///
    /// Uses a simple TCP + rustls handshake; does not verify the server
    /// cert against a CA (we only care about the cert's TTL, not its
    /// trust chain, since we manage the CA ourselves).
    pub fn check_ttl(&self) -> Result<i64, String> {
        use rustls::ClientConfig;
        use rustls::pki_types::ServerName;
        use std::io::Write;

        let addr = format!("{}:{}", self.host, self.port);

        let stream = TcpStream::connect(&addr)
            .map_err(|e| format!("failed to connect to {addr}: {e}"))?;

        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("failed to set read timeout on {addr}: {e}"))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("failed to set write timeout on {addr}: {e}"))?;

        // We use a dangerous config that skips cert verification — we only
        // want to inspect the cert's TTL, not verify its trust chain.
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
            .with_no_client_auth();
        let config = Arc::new(config);

        let server_name = ServerName::try_from(self.host.as_str().to_owned())
            .map_err(|e| format!("invalid server name '{}': {e}", self.host))?;

        let mut conn = rustls::ClientConnection::new(Arc::clone(&config), server_name)
            .map_err(|e| format!("failed to create TLS connection to {addr}: {e}"))?;

        let mut stream = stream;
        let mut tls = rustls::Stream::new(&mut conn, &mut stream);

        // Drive the handshake by flushing — this triggers the TLS exchange.
        tls.flush()
            .map_err(|e| format!("TLS handshake failed with {addr}: {e}"))?;

        let certs = conn
            .peer_certificates()
            .ok_or_else(|| format!("no peer certificates received from {addr}"))?;

        let first = certs
            .first()
            .ok_or_else(|| format!("empty certificate chain from {addr}"))?;

        let (_, cert) = parse_x509_certificate(first.as_ref())
            .map_err(|e| format!("failed to parse server certificate from {addr}: {e}"))?;

        let not_after = cert.validity().not_after.timestamp();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system clock error: {e}"))?
            .as_secs() as i64;

        let remaining = not_after.checked_sub(now).ok_or_else(|| {
            format!("certificate timestamp arithmetic overflow for {addr}")
        })?;

        if remaining < self.min_ttl_secs {
            Err(format!(
                "dep cert at {addr} expires in {remaining}s (minimum required: {}s)",
                self.min_ttl_secs
            ))
        } else {
            Ok(remaining)
        }
    }
}

/// A rustls certificate verifier that accepts any certificate.
/// Used only for TTL inspection — we manage the CA ourselves and
/// care about cert freshness, not trust chain validation here.
#[derive(Debug)]
struct NoCertVerifier;

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

// ---------------------------------------------------------------------------
// RejuvenationPhase
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum RejuvenationPhase {
    /// Normal operation — not yet within rejuvenation threshold.
    Normal,
    /// Within threshold — waiting for all dep certs to be healthy.
    WaitingForDeps,
    /// Deps healthy — jitter timer running. `fire_at_secs` is the Unix
    /// timestamp at which we will signal the supervisor.
    Jittering { fire_at_secs: u64 },
}

// ---------------------------------------------------------------------------
// HealthCheckMessage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum HealthCheckMessage {
    /// Cassini TLS connection established and registered.
    CassiniConnected,
    /// Cassini connection lost or rejected.
    CassiniDisconnected,
    /// Graph controller actor spawned. Does NOT imply successful auth or
    /// connectivity — neo4rs::Graph::connect only builds pool config, it
    /// does not handshake. Use GraphAvailable for real connectivity.
    GraphConnected,
    /// Graph database connection lost.
    GraphDisconnected,
    /// A real operation (canary or application write) against Neo4j
    /// succeeded. This is what actually proves the graph is reachable.
    GraphAvailable,
    /// A real operation against Neo4j failed. Carries a short reason,
    /// logged but not immediately fatal — see graph_deadline_secs.
    GraphOpFailed(String),
    /// Internal periodic tick — do not send externally.
    Tick,
}

// ---------------------------------------------------------------------------
// HealthCheckArgs
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct HealthCheckArgs {
    /// Does this agent connect to a graph database?
    pub expects_graph: bool,

    /// Seconds before own cert expiry to enter rejuvenation mode.
    pub rejuvenation_threshold_secs: i64,

    /// Dependency TLS endpoints to check during rejuvenation.
    /// Parsed from `"host:port:min_ttl_seconds"`.
    pub dep_cert_endpoints: Vec<DepCertEndpoint>,
}

impl std::fmt::Debug for HealthCheckArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthCheckArgs")
            .field("expects_graph", &self.expects_graph)
            .field("rejuvenation_threshold_secs", &self.rejuvenation_threshold_secs)
            .field("dep_cert_endpoints", &self.dep_cert_endpoints)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// HealthCheckActorState
// ---------------------------------------------------------------------------

pub struct HealthCheckActorState {
    pub cassini_connected: bool,
    pub graph_connected: bool,
    pub expects_graph: bool,
    pub expiry_threshold_secs: i64,
    pub rejuvenation_threshold_secs: i64,
    pub tick_secs: u64,
    pub cert_paths: Vec<String>,
    pub dep_cert_endpoints: Vec<DepCertEndpoint>,
    pub rejuvenation_phase: RejuvenationPhase,
    /// Seconds an expects_graph agent may go without a real graph success
    /// before HealthCheckActor fails itself.
    pub graph_deadline_secs: u64,
    /// Unix timestamp of the last GraphAvailable. None if never seen.
    pub last_graph_success_secs: Option<u64>,
    /// Unix timestamp this actor started — anchors the deadline for the
    /// "never succeeded even once" case.
    pub actor_start_secs: u64,
}

impl HealthCheckActorState {
    /// Returns `true` if all required connections are established.
    /// Note: uses graph_connected (actor spawned), not graph availability.
    /// Graph availability is tracked separately via last_graph_success_secs
    /// and enforced by the deadline check in Tick.
    fn connections_healthy(&self) -> bool {
        if !self.cassini_connected {
            return false;
        }
        if self.expects_graph && !self.graph_connected {
            return false;
        }
        true
    }

    /// Returns `true` if all dep cert endpoints have healthy TTLs.
    /// Logs warnings for any that fail; does not short-circuit so all
    /// failures are visible in a single tick.
    ///
    /// check_ttl() does blocking network + TLS handshake I/O (up to ~5s per
    /// endpoint via its read/write timeouts). We run it via spawn_blocking
    /// so it doesn't hold up whatever thread this tick is running on.
    async fn deps_healthy(&self) -> bool {
        if self.dep_cert_endpoints.is_empty() {
            return true;
        }

        let mut all_healthy = true;
        for dep in &self.dep_cert_endpoints {
            let dep = dep.clone();
            let result = tokio::task::spawn_blocking(move || dep.check_ttl()).await;

            match result {
                Ok(Ok(remaining)) => {
                    tracing::debug!("dep cert healthy, {remaining}s remaining");
                }
                Ok(Err(reason)) => {
                    tracing::warn!("healthcheck: dep cert unhealthy: {reason}");
                    all_healthy = false;
                }
                Err(join_err) => {
                    tracing::warn!(
                        "healthcheck: dep cert check task panicked or was cancelled: {join_err}"
                    );
                    all_healthy = false;
                }
            }
        }
        all_healthy
    }

    /// Checks the graph availability deadline for expects_graph agents.
    /// Returns `Err` describing the violation if the deadline has been
    /// exceeded, whether the graph never became available or went dark
    /// after previously succeeding.
    fn check_graph_deadline(&self) -> Result<(), String> {
        if !self.expects_graph {
            return Ok(());
        }

        let now = HealthState::now();
        match self.last_graph_success_secs {
            Some(last_success) => {
                let elapsed = now.saturating_sub(last_success);
                if elapsed > self.graph_deadline_secs {
                    return Err(format!(
                        "graph unavailable for {elapsed}s (deadline: {}s)",
                        self.graph_deadline_secs
                    ));
                }
            }
            None => {
                let elapsed = now.saturating_sub(self.actor_start_secs);
                if elapsed > self.graph_deadline_secs {
                    return Err(format!(
                        "graph never became available within {elapsed}s (deadline: {}s)",
                        self.graph_deadline_secs
                    ));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HealthCheckActor
// ---------------------------------------------------------------------------

pub struct HealthCheckActor;

#[async_trait]
impl Actor for HealthCheckActor {
    type Msg = HealthCheckMessage;
    type State = HealthCheckActorState;
    type Arguments = HealthCheckArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: HealthCheckArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let config = HealthConfig::load(args).map_err(ActorProcessingErr::from)?;

        Ok(HealthCheckActorState {
            cassini_connected: false,
            graph_connected: false,
            expects_graph: config.expects_graph,
            expiry_threshold_secs: config.expiry_threshold_secs,
            rejuvenation_threshold_secs: config.rejuvenation_threshold_secs,
            tick_secs: config.tick_secs,
            cert_paths: config.cert_paths,
            dep_cert_endpoints: config.dep_cert_endpoints,
            rejuvenation_phase: RejuvenationPhase::Normal,
            graph_deadline_secs: config.graph_deadline_secs,
            last_graph_success_secs: None,
            actor_start_secs: HealthState::now(),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            HealthCheckMessage::CassiniConnected => {
                state.cassini_connected = true;
                tracing::debug!("healthcheck: cassini connected");
                let _ = myself
                    .send_after(
                        Duration::from_secs(state.tick_secs),
                        || HealthCheckMessage::Tick,
                    )
                    .await;
            }

            HealthCheckMessage::CassiniDisconnected => {
                state.cassini_connected = false;
                tracing::warn!("healthcheck: cassini disconnected");
                HealthState::unhealthy(false, state.graph_connected).write();
                // Don't fail — the TCP client will attempt to reconnect.
                // The tick-based connection health check will catch it if
                // reconnection doesn't succeed.
            }

            HealthCheckMessage::GraphConnected => {
                state.graph_connected = true;
                tracing::debug!("healthcheck: graph connected (actor spawned; not yet verified)");
            }

            HealthCheckMessage::GraphDisconnected => {
                state.graph_connected = false;
                tracing::warn!("healthcheck: graph disconnected");
                if state.expects_graph {
                    HealthState::unhealthy(state.cassini_connected, false).write();
                }
                // Don't fail — allow reconnection to recover.
            }

            HealthCheckMessage::GraphAvailable => {
                state.graph_connected = true;
                state.last_graph_success_secs = Some(HealthState::now());
                tracing::debug!("healthcheck: graph available (real op succeeded)");
            }

            HealthCheckMessage::GraphOpFailed(reason) => {
                tracing::warn!("healthcheck: graph op failed: {reason}");
                HealthState::unhealthy(state.cassini_connected, state.graph_connected).write();
                // Don't fail immediately — a single failed op could be
                // transient. check_graph_deadline (run every Tick) is what
                // decides whether this has gone on too long.
            }

            HealthCheckMessage::Tick => {
                // ----------------------------------------------------------------
                // Hard deadline checks — own certs and, for expects_graph
                // agents, graph availability. Always checked regardless of
                // rejuvenation phase.
                // ----------------------------------------------------------------
                let now_secs = HealthState::now() as i64;
                if let Err(reason) =
                    certs::check_hard(&state.cert_paths, state.expiry_threshold_secs, now_secs)
                {
                    tracing::error!("healthcheck: cert check failed (hard deadline): {reason}");
                    HealthState::unhealthy(
                        state.cassini_connected,
                        state.graph_connected,
                    )
                    .write();
                    return Err(ActorProcessingErr::from(reason));
                }

                if let Err(reason) = state.check_graph_deadline() {
                    tracing::error!("healthcheck: {reason}");
                    HealthState::unhealthy(state.cassini_connected, false).write();
                    return Err(ActorProcessingErr::from(reason));
                }

                match state.rejuvenation_phase.clone() {
                    // ------------------------------------------------------------
                    // Normal operation
                    // ------------------------------------------------------------
                    RejuvenationPhase::Normal => {
                        // Connection health check
                        if !state.connections_healthy() {
                            tracing::warn!(
                                "healthcheck: connections unhealthy (cassini={} graph={} expects_graph={}), waiting for recovery",
                                state.cassini_connected,
                                state.graph_connected,
                                state.expects_graph
                            );
                            HealthState::unhealthy(
                                state.cassini_connected,
                                state.graph_connected,
                            )
                            .write();
                            // Don't fail — reschedule tick and wait for recovery.
                            let _ = myself
                                .send_after(
                                    Duration::from_secs(state.tick_secs),
                                    || HealthCheckMessage::Tick,
                                )
                                .await;
                            return Ok(());
                        }

                        // Check whether we've entered the rejuvenation window
                        match certs::min_ttl(&state.cert_paths, now_secs) {
                            None => {
                                tracing::warn!(
                                    "healthcheck: could not determine cert TTL, \
                                     skipping rejuvenation window check"
                                );
                            }
                            Some(min_ttl) => {
                                if min_ttl <= state.rejuvenation_threshold_secs {
                                    tracing::info!(
                                        "healthcheck: entering rejuvenation mode \
                                         (cert TTL {min_ttl}s <= threshold {}s)",
                                        state.rejuvenation_threshold_secs
                                    );
                                    state.rejuvenation_phase =
                                        RejuvenationPhase::WaitingForDeps;
                                }
                            }
                        }

                        HealthState::healthy(
                            state.cassini_connected,
                            state.graph_connected,
                        )
                        .write();

                        let _ = myself
                            .send_after(
                                Duration::from_secs(state.tick_secs),
                                || HealthCheckMessage::Tick,
                            )
                            .await;
                    }

                    // ------------------------------------------------------------
                    // Rejuvenation: waiting for dep certs to be healthy
                    // ------------------------------------------------------------
                    RejuvenationPhase::WaitingForDeps => {
                        if state.deps_healthy().await {
                            // Jitter window must dominate tick_secs, or every
                            // agent that enters this phase around the same
                            // time (e.g. after a shared cert-issuer rotation)
                            // fires within one tick of each other regardless
                            // of the jitter value chosen -- a thundering herd
                            // on cert-issuer. DEFAULT_JITTER_WINDOW_SECS (180s)
                            // comfortably exceeds the default 30s tick.
                            let jitter_secs =
                                u64::from(rand::random::<u32>()) % DEFAULT_JITTER_WINDOW_SECS;
                            let fire_at_secs = HealthState::now()
                                .checked_add(jitter_secs)
                                .unwrap_or(HealthState::now());

                            tracing::info!(
                                "healthcheck: deps healthy, jitter {jitter_secs}s (window {}s), \
                                 will signal shutdown at t={fire_at_secs}",
                                DEFAULT_JITTER_WINDOW_SECS
                            );
                            state.rejuvenation_phase =
                                RejuvenationPhase::Jittering { fire_at_secs };
                        } else {
                            tracing::info!(
                                "healthcheck: rejuvenation — waiting for dep certs to become healthy"
                            );
                        }

                        let _ = myself
                            .send_after(
                                Duration::from_secs(state.tick_secs),
                                || HealthCheckMessage::Tick,
                            )
                            .await;
                    }

                    // ------------------------------------------------------------
                    // Rejuvenation: jitter timer running
                    // ------------------------------------------------------------
                    RejuvenationPhase::Jittering { fire_at_secs } => {
                        let now = HealthState::now();
                        if now >= fire_at_secs {
                            tracing::info!(
                                "healthcheck: jitter elapsed, terminating for rejuvenation"
                            );
                            // Ordinary, real Ractor supervision is the
                            // whole, sufficient mechanism -- our own
                            // supervisor sees this via a genuine
                            // SupervisionEvent::ActorTerminated,
                            // carrying this exact, real reason string,
                            // with no separate port/ack handshake
                            // needed at all.
                            myself.stop(Some(
                                "rejuvenation: certificate rejuvenation threshold reached"
                                    .to_string(),
                            ));
                        } else {
                            let remaining = fire_at_secs.saturating_sub(now);
                            tracing::debug!(
                                "healthcheck: jitter pending, {remaining}s until shutdown signal"
                            );
                            let _ = myself
                                .send_after(
                                    Duration::from_secs(state.tick_secs),
                                    || HealthCheckMessage::Tick,
                                )
                                .await;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// These pin the behaviour of the health system as it is, before the testability
// refactor. Where today's behaviour is a known defect (see the "Health system
// rework" plan in remaining-work.md) the test says so, and the fix flips it.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::cert_file;
    use ractor::ActorStatus;

    // -- helpers ------------------------------------------------------------

    fn state(cert_paths: Vec<String>) -> HealthCheckActorState {
        HealthCheckActorState {
            cassini_connected: false,
            graph_connected: false,
            expects_graph: false,
            expiry_threshold_secs: 60,
            rejuvenation_threshold_secs: 300,
            tick_secs: 30,
            cert_paths,
            dep_cert_endpoints: vec![],
            rejuvenation_phase: RejuvenationPhase::Normal,
            graph_deadline_secs: 120,
            last_graph_success_secs: None,
            actor_start_secs: HealthState::now(),
        }
    }

    // -- connection and graph rules -----------------------------------------

    #[test]
    fn connections_healthy_matrix() {
        for (cassini, graph, expects_graph, healthy) in [
            (false, false, false, false),
            (true, false, false, true),
            (true, true, false, true),
            (false, true, true, false),
            (true, false, true, false),
            (true, true, true, true),
        ] {
            let mut s = state(vec![]);
            s.cassini_connected = cassini;
            s.graph_connected = graph;
            s.expects_graph = expects_graph;
            assert_eq!(
                s.connections_healthy(),
                healthy,
                "cassini={cassini} graph={graph} expects_graph={expects_graph}"
            );
        }
    }

    #[test]
    fn graph_deadline_is_ignored_when_no_graph_is_expected() {
        let mut s = state(vec![]);
        s.actor_start_secs = HealthState::now() - 10_000;
        assert!(s.check_graph_deadline().is_ok());
    }

    #[test]
    fn graph_deadline_holds_after_a_recent_success() {
        let mut s = state(vec![]);
        s.expects_graph = true;
        s.last_graph_success_secs = Some(HealthState::now() - 100);
        assert!(s.check_graph_deadline().is_ok());
    }

    #[test]
    fn graph_deadline_fires_after_the_graph_goes_dark() {
        let mut s = state(vec![]);
        s.expects_graph = true;
        s.last_graph_success_secs = Some(HealthState::now() - 121);
        let err = s.check_graph_deadline().unwrap_err();
        assert!(err.contains("graph unavailable"), "{err}");
    }

    #[test]
    fn graph_deadline_fires_when_the_graph_never_came_up() {
        let mut s = state(vec![]);
        s.expects_graph = true;
        s.actor_start_secs = HealthState::now() - 121;
        let err = s.check_graph_deadline().unwrap_err();
        assert!(err.contains("never became available"), "{err}");
    }

    #[test]
    fn graph_deadline_does_not_fire_inside_the_startup_window() {
        let mut s = state(vec![]);
        s.expects_graph = true;
        s.actor_start_secs = HealthState::now() - 30;
        assert!(s.check_graph_deadline().is_ok());
    }

    // -- dependency endpoint parsing ----------------------------------------

    #[test]
    fn dep_endpoint_parses_host_port_and_ttl() {
        let e = DepCertEndpoint::parse("cassini.polar.svc.cluster.local:8080:300").unwrap();
        assert_eq!(e.host, "cassini.polar.svc.cluster.local");
        assert_eq!(e.port, 8080);
        assert_eq!(e.min_ttl_secs, 300);
    }

    #[test]
    fn dep_endpoint_rejects_malformed_input() {
        for bad in [
            "",
            "host",
            "host:8080",
            "host:0:300",
            "host:notaport:300",
            "host:8080:-1",
            "host:8080:soon",
            "-bad.example:8080:300",
            "bad..example:8080:300",
            "bad_host:8080:300",
        ] {
            assert!(DepCertEndpoint::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    // -- the file format the probe reads --------------------------------------

    #[test]
    fn health_state_round_trips_through_json() {
        let json = serde_json::to_string(&HealthState::healthy(true, false)).unwrap();
        let back: HealthState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, HealthStatus::Healthy);
        assert!(back.cassini_connected);
        assert!(!back.graph_connected);
    }

    #[test]
    fn health_state_reads_files_written_before_cassini_connected_existed() {
        let old = r#"{"status":"healthy","timestamp":1,"graph_connected":true}"#;
        let state: HealthState = serde_json::from_str(old).unwrap();
        assert!(!state.cassini_connected);
    }

    #[test]
    fn health_status_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&HealthStatus::Unhealthy).unwrap(),
            "\"unhealthy\""
        );
    }

    // -- the whole actor ----------------------------------------------------
    //
    // These go through the real actor, real files and real (1 s) ticks, so they
    // keep passing unchanged across the refactor. The actor reads its settings
    // from the process environment, so they run one at a time.

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    const ALL_HEALTH_ENV: [&str; 8] = [
        POLAR_HEALTH_FILE_ENV,
        POLAR_HEALTH_CERTS_ENV,
        POLAR_HEALTH_EXPIRY_SECS_ENV,
        POLAR_HEALTH_STALE_SECS_ENV,
        POLAR_HEALTH_TICK_SECS_ENV,
        POLAR_HEALTH_DEP_ENDPOINTS_ENV,
        POLAR_HEALTH_REJUVENATION_SECS_ENV,
        POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV,
    ];

    /// Clears every health variable, applies `vars`, and clears them again on drop.
    struct EnvGuard;

    impl EnvGuard {
        fn set(vars: &[(&str, &str)]) -> Self {
            // SAFETY: callers hold ENV_LOCK, and no other test reads these.
            unsafe {
                for name in ALL_HEALTH_ENV {
                    std::env::remove_var(name);
                }
                for (name, value) in vars {
                    std::env::set_var(name, value);
                }
            }
            EnvGuard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `set`.
            unsafe {
                for name in ALL_HEALTH_ENV {
                    std::env::remove_var(name);
                }
            }
        }
    }

    fn args() -> HealthCheckArgs {
        HealthCheckArgs {
            expects_graph: false,
            rejuvenation_threshold_secs: 300,
            dep_cert_endpoints: vec![],
        }
    }

    fn read_state(path: &std::path::Path) -> Option<HealthState> {
        serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
    }

    /// Polls the health file until `want` accepts it, or panics after 8 s.
    async fn wait_for_state(
        path: &std::path::Path,
        what: &str,
        want: impl Fn(&HealthState) -> bool,
    ) -> HealthState {
        for _ in 0..80 {
            if let Some(state) = read_state(path)
                && want(&state)
            {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("health file never showed: {what}");
    }

    #[tokio::test]
    async fn actor_reports_healthy_then_unhealthy_when_cassini_drops() {
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 3600);
        let _env = EnvGuard::set(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            (POLAR_HEALTH_TICK_SECS_ENV, "1"),
        ]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();

        // Nothing is written until Cassini connects: the tick chain starts there.
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        wait_for_state(&file, "healthy with cassini connected", |s| {
            s.status == HealthStatus::Healthy && s.cassini_connected
        })
        .await;

        // A disconnect is written immediately, not at the next tick.
        actor.cast(HealthCheckMessage::CassiniDisconnected).unwrap();
        wait_for_state(&file, "unhealthy after the disconnect", |s| {
            s.status == HealthStatus::Unhealthy && !s.cassini_connected
        })
        .await;

        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn actor_fails_and_writes_unhealthy_when_its_cert_is_inside_the_hard_threshold() {
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 20);
        let _env = EnvGuard::set(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            (POLAR_HEALTH_TICK_SECS_ENV, "1"),
        ]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();

        wait_for_state(&file, "unhealthy after the cert deadline", |s| {
            s.status == HealthStatus::Unhealthy
        })
        .await;
        // Today a hard deadline is the actor failing itself.
        handle.await.unwrap();
        assert_eq!(actor.get_status(), ActorStatus::Stopped);
    }

    #[tokio::test]
    async fn actor_refuses_to_start_when_rejuvenation_is_not_above_expiry() {
        let _lock = ENV_LOCK.lock().await;
        let _env = EnvGuard::set(&[
            (POLAR_HEALTH_EXPIRY_SECS_ENV, "60"),
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "60"),
        ]);
        assert!(Actor::spawn(None, HealthCheckActor, args()).await.is_err());
    }

    #[tokio::test]
    async fn actor_refuses_to_start_on_a_malformed_dependency_endpoint() {
        let _lock = ENV_LOCK.lock().await;
        let _env = EnvGuard::set(&[(POLAR_HEALTH_DEP_ENDPOINTS_ENV, "not-an-endpoint")]);
        assert!(Actor::spawn(None, HealthCheckActor, args()).await.is_err());
    }

    #[tokio::test]
    async fn actor_starts_when_dependency_endpoints_come_from_both_env_and_args() {
        let _lock = ENV_LOCK.lock().await;
        let _env = EnvGuard::set(&[(POLAR_HEALTH_DEP_ENDPOINTS_ENV, "a.example:443:300")]);
        let mut a = args();
        a.dep_cert_endpoints = vec![DepCertEndpoint::parse("b.example:443:300").unwrap()];
        // Starts fine; the precedence itself is pinned in the config tests once
        // the settings live in HealthConfig.
        let (actor, handle) = Actor::spawn(None, HealthCheckActor, a).await.unwrap();
        actor.stop(None);
        handle.await.unwrap();
    }
}
