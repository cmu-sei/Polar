/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

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
// Reading the process environment goes through polar-env (see clippy.toml).
#![deny(clippy::disallowed_methods)]

use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use x509_parser::prelude::*;

mod certs;
mod clock;
pub mod config;
mod machine;
pub mod probe;
#[cfg(test)]
mod simulation;
#[cfg(test)]
mod test_support;
pub mod vars;

pub use config::HealthConfig;
use machine::{HealthMachine, Next, TickOutcome, TickStep};

// ---------------------------------------------------------------------------
// Environment variables
// ---------------------------------------------------------------------------
//
// Declared once, in `vars`. The names and defaults below are derived from those
// declarations, not repeated.

pub use vars::{
    DEFAULT_CASSINI_DEADLINE_SECS, DEFAULT_EXPIRY_THRESHOLD_SECS, DEFAULT_GRAPH_DEADLINE_SECS,
    DEFAULT_REJUVENATION_THRESHOLD_SECS, DEFAULT_STALE_THRESHOLD_SECS, DEFAULT_TICK_SECS,
    POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV, POLAR_HEALTH_CERTS_ENV, POLAR_HEALTH_DEP_ENDPOINTS_ENV,
    POLAR_HEALTH_EXPIRY_SECS_ENV, POLAR_HEALTH_FILE_DEFAULT, POLAR_HEALTH_FILE_ENV,
    POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV, POLAR_HEALTH_REJUVENATION_SECS_ENV,
    POLAR_HEALTH_STALE_SECS_ENV, POLAR_HEALTH_TICK_SECS_ENV,
};

/// Jitter window width in seconds. Must comfortably exceed DEFAULT_TICK_SECS
/// (and any agent's configured tick_secs) or jitter effectively rounds up to
/// the next tick boundary and every agent in WaitingForDeps at the same time
/// fires within one tick of each other -- a thundering herd on cert-issuer.
/// Not an environment variable: it is how the actor behaves, and the room a
/// rejuvenation needs depends on it.
pub const DEFAULT_JITTER_WINDOW_SECS: u64 = 180;

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
// HealthPhase — informational, for whoever reads the health file
// ---------------------------------------------------------------------------

/// Which stage of its life the agent is in, written into the health file so an
/// operator can tell "rejuvenating" from "unhealthy". The liveness probe ignores
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthPhase {
    #[default]
    Normal,
    WaitingForDeps,
    Jittering,
    /// The agent has been told to end (a verdict was given) and is draining. The
    /// file keeps being refreshed so the probe does not kill it mid-drain.
    ShuttingDown,
}

impl std::fmt::Display for HealthPhase {
    /// The same names the health file uses.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            HealthPhase::Normal => "normal",
            HealthPhase::WaitingForDeps => "waiting_for_deps",
            HealthPhase::Jittering => "jittering",
            HealthPhase::ShuttingDown => "shutting_down",
        })
    }
}

