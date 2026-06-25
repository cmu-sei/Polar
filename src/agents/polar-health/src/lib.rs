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
//!     certs to be healthy, applies jitter, then signals the supervisor via an
//!     OutputPort<()> to prepare for shutdown.
//!   - On receiving ShutdownAck from supervisor, stops itself cleanly.
//!     Supervisor handles ActorTerminated for the healthcheck actor by exiting.

use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use x509_parser::prelude::*;

// ---------------------------------------------------------------------------
// Environment variable constants
// ---------------------------------------------------------------------------

pub const POLAR_HEALTH_FILE_ENV: &str = "POLAR_HEALTH_FILE";
pub const POLAR_HEALTH_FILE_DEFAULT: &str = "/tmp/polar-health.json";
pub const POLAR_HEALTH_CERTS_ENV: &str = "POLAR_HEALTH_CERTS";
pub const POLAR_HEALTH_EXPIRY_SECS_ENV: &str = "POLAR_HEALTH_EXPIRY_SECS";
pub const POLAR_HEALTH_STALE_SECS_ENV: &str = "POLAR_HEALTH_STALE_SECS";
pub const POLAR_HEALTH_TICK_SECS_ENV: &str = "POLAR_HEALTH_TICK_SECS";

pub const DEFAULT_EXPIRY_THRESHOLD_SECS: i64 = 60;
pub const DEFAULT_TICK_SECS: u64 = 30;
pub const DEFAULT_REJUVENATION_THRESHOLD_SECS: i64 = 300;

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
    /// PrepareShutdown signalled via OutputPort — waiting for ShutdownAck.
    WaitingForAck,
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
    /// Graph database connection established.
    GraphConnected,
    /// Graph database connection lost.
    GraphDisconnected,
    /// Internal periodic tick — do not send externally.
    Tick,
    /// Sent by the supervisor after it has handled PrepareShutdown.
    ShutdownAck,
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

    /// OutputPort fired when the HealthCheckActor decides it is time
    /// for the supervisor to prepare for shutdown. The supervisor
    /// subscribes to this port at spawn time.
    pub prepare_shutdown_port: Arc<OutputPort<()>>,
}

impl std::fmt::Debug for HealthCheckArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthCheckArgs")
            .field("expects_graph", &self.expects_graph)
            .field("rejuvenation_threshold_secs", &self.rejuvenation_threshold_secs)
            .field("dep_cert_endpoints", &self.dep_cert_endpoints)
            .field("prepare_shutdown_port", &"<OutputPort<()>>")
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
    pub prepare_shutdown_port: Arc<OutputPort<()>>,
}

impl HealthCheckActorState {
    /// Check all own cert paths against the hard expiry threshold.
    /// Returns `Err` with a description if any cert is missing, unparseable,
    /// or within the expiry threshold.
    fn check_certs(&self) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system clock error: {e}"))?
            .as_secs() as i64;

        if self.cert_paths.is_empty() {
            tracing::warn!("healthcheck: no cert paths configured, skipping cert check");
            return Ok(());
        }

        for path in &self.cert_paths {
            let data = fs::read(path)
                .map_err(|e| format!("failed to read cert '{path}': {e}"))?;

            let (_, pem) = parse_x509_pem(&data)
                .map_err(|e| format!("failed to parse PEM '{path}': {e}"))?;

            let cert = pem
                .parse_x509()
                .map_err(|e| format!("failed to parse x509 in '{path}': {e}"))?;

            let not_after = cert.validity().not_after.timestamp();
            let secs_remaining = not_after.checked_sub(now).ok_or_else(|| {
                format!("certificate timestamp arithmetic overflow for '{path}'")
            })?;

            if secs_remaining < self.expiry_threshold_secs {
                return Err(format!(
                    "cert '{path}' expires in {secs_remaining}s (hard threshold: {}s)",
                    self.expiry_threshold_secs
                ));
            }

            tracing::debug!("cert '{path}' valid for {secs_remaining}s");
        }

