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
mod test_support;

pub use config::HealthConfig;
use machine::{HealthMachine, Next, TickOutcome, TickStep};

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
pub const POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV: &str = "POLAR_HEALTH_CASSINI_DEADLINE_SECS";

pub const DEFAULT_EXPIRY_THRESHOLD_SECS: i64 = 60;
pub const DEFAULT_TICK_SECS: u64 = 30;
/// How old the health file may get before the liveness probe fails.
pub const DEFAULT_STALE_THRESHOLD_SECS: u64 = 90;
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
/// How long the agent may go without a Cassini connection, from startup or from a
/// disconnect, before the health actor gives up on it. About what the liveness
/// probe amounted to before the actor owned this decision; to be tuned together
/// with Cassini session repair, since a longer grace only helps once a reconnect
/// really restores the agent's subscriptions.
pub const DEFAULT_CASSINI_DEADLINE_SECS: u64 = 60;

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

    /// Write the health state atomically to the configured path.
    /// Uses a tmp file + rename to avoid partial reads by the healthcheck binary.
    pub fn write(&self) -> Result<(), String> {
        let path = std::env::var(POLAR_HEALTH_FILE_ENV)
            .unwrap_or_else(|_| POLAR_HEALTH_FILE_DEFAULT.to_string());
        self.write_to(&path)
    }

    /// As [`write`](Self::write), to `path`.
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

    /// Where to send the verdict that the agent must end.
    ///
    /// With a recipient, the actor sends one [`HealthVerdict`] and stays alive,
    /// refreshing the health file while the agent shuts down. With `None` it
    /// ends itself instead (fails on a hard deadline, stops for rejuvenation),
    /// which is what agents that do not use the builder rely on.
    pub verdict_to: Option<Arc<OutputPort<HealthVerdict>>>,
}

impl std::fmt::Debug for HealthCheckArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthCheckArgs")
            .field("expects_graph", &self.expects_graph)
            .field("rejuvenation_threshold_secs", &self.rejuvenation_threshold_secs)
            .field("dep_cert_endpoints", &self.dep_cert_endpoints)
            .field("verdict_to", &self.verdict_to.is_some())
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
}

/// Whether every dependency endpoint has a healthy certificate TTL.
/// Logs a warning for each that fails and does not short-circuit, so all
/// failures are visible in a single tick.
///
/// `check_ttl()` does blocking network and TLS handshake I/O (up to ~5s per
/// endpoint through its read and write timeouts), so each runs through
/// `spawn_blocking` rather than holding up the thread this tick runs on.
async fn deps_healthy(endpoints: &[DepCertEndpoint]) -> bool {
    if endpoints.is_empty() {
        return true;
    }

    let mut all_healthy = true;
    for dep in endpoints {
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
    let result = health.write();
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
        let config = HealthConfig::load(args).map_err(ActorProcessingErr::from)?;
        let machine = HealthMachine::new(&config, HealthState::now());
        // The one tick chain. It runs from startup, so the hard deadlines and the
        // health file do not wait for Cassini to connect.
        schedule_tick(&myself, config.tick_secs);
        Ok(HealthCheckActorState {
            config,
            machine,
            verdict_to,
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
                let outcome = match state.machine.tick(now, &certs) {
                    TickStep::Done(outcome) => outcome,
                    TickStep::NeedDeps => {
                        let healthy = deps_healthy(&state.config.dep_cert_endpoints).await;
                        let jitter = machine::jitter_secs(rand::random::<u32>());
                        state
                            .machine
                            .deps_checked(HealthState::now(), healthy, jitter)
                    }
                };
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
    // These go through the real actor, real files and real (1 s) ticks, so they
    // keep passing unchanged across the refactor. The actor reads its settings
    // from the process environment, so they run one at a time.

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Removes every health variable, whatever settings exist today or are added
    /// later, so one test's environment cannot leak into the next.
    fn clear_health_env() {
        let names: Vec<String> = std::env::vars()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("POLAR_HEALTH_"))
            .collect();
        // SAFETY: callers hold ENV_LOCK, and no other test reads these.
        unsafe {
            for name in names {
                std::env::remove_var(name);
            }
        }
    }

    /// Clears every health variable, applies `vars`, and clears them again on drop.
    struct EnvGuard;

    impl EnvGuard {
        fn set(vars: &[(&str, &str)]) -> Self {
            clear_health_env();
            // SAFETY: callers hold ENV_LOCK, and no other test reads these.
            unsafe {
                for (name, value) in vars {
                    std::env::set_var(name, value);
                }
            }
            EnvGuard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            clear_health_env();
        }
    }

    fn args() -> HealthCheckArgs {
        HealthCheckArgs {
            expects_graph: false,
            rejuvenation_threshold_secs: 300,
            dep_cert_endpoints: vec![],
            verdict_to: None,
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

    // -- the tick chain ---------------------------------------------------------
    //
    // Paused time: the 30 s default tick costs nothing, and the clock only moves
    // when every task is idle, so "how long did this take" is exact.

    #[tokio::test(start_paused = true)]
    async fn ticks_run_from_startup_without_waiting_for_cassini() {
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let _env = EnvGuard::set(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let _env = EnvGuard::set(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        // The directory does not exist, so every write fails.
        let file = dir.path().join("no-such-dir").join("health.json");
        let _env = EnvGuard::set(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let _env = EnvGuard::set(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();
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
    async fn args_with_verdicts() -> (
        HealthCheckArgs,
        Arc<std::sync::Mutex<Vec<HealthVerdict>>>,
        ActorRef<HealthVerdict>,
    ) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (collector, _) = Actor::spawn(None, Collector, seen.clone()).await.unwrap();
        let port = Arc::new(OutputPort::default());
        port.subscribe(collector.clone(), Some);
        let mut a = args();
        a.verdict_to = Some(port);
        (a, seen, collector)
    }

    #[tokio::test(start_paused = true)]
    async fn a_hard_deadline_becomes_a_verdict_and_the_actor_stays_alive() {
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 20);
        let _env = EnvGuard::set(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
        ]);

        let (a, seen, _collector) = args_with_verdicts().await;
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 3600);
        let _env = EnvGuard::set(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            // Rejuvenate as soon as the cert is under two hours from expiry.
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "7200"),
        ]);

        let (a, seen, _collector) = args_with_verdicts().await;
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("no-such-dir").join("health.json");
        let _env = EnvGuard::set(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (a, seen, _collector) = args_with_verdicts().await;
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let cert = cert_file(&dir, "agent.pem", 3600);
        let _env = EnvGuard::set(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CERTS_ENV, &cert),
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "7200"),
        ]);

        let (actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();
        actor.cast(HealthCheckMessage::CassiniConnected).unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(600), handle).await;
        assert!(ended.is_ok(), "the actor never stopped for rejuvenation");
    }

    // -- the Cassini deadline ----------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn losing_cassini_for_too_long_becomes_a_verdict() {
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let _env = EnvGuard::set(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (a, seen, _collector) = args_with_verdicts().await;
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let _env = EnvGuard::set(&[
            (POLAR_HEALTH_FILE_ENV, file.to_str().unwrap()),
            (POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV, "100"),
        ]);

        let (a, seen, _collector) = args_with_verdicts().await;
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
        let _lock = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("health.json");
        let _env = EnvGuard::set(&[(POLAR_HEALTH_FILE_ENV, file.to_str().unwrap())]);

        let (_actor, handle) = Actor::spawn(None, HealthCheckActor, args()).await.unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(200), handle).await;
        assert!(
            ended.is_ok(),
            "Cassini never came up and the actor never failed"
        );
    }
}