impl From<&RejuvenationPhase> for HealthPhase {
    fn from(phase: &RejuvenationPhase) -> Self {
        match phase {
            RejuvenationPhase::Normal => HealthPhase::Normal,
            RejuvenationPhase::WaitingForDeps => HealthPhase::WaitingForDeps,
            RejuvenationPhase::Jittering { .. } => HealthPhase::Jittering,
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
    /// Informational. Files written before this field existed read as `Normal`.
    #[serde(default)]
    pub phase: HealthPhase,
}

impl HealthState {
    /// A state with an explicit timestamp, for callers that do not read the clock.
    pub fn at(
        status: HealthStatus,
        timestamp: u64,
        cassini_connected: bool,
        graph_connected: bool,
    ) -> Self {
        Self {
            status,
            timestamp,
            cassini_connected,
            graph_connected,
            phase: HealthPhase::Normal,
        }
    }

    pub fn with_phase(mut self, phase: HealthPhase) -> Self {
        self.phase = phase;
        self
    }

    pub fn healthy(cassini_connected: bool, graph_connected: bool) -> Self {
        Self {
            status: HealthStatus::Healthy,
            timestamp: Self::now(),
            cassini_connected,
            graph_connected,
            phase: HealthPhase::Normal,
        }
    }

    pub fn unhealthy(cassini_connected: bool, graph_connected: bool) -> Self {
        Self {
            status: HealthStatus::Unhealthy,
            timestamp: Self::now(),
            cassini_connected,
            graph_connected,
            phase: HealthPhase::Normal,
        }
    }

    /// Write the health state atomically to `path`.
    /// Uses a tmp file + rename to avoid partial reads by the healthcheck binary.
    pub fn write_to(&self, path: &str) -> Result<(), String> {
        let json = serde_json::to_string(self)
            .map_err(|e| format!("failed to serialize health state: {e}"))?;

        let tmp = format!("{path}.tmp");
        fs::write(&tmp, &json)
            .map_err(|e| format!("failed to write health state tmp file {tmp}: {e}"))?;
        fs::rename(&tmp, path)
            .map_err(|e| format!("failed to rename health state tmp file to {path}: {e}"))
    }

    pub fn now() -> u64 {
        crate::clock::now_secs()
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

/// Time limits for checking a dependency's certificate.
#[derive(Debug, Clone, Copy)]
pub struct DepCheckLimits {
    /// Opening the TCP connection. Without this the operating system decides,
    /// which for a host that drops packets is about two minutes.
    pub connect: Duration,
    /// Each read and write during the TLS handshake.
    pub io: Duration,
    /// The whole check, enforced by the health actor around it.
    pub overall: Duration,
}

impl Default for DepCheckLimits {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            io: Duration::from_secs(5),
            overall: Duration::from_secs(10),
        }
    }
}

/// Connects to the first address of `addr` that answers within `timeout` each.
fn connect_within(addr: &str, timeout: Duration) -> Result<TcpStream, String> {
    use std::net::ToSocketAddrs;

    let addrs = addr
        .to_socket_addrs()
        .map_err(|e| format!("failed to resolve {addr}: {e}"))?;
    let mut last = format!("no addresses found for {addr}");
    for candidate in addrs {
        match TcpStream::connect_timeout(&candidate, timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = format!("failed to connect to {addr}: {e}"),
        }
    }
    Err(last)
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
        self.check_ttl_within(&DepCheckLimits::default())
    }

    /// As [`check_ttl`](Self::check_ttl), with chosen time limits. The connect and
    /// each read and write are bounded, so a host that drops packets or a server
    /// that goes quiet cannot hold the caller for long.
    pub fn check_ttl_within(&self, limits: &DepCheckLimits) -> Result<i64, String> {
        use rustls::ClientConfig;
        use rustls::pki_types::ServerName;
        use std::io::Write;

        let addr = format!("{}:{}", self.host, self.port);

        let stream = connect_within(&addr, limits.connect)?;

        stream
            .set_read_timeout(Some(limits.io))
            .map_err(|e| format!("failed to set read timeout on {addr}: {e}"))?;
        stream
            .set_write_timeout(Some(limits.io))
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

        let remaining = not_after
            .checked_sub(now)
            .ok_or_else(|| format!("certificate timestamp arithmetic overflow for {addr}"))?;

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
// HealthVerdict
// ---------------------------------------------------------------------------

/// The health actor's decision that the agent must end.
///
/// Sent once, to whoever the actor was given in `HealthCheckArgs::verdict_to`.
/// The actor reports its decision with a message and does not die to make a
/// point, so it can keep the health file fresh while the agent shuts down.
#[derive(Debug, Clone, PartialEq)]
pub enum HealthVerdict {
    /// The certificates are due for rejuvenation and the dependencies are ready.
    /// Not a fault: the agent should end in an orderly way and be replaced.
    Rejuvenate,
    /// A hard deadline passed or the actor can no longer do its job.
    Unhealthy(String),
}

impl HealthVerdict {
    /// A sentence for logs and for the reason an agent exits with.
    pub fn reason(&self) -> String {
        match self {
            HealthVerdict::Rejuvenate => {
                "rejuvenation: certificate rejuvenation threshold reached".to_string()
            }
            HealthVerdict::Unhealthy(reason) => reason.clone(),
        }
    }
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
    /// Internal: the dependency check a tick started has finished — do not send
    /// externally. A tick that needs the dependencies hands the network check to
    /// a task and carries on when this arrives, so the actor keeps handling
    /// messages in the meantime.
    DepsChecked(bool),
}

// ---------------------------------------------------------------------------
// HealthCheckArgs
// ---------------------------------------------------------------------------

/// What the health actor is started with.
///
/// Built with [`new`](Self::new) and the `with_` methods, never as a literal, so a field
/// added later does not touch every agent that starts one (the struct is
/// `non_exhaustive` to keep it so).
#[derive(Clone)]
#[non_exhaustive]
pub struct HealthCheckArgs {
    /// Does this agent connect to a graph database?
    pub expects_graph: bool,

    /// Seconds before own cert expiry to enter rejuvenation mode.
    pub rejuvenation_threshold_secs: i64,

    /// Dependency TLS endpoints to check during rejuvenation.
    /// Parsed from `"host:port:min_ttl_seconds"`.
    pub dep_cert_endpoints: Vec<DepCertEndpoint>,

    /// Where to send the verdict that the agent must end.
    ///
    /// With a recipient, the actor sends one [`HealthVerdict`] and stays alive,
    /// refreshing the health file while the agent shuts down. With `None` it
    /// ends itself instead (fails on a hard deadline, stops for rejuvenation),
    /// which is what agents that do not use the builder rely on.
    pub verdict_to: Option<Arc<OutputPort<HealthVerdict>>>,

    /// The settings, already parsed. The builder supplies them from the one snapshot of
    /// the environment it took, so the actor never reads the process.
    ///
    /// `None` is for agents not yet on the builder: the actor then reads the process
    /// environment itself, as it always did. That path goes when the last of them moves.
    pub config: Option<HealthConfig>,
}

impl HealthCheckArgs {
    /// The three things an agent knows about itself; everything else is configuration.
    pub fn new(
        expects_graph: bool,
        rejuvenation_threshold_secs: i64,
        dep_cert_endpoints: Vec<DepCertEndpoint>,
    ) -> Self {
        Self {
            expects_graph,
            rejuvenation_threshold_secs,
            dep_cert_endpoints,
            verdict_to: None,
            config: None,
        }
    }

    /// Send the verdict here and keep the actor alive, instead of ending it.
    pub fn with_verdict_to(mut self, port: Arc<OutputPort<HealthVerdict>>) -> Self {
        self.verdict_to = Some(port);
        self
    }

    /// Start on these settings, without reading the process environment.
    pub fn with_config(mut self, config: HealthConfig) -> Self {
        self.config = Some(config);
        self
    }
}

impl std::fmt::Debug for HealthCheckArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthCheckArgs")
            .field("expects_graph", &self.expects_graph)
            .field(
                "rejuvenation_threshold_secs",
                &self.rejuvenation_threshold_secs,
            )
            .field("dep_cert_endpoints", &self.dep_cert_endpoints)
            .field("verdict_to", &self.verdict_to.is_some())
            .field("config", &self.config)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// HealthCheckActor
// ---------------------------------------------------------------------------

pub struct HealthCheckActorState {
    config: HealthConfig,
    machine: HealthMachine,
    verdict_to: Option<Arc<OutputPort<HealthVerdict>>>,
    /// A dependency check is running on behalf of the current tick. The tick
    /// chain is paused until it reports back.
    awaiting_deps: bool,
}

/// Whether every dependency endpoint has a healthy certificate TTL.
async fn deps_healthy(endpoints: &[DepCertEndpoint]) -> bool {
    deps_healthy_within(endpoints, &DepCheckLimits::default()).await
}

/// As [`deps_healthy`], with chosen limits.
///
/// The endpoints are checked at the same time, each on a blocking thread (the
/// check does blocking network and TLS I/O) and each held to `limits.overall`,
/// so the whole check takes no longer than the slowest single one, never the sum.
/// A warning is logged for each that fails, so all failures show in one tick.
async fn deps_healthy_within(endpoints: &[DepCertEndpoint], limits: &DepCheckLimits) -> bool {
    if endpoints.is_empty() {
        return true;
    }

    let mut checks = tokio::task::JoinSet::new();
    for dep in endpoints {
        let dep = dep.clone();
        let limits = *limits;
        checks.spawn(async move {
            let label = format!("{}:{}", dep.host, dep.port);
            let overall = limits.overall;
            let check = tokio::task::spawn_blocking(move || dep.check_ttl_within(&limits));
            match tokio::time::timeout(overall, check).await {
                Ok(Ok(Ok(remaining))) => {
                    tracing::debug!("dep cert {label} healthy, {remaining}s remaining");
                    true
                }
                Ok(Ok(Err(reason))) => {
                    tracing::warn!("healthcheck: dep cert unhealthy: {reason}");
                    false
                }
                Ok(Err(join_err)) => {
                    tracing::warn!(
                        "healthcheck: dep cert check of {label} panicked or was cancelled: {join_err}"
                    );
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        "healthcheck: dep cert check of {label} did not finish within {overall:?}"
                    );
                    false
                }
            }
        });
    }

    let mut all_healthy = true;
    while let Some(result) = checks.join_next().await {
        all_healthy &= result.unwrap_or(false);
    }
    all_healthy
}

/// Schedules the next tick, and returns at once.
///
/// `send_after` hands back a handle that resolves only after the whole period has
/// passed, so awaiting it would hold this actor's handler, and every message
/// queued behind it, for `tick_secs`. Dropping the handle detaches the timer.
/// There is only ever one tick chain: it starts in `pre_start`, and each tick
/// schedules the next one only when it has finished.
fn schedule_tick(myself: &ActorRef<HealthCheckMessage>, tick_secs: u64) {
    drop(myself.send_after(Duration::from_secs(tick_secs), || HealthCheckMessage::Tick));
}

/// Writes the health file and tells the machine how it went.
///
/// A failed write is logged. The file is the liveness probe's only evidence, so
/// a file that cannot be written will go stale and the kubelet will kill the
/// pod without a word of explanation; after enough failures in a row the actor
/// gives a verdict instead, with the reason. Returns that verdict, if so.
fn persist(state: &mut HealthCheckActorState, health: &HealthState) -> Option<HealthVerdict> {
    let result = health.write_to(&state.config.file_path);
    if let Err(reason) = &result {
        tracing::warn!("healthcheck: {reason}");
    }
    let reason = state.machine.record_write(result)?;
    tracing::error!("healthcheck: {reason}");
    let verdict = HealthVerdict::Unhealthy(reason);
    state.machine.conclude(&verdict);
    Some(verdict)
}

/// Hands the verdict over. Returns whether the actor goes on (and so keeps
/// ticking): with a recipient it does, so it can keep the file fresh through the
/// shutdown; without one it ends itself.
fn deliver(
    myself: &ActorRef<HealthCheckMessage>,
    state: &HealthCheckActorState,
    verdict: HealthVerdict,
) -> Result<bool, ActorProcessingErr> {
    match &state.verdict_to {
        Some(port) => {
            tracing::warn!("healthcheck: verdict: {}", verdict.reason());
            port.send(verdict);
            Ok(true)
        }
        None => match verdict {
            HealthVerdict::Unhealthy(reason) => Err(ActorProcessingErr::from(reason)),
            HealthVerdict::Rejuvenate => {
                // Ordinary Ractor supervision is the mechanism: the supervisor sees
                // a genuine SupervisionEvent::ActorTerminated carrying this reason.
                myself.stop(Some(verdict.reason()));
                Ok(false)
            }
        },
    }
}

/// Carries out what the machine decided after a tick.
async fn finish_tick(
    myself: &ActorRef<HealthCheckMessage>,
    state: &mut HealthCheckActorState,
    outcome: TickOutcome,
) -> Result<(), ActorProcessingErr> {
    let write_verdict = outcome.write.as_ref().and_then(|h| persist(state, h));
    // A verdict from the machine outranks one about a failed write.
    let verdict = match outcome.next {
        Next::Verdict(v) => Some(v),
        Next::Reschedule => write_verdict,
    };
    let goes_on = match verdict {
        Some(v) => deliver(myself, state, v)?,
        None => true,
    };
    if goes_on {
        schedule_tick(myself, state.config.tick_secs);
    }
    Ok(())
}

pub struct HealthCheckActor;

#[async_trait]
impl Actor for HealthCheckActor {
    type Msg = HealthCheckMessage;
    type State = HealthCheckActorState;
    type Arguments = HealthCheckArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: HealthCheckArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let verdict_to = args.verdict_to.clone();
        let config = match args.config {
            Some(config) => {
                config.validate().map_err(ActorProcessingErr::from)?;
                config
            }
            None => HealthConfig::load(args).map_err(ActorProcessingErr::from)?,
        };
        let machine = HealthMachine::new(&config, HealthState::now());
        // The one tick chain. It runs from startup, so the hard deadlines and the
        // health file do not wait for Cassini to connect.
        schedule_tick(&myself, config.tick_secs);
        Ok(HealthCheckActorState {
            config,
            machine,
            verdict_to,
            awaiting_deps: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            HealthCheckMessage::Tick => {
                let now = HealthState::now();
                let certs = certs::read(
                    &state.config.cert_paths,
                    state.config.expiry_threshold_secs,
                    now as i64,
                );
                match state.machine.tick(now, &certs) {
                    TickStep::Done(outcome) => finish_tick(&myself, state, outcome).await?,
                    TickStep::NeedDeps => {
                        // The network check runs in a task. The tick chain pauses
                        // here and resumes when DepsChecked arrives, and the actor
                        // handles other messages in the meantime.
                        state.awaiting_deps = true;
                        let endpoints = state.config.dep_cert_endpoints.clone();
                        let me = myself.clone();
                        tokio::spawn(async move {
                            let healthy = deps_healthy(&endpoints).await;
                            let _ = me.cast(HealthCheckMessage::DepsChecked(healthy));
                        });
                    }
                }
            }

            HealthCheckMessage::DepsChecked(healthy) => {
                if !std::mem::take(&mut state.awaiting_deps) {
                    tracing::warn!("healthcheck: ignoring a dependency result nobody asked for");
                    return Ok(());
                }
                let jitter = machine::jitter_secs(rand::random::<u32>());
                let outcome = state
                    .machine
                    .deps_checked(HealthState::now(), healthy, jitter);
                finish_tick(&myself, state, outcome).await?;
            }

            message => {
                if let Some(write) = state.machine.on_message(&message, HealthState::now())
                    && let Some(verdict) = persist(state, &write)
                {
                    // The tick chain is already running; this only delivers.
                    deliver(&myself, state, verdict)?;
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
    fn health_state_reads_files_written_before_the_phase_field_existed() {
        let old =
            r#"{"status":"healthy","timestamp":1,"cassini_connected":true,"graph_connected":true}"#;
        let state: HealthState = serde_json::from_str(old).unwrap();
        assert_eq!(state.phase, HealthPhase::Normal);
    }

    #[test]
    fn the_phase_is_written_in_snake_case() {
        let state = HealthState::healthy(true, true).with_phase(HealthPhase::WaitingForDeps);
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""phase":"waiting_for_deps""#), "{json}");
        let back: HealthState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.phase, HealthPhase::WaitingForDeps);
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
    // These go through the real actor, real files and real (1 s) ticks. The actor is
    // handed its settings parsed, as the builder hands them, and never reads the process
    // environment, so they can run side by side.

    /// The settings the builder would hand over for these variables.
    fn health_config(vars: &[(&str, &str)]) -> HealthConfig {
        let map: std::collections::HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        HealthConfig::from_env(HealthCheckArgs::new(false, 300, vec![]), &map).unwrap()
    }

    fn args() -> HealthCheckArgs {
        HealthCheckArgs::new(false, 300, vec![]).with_config(health_config(&[]))
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
        wait_for_state_within(path, what, Duration::from_secs(8), want).await
    }

    /// As `wait_for_state`, with a chosen budget. Under paused time the budget
    /// is virtual, so a long one costs nothing.
    async fn wait_for_state_within(
        path: &std::path::Path,
        what: &str,
        budget: Duration,
        want: impl Fn(&HealthState) -> bool,
    ) -> HealthState {
        let polls = budget.as_millis() / 100;
        for _ in 0..polls {
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
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 3600);
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            (POLAR_HEALTH_TICK_SECS_ENV, "1"),
        ]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();

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
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 20);
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            (POLAR_HEALTH_TICK_SECS_ENV, "1"),
        ]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
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
    async fn actor_refuses_to_start_on_settings_that_contradict_each_other() {
        // The parsing and the rules are tested on HealthConfig. What is pinned here is
        // that an actor handed settings that were built some other way (the fields are
        // public) still will not run on a state its own startup check refuses.
        let mut config = health_config(&[]);
        config.rejuvenation_threshold_secs = config.expiry_threshold_secs;
        let refused = Actor::spawn(None, HealthCheckActor, args().with_config(config)).await;
        assert!(refused.is_err());
    }

    // -- the tick chain ---------------------------------------------------------
    //
    // Paused time: the 30 s default tick costs nothing, and the clock only moves
    // when every task is idle, so "how long did this take" is exact.

    #[tokio::test(start_paused = true)]
    async fn ticks_run_from_startup_without_waiting_for_cassini() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let config = health_config(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
        assert!(
            read_state(&file).is_none(),
            "nothing is written before a tick"
        );

        // Cassini never connects, yet the first tick still reports in.
        let state = wait_for_state_within(
            &file,
            "a file from the first tick with Cassini never connected",
            Duration::from_secs(60),
            |_| true,
        )
        .await;
        assert_eq!(state.status, HealthStatus::Unhealthy);
        assert!(!state.cassini_connected);

        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_disconnect_is_written_without_waiting_out_a_tick_period() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let config = health_config(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        actor.cast(HealthCheckMessage::CassiniDisconnected).unwrap();

        let started = tokio::time::Instant::now();
        wait_for_state_within(
            &file,
            "unhealthy after the disconnect",
            Duration::from_secs(60),
            |s| s.status == HealthStatus::Unhealthy && !s.cassini_connected,
        )
        .await;
        // The handler used to sleep out the whole 30 s tick period inside the
        // Cassini connected message, so the disconnect queued behind it.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the disconnect took {:?} to be written",
            started.elapsed()
        );

        actor.stop(None);
        handle.await.unwrap();
    }

    // -- a health file that cannot be written -------------------------------------

    #[tokio::test(start_paused = true)]
    async fn an_actor_that_cannot_write_its_file_reports_itself_unhealthy() {
        let dir = tempfile::tempdir().unwrap();
        // The directory does not exist, so every write fails.
        let file = dir.path().join("no-such-dir").join("health.json");
        let config = health_config(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();

        // Three failed ticks at 30 s, so well inside five periods.
        let ended = tokio::time::timeout(Duration::from_secs(150), handle).await;
        assert!(
            ended.is_ok(),
            "the actor was still running after 150 s of failed writes"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn occasional_write_failures_do_not_end_the_actor() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let config = health_config(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        // A directory where the temp file should go makes that write fail once.
        let tmp = format!("{}.tmp", file.display());
        fs::create_dir(&tmp).unwrap();
        tokio::time::sleep(Duration::from_secs(45)).await;
        fs::remove_dir(&tmp).unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;

        assert_eq!(actor.get_status(), ActorStatus::Running);
        assert!(read_state(&file).is_some(), "writing recovered");
        actor.stop(None);
        handle.await.unwrap();
    }

    // -- verdicts ------------------------------------------------------------------

    /// Collects the verdicts an actor sends, for the tests to read.
    struct Collector;

    #[async_trait]
    impl Actor for Collector {
        type Msg = HealthVerdict;
        type State = Arc<std::sync::Mutex<Vec<HealthVerdict>>>;
        type Arguments = Self::State;

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            seen: Self::State,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(seen)
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            verdict: Self::Msg,
            seen: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            seen.lock().unwrap().push(verdict);
            Ok(())
        }
    }

    /// Args whose verdicts go to a collector, and the list it fills.
    async fn args_with_verdicts(
        config: HealthConfig,
    ) -> (
        HealthCheckArgs,
        Arc<std::sync::Mutex<Vec<HealthVerdict>>>,
        ActorRef<HealthVerdict>,
    ) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (collector, _) = Actor::spawn(None, Collector, seen.clone()).await.unwrap();
        let port = Arc::new(OutputPort::default());
        port.subscribe(collector.clone(), Some);
        let a = args().with_config(config).with_verdict_to(port);
        (a, seen, collector)
    }

    #[tokio::test(start_paused = true)]
    async fn a_hard_deadline_becomes_a_verdict_and_the_actor_stays_alive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 20);
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
        ]);

        let (a, seen, _collector) = args_with_verdicts(config).await;
        let (actor, handle) = Actor::spawn(None, HealthCheckActor, a).await.unwrap();

        let first = wait_for_state_within(
            &file,
            "the file from the verdict tick",
            Duration::from_secs(60),
            |s| s.phase == HealthPhase::ShuttingDown,
        )
        .await;
        assert_eq!(first.status, HealthStatus::Unhealthy);

        // The actor must keep refreshing the file while the agent drains, or the
        // liveness probe kills the pod mid-shutdown.
        tokio::time::sleep(Duration::from_secs(100)).await;
        assert_eq!(actor.get_status(), ActorStatus::Running);
        let later = read_state(&file).unwrap();
        assert!(later.timestamp >= first.timestamp);
        assert_eq!(later.phase, HealthPhase::ShuttingDown);

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "exactly one verdict: {seen:?}");
        assert!(matches!(&seen[0], HealthVerdict::Unhealthy(r) if r.contains("hard threshold")));

        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejuvenation_becomes_a_verdict_and_the_actor_stays_alive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 3600);
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            // Rejuvenate as soon as the cert is under two hours from expiry.
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "7200"),
        ]);

        let (a, seen, _collector) = args_with_verdicts(config).await;
        let (actor, handle) = Actor::spawn(None, HealthCheckActor, a).await.unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();

        // Enters the window on tick 1, checks deps on tick 2, then at most 180 s
        // of jitter.
        let state = wait_for_state_within(
            &file,
            "a shutting_down file after the rejuvenation verdict",
            Duration::from_secs(400),
            |s| s.phase == HealthPhase::ShuttingDown,
        )
        .await;
        assert_eq!(state.status, HealthStatus::Healthy);

        tokio::time::sleep(Duration::from_secs(100)).await;
        assert_eq!(actor.get_status(), ActorStatus::Running);
        assert_eq!(*seen.lock().unwrap(), vec![HealthVerdict::Rejuvenate]);

        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_unwritable_file_becomes_a_verdict_and_the_actor_stays_alive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("no-such-dir").join("health.json");
        let config = health_config(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (a, seen, _collector) = args_with_verdicts(config).await;
        let (actor, handle) = Actor::spawn(None, HealthCheckActor, a).await.unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        tokio::time::sleep(Duration::from_secs(200)).await;

        assert_eq!(actor.get_status(), ActorStatus::Running);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "exactly one verdict: {seen:?}");
        assert!(matches!(&seen[0], HealthVerdict::Unhealthy(r) if r.contains("3 times in a row")));

        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_recipient_a_rejuvenation_still_stops_the_actor() {
        // Agents that have not moved to the builder pass no recipient, and rely on
        // the actor ending itself.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 3600);
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "7200"),
        ]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(600), handle).await;
        assert!(ended.is_ok(), "the actor never stopped for rejuvenation");
    }

    // -- the Cassini deadline ----------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn losing_cassini_for_too_long_becomes_a_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        // The deadline is set here, so the test says what it means whatever the default is.
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV, "60"),
        ]);

        let (a, seen, _collector) = args_with_verdicts(config).await;
        let (actor, handle) = Actor::spawn(None, HealthCheckActor, a).await.unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        tokio::time::sleep(Duration::from_secs(100)).await;
        assert!(seen.lock().unwrap().is_empty(), "connected, so no verdict");

        actor.cast(HealthCheckMessage::CassiniDisconnected).unwrap();
        // The deadline is 60 s, so by 100 s later there is exactly one verdict.
        tokio::time::sleep(Duration::from_secs(100)).await;

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "exactly one verdict: {seen:?}");
        assert!(
            matches!(&seen[0], HealthVerdict::Unhealthy(r) if r.contains("cassini unreachable"))
        );
        assert_eq!(read_state(&file).unwrap().phase, HealthPhase::ShuttingDown);
        assert_eq!(actor.get_status(), ActorStatus::Running);

        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_reconnect_inside_the_deadline_gives_no_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV, "100"),
        ]);

        let (a, seen, _collector) = args_with_verdicts(config).await;
        let (actor, handle) = Actor::spawn(None, HealthCheckActor, a).await.unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        for _ in 0..5 {
            actor.cast(HealthCheckMessage::CassiniDisconnected).unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
            actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }

        assert!(
            seen.lock().unwrap().is_empty(),
            "outages were each under the deadline"
        );
        assert_eq!(actor.get_status(), ActorStatus::Running);
        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_recipient_losing_cassini_fails_the_actor() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let config = health_config(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (_actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(200), handle).await;
        assert!(
            ended.is_ok(),
            "Cassini never came up and the actor never failed"
        );
    }

    // -- dependency endpoints ------------------------------------------------------
    //
    // These use real local sockets and real time, with short limits.

    use std::net::TcpListener;

    fn limits(io_ms: u64, overall_ms: u64) -> DepCheckLimits {
        DepCheckLimits {
            connect: Duration::from_millis(500),
            io: Duration::from_millis(io_ms),
            overall: Duration::from_millis(overall_ms),
        }
    }

    fn endpoint(port: u16, min_ttl_secs: i64) -> DepCertEndpoint {
        DepCertEndpoint::parse(&format!("127.0.0.1:{port}:{min_ttl_secs}")).unwrap()
    }

    /// A server that accepts connections and then says nothing.
    fn silent_server() -> (u16, TcpListener) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepter = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = accepter.accept() {
                held.push(stream); // never answered, never closed
            }
        });
        (port, listener)
    }

    /// A local TLS server whose certificate has `cert_ttl_secs` left. Serves
    /// `connections` handshakes, then stops.
    fn tls_server(cert_ttl_secs: i64, connections: usize) -> u16 {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

        let mut params =
            rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("params");
        params.not_before = ::time::OffsetDateTime::now_utc() - ::time::Duration::hours(1);
        params.not_after =
            ::time::OffsetDateTime::now_utc() + ::time::Duration::seconds(cert_ttl_secs);
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = params.self_signed(&key).expect("cert");
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(cert.der().to_vec())],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("server config"),
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..connections {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut conn = rustls::ServerConnection::new(config.clone()).unwrap();
                while conn.is_handshaking() {
                    if conn.complete_io(&mut stream).is_err() {
                        break;
                    }
                }
            }
        });
        port
    }

    #[test]
    fn a_healthy_dependency_reports_its_remaining_ttl() {
        let port = tls_server(3600, 1);
        let remaining = endpoint(port, 300)
            .check_ttl_within(&limits(2000, 4000))
            .expect("healthy");
        assert!(
            (3590..=3600).contains(&remaining),
            "remaining was {remaining}"
        );
    }

    #[test]
    fn a_dependency_with_too_little_ttl_is_unhealthy() {
        let port = tls_server(120, 1);
        let err = endpoint(port, 300)
            .check_ttl_within(&limits(2000, 4000))
            .unwrap_err();
        assert!(err.contains("expires in"), "{err}");
        assert!(err.contains("minimum required: 300s"), "{err}");
    }

    #[test]
    fn a_silent_server_is_given_up_on_at_the_io_limit() {
        let (port, _server) = silent_server();
        let started = std::time::Instant::now();
        let err = endpoint(port, 300)
            .check_ttl_within(&limits(300, 4000))
            .unwrap_err();
        assert!(err.contains("handshake"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_closed_port_fails_at_once() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let started = std::time::Instant::now();
        let err = endpoint(port, 300)
            .check_ttl_within(&limits(2000, 4000))
            .unwrap_err();
        assert!(err.contains("failed to connect"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn dependencies_are_checked_at_the_same_time() {
        // Three silent servers at 400 ms each: one after another would take 1.2 s.
        let servers: Vec<_> = (0..3).map(|_| silent_server()).collect();
        let endpoints: Vec<_> = servers.iter().map(|(p, _)| endpoint(*p, 300)).collect();

        let started = std::time::Instant::now();
        let healthy = deps_healthy_within(&endpoints, &limits(400, 4000)).await;
        let took = started.elapsed();

        assert!(!healthy);
        assert!(
            took < Duration::from_millis(1100),
            "took {took:?}, so not concurrent"
        );
    }

    #[tokio::test]
    async fn one_unhealthy_dependency_makes_them_all_unhealthy() {
        let good = tls_server(3600, 1);
        let bad = tls_server(60, 1);
        let endpoints = vec![endpoint(good, 300), endpoint(bad, 300)];
        assert!(!deps_healthy_within(&endpoints, &limits(2000, 4000)).await);

        let both_good = vec![
            endpoint(tls_server(3600, 1), 300),
            endpoint(tls_server(3600, 1), 300),
        ];
        assert!(deps_healthy_within(&both_good, &limits(2000, 4000)).await);
    }

    #[tokio::test]
    async fn no_dependencies_is_healthy() {
        assert!(deps_healthy_within(&[], &limits(100, 200)).await);
    }

    #[tokio::test]
    async fn a_dependency_check_never_outlasts_the_overall_budget() {
        // The io limit is longer than the overall one, so only the overall limit
        // can end this check early.
        let (port, _server) = silent_server();
        let started = std::time::Instant::now();
        let healthy = deps_healthy_within(&[endpoint(port, 300)], &limits(1500, 300)).await;
        assert!(!healthy);
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_dependency_keeps_the_agent_waiting_and_the_file_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 3600);
        let closed = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let config = health_config(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "7200"),
            (
                POLAR_HEALTH_DEP_ENDPOINTS_ENV,
                &format!("127.0.0.1:{closed}:300"),
            ),
        ]);

        let (a, seen, _collector) = args_with_verdicts(config).await;
        let (actor, handle) = Actor::spawn(None, HealthCheckActor, a).await.unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();

        // Several periods of an unhealthy dependency: still waiting, file kept
        // fresh by the ticks that follow each check, and no verdict.
        let mut stamps = vec![];
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_secs(40)).await;
            let state = read_state(&file).expect("a file");
            assert_eq!(state.phase, HealthPhase::WaitingForDeps);
            stamps.push(state.timestamp);
        }
        assert!(stamps.windows(2).all(|w| w[1] >= w[0]));
        assert!(
            stamps.first() < stamps.last(),
            "the file kept being refreshed: {stamps:?}"
        );
        assert!(seen.lock().unwrap().is_empty());

        actor.stop(None);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_stray_deps_checked_message_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let config = health_config(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args().with_config(config))
            .await
            .unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        actor.cast(HealthCheckMessage::DepsChecked(true)).unwrap();
        tokio::time::sleep(Duration::from_secs(100)).await;

        assert_eq!(actor.get_status(), ActorStatus::Running);
        assert_eq!(read_state(&file).unwrap().phase, HealthPhase::Normal);
        actor.stop(None);
        handle.await.unwrap();
    }
}