        Ok(())
    }

    /// Returns the minimum seconds remaining across all own certs,
    /// or `None` if no cert paths are configured or none are parseable.
    fn min_cert_ttl(&self) -> Option<i64> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;

        self.cert_paths
            .iter()
            .filter_map(|path| {
                let data = fs::read(path).ok()?;
                let (_, pem) = parse_x509_pem(&data).ok()?;
                let cert = pem.parse_x509().ok()?;
                let not_after = cert.validity().not_after.timestamp();
                not_after.checked_sub(now)
            })
            .reduce(i64::min)
    }

    /// Returns `true` if all required connections are established.
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
    fn deps_healthy(&self) -> bool {
        if self.dep_cert_endpoints.is_empty() {
            return true;
        }

        let mut all_healthy = true;
        for dep in &self.dep_cert_endpoints {
            match dep.check_ttl() {
                Ok(remaining) => {
                    tracing::debug!(
                        "dep cert {}:{} healthy, {remaining}s remaining",
                        dep.host,
                        dep.port
                    );
                }
                Err(reason) => {
                    tracing::warn!("healthcheck: dep cert unhealthy: {reason}");
                    all_healthy = false;
                }
            }
        }
        all_healthy
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
        let cert_paths = std::env::var(POLAR_HEALTH_CERTS_ENV)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        let expiry_threshold_secs = std::env::var(POLAR_HEALTH_EXPIRY_SECS_ENV)
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(DEFAULT_EXPIRY_THRESHOLD_SECS);

        let tick_secs = std::env::var(POLAR_HEALTH_TICK_SECS_ENV)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TICK_SECS);

        if expiry_threshold_secs <= 0 {
            return Err(ActorProcessingErr::from(format!(
                "POLAR_HEALTH_EXPIRY_SECS must be positive, got {expiry_threshold_secs}"
            )));
        }

        if args.rejuvenation_threshold_secs <= expiry_threshold_secs {
            return Err(ActorProcessingErr::from(format!(
                "rejuvenation_threshold_secs ({}) must be greater than expiry_threshold_secs ({})",
                args.rejuvenation_threshold_secs, expiry_threshold_secs
            )));
        }

        Ok(HealthCheckActorState {
            cassini_connected: false,
            graph_connected: false,
            expects_graph: args.expects_graph,
            expiry_threshold_secs,
            rejuvenation_threshold_secs: args.rejuvenation_threshold_secs,
            tick_secs,
            cert_paths,
            dep_cert_endpoints: args.dep_cert_endpoints,
            rejuvenation_phase: RejuvenationPhase::Normal,
            prepare_shutdown_port: args.prepare_shutdown_port,
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
                tracing::debug!("healthcheck: graph connected");
            }

            HealthCheckMessage::GraphDisconnected => {
                state.graph_connected = false;
                tracing::warn!("healthcheck: graph disconnected");
                if state.expects_graph {
                    HealthState::unhealthy(state.cassini_connected, false).write();
                }
                // Don't fail — allow reconnection to recover.
            }

            HealthCheckMessage::ShutdownAck => {
                tracing::info!(
                    "healthcheck: received ShutdownAck from supervisor, stopping cleanly"
                );
                myself.stop(Some("rejuvenation".to_string()));
            }

            HealthCheckMessage::Tick => {
                // ----------------------------------------------------------------
                // Hard deadline check — own certs at or within expiry threshold.
                // Always checked regardless of rejuvenation phase.
                // ----------------------------------------------------------------
                if let Err(reason) = state.check_certs() {
                    tracing::error!("healthcheck: cert check failed (hard deadline): {reason}");
                    HealthState::unhealthy(
                        state.cassini_connected,
                        state.graph_connected,
                    )
                    .write();
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
                            // Don't fail — reschedule tick and wait for reconnection.
                            let _ = myself
                                .send_after(
                                    Duration::from_secs(state.tick_secs),
                                    || HealthCheckMessage::Tick,
                                )
                                .await;
                            return Ok(());
                        }

                        // Check whether we've entered the rejuvenation window
                        match state.min_cert_ttl() {
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
                        if state.deps_healthy() {
                            // Choose jitter: 0–9 seconds (uniform)
                            let jitter_secs = u64::from(rand::random::<u8>()) % 10;
                            let fire_at_secs = HealthState::now()
                                .checked_add(jitter_secs)
                                .unwrap_or(HealthState::now());

                            tracing::info!(
                                "healthcheck: deps healthy, jitter {jitter_secs}s, \
                                 will signal shutdown at t={fire_at_secs}"
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
                                "healthcheck: jitter elapsed, signalling PrepareShutdown"
                            );
                            state.prepare_shutdown_port.send(());
                            state.rejuvenation_phase = RejuvenationPhase::WaitingForAck;
                            // Reschedule tick only to catch hard deadline while waiting
                            let _ = myself
                                .send_after(
                                    Duration::from_secs(state.tick_secs),
                                    || HealthCheckMessage::Tick,
                                )
                                .await;
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

                    // ------------------------------------------------------------
                    // Rejuvenation: waiting for supervisor ShutdownAck
                    // ------------------------------------------------------------
                    RejuvenationPhase::WaitingForAck => {
                        // Nothing to do except reschedule tick to catch hard deadline.
                        tracing::debug!(
                            "healthcheck: waiting for ShutdownAck from supervisor"
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

        Ok(())
    }
}
