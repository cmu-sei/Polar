//! polar-health
//!
//! Shared health state types and the HealthCheckActor for Polar agents.
//!
//! Each agent supervisor spawns a HealthCheckActor as a linked child.
//! The actor owns all health concerns:
//!   - Periodic cert expiry checks via x509-parser
//!   - Tracking of connection state (Cassini, Neo4j)
//!   - Writing the health file read by the polar-healthcheck liveness probe binary
//!   - Signaling unhealthy state by stopping itself, which propagates as
//!     ActorFailed to the supervisor, which calls std::process::exit(1)

use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use serde::{Deserialize, Serialize};
use std::fs;
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

// ---------------------------------------------------------------------------
// HealthState — the JSON file format read by polar-healthcheck
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthState {
    /// "healthy" | "unhealthy"
    pub status: String,
    /// Unix timestamp (seconds) when this state was written.
    pub timestamp: u64,
    pub cassini_connected: bool,
    pub neo4j_connected: bool,
}

impl HealthState {
    pub fn healthy(cassini_connected: bool, neo4j_connected: bool) -> Self {
        Self {
            status: "healthy".to_string(),
            timestamp: Self::now(),
            cassini_connected,
            neo4j_connected,
        }
    }

    pub fn unhealthy(cassini_connected: bool, neo4j_connected: bool) -> Self {
        Self {
            status: "unhealthy".to_string(),
            timestamp: Self::now(),
            cassini_connected,
            neo4j_connected,
        }
    }

    /// Write the health state atomically to the configured path.
    /// Uses a tmp file + rename to avoid partial reads by the healthcheck binary.
    pub fn write(&self) {
        let path = std::env::var(POLAR_HEALTH_FILE_ENV)
            .unwrap_or_else(|_| POLAR_HEALTH_FILE_DEFAULT.to_string());
        match serde_json::to_string(self) {
            Ok(json) => {
                let tmp = format!("{path}.tmp");
                if fs::write(&tmp, &json).is_ok() {
                    let _ = fs::rename(&tmp, &path);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize health state: {e}");
            }
        }
    }

    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

// ---------------------------------------------------------------------------
// HealthCheckMessage — messages the supervisor sends to the actor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum HealthCheckMessage {
    /// Cassini TLS connection established and registered.
    CassiniConnected,
    /// Cassini connection lost or rejected.
    CassiniDisconnected,
    /// Neo4j connection established.
    Neo4jConnected,
    /// Neo4j connection lost.
    Neo4jDisconnected,
    /// Internal periodic tick — do not send externally.
    Tick,
}

// ---------------------------------------------------------------------------
// HealthCheckArgs — passed at spawn time
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HealthCheckArgs {
    /// Does this agent connect to Neo4j? If true, neo4j_connected must be
    /// true for the agent to be considered healthy.
    pub expects_neo4j: bool,
}

// ---------------------------------------------------------------------------
// HealthCheckActor
// ---------------------------------------------------------------------------

pub struct HealthCheckActor;

pub struct HealthCheckActorState {
    pub cassini_connected: bool,
    pub neo4j_connected: bool,
    pub expects_neo4j: bool,
    pub expiry_threshold_secs: i64,
    pub tick_secs: u64,
    pub cert_paths: Vec<String>,
}

impl HealthCheckActorState {
    /// Check all cert paths. Returns Ok(()) if all certs are valid and not
    /// expiring within the threshold. Returns Err with a description otherwise.
    fn check_certs(&self) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        for path in &self.cert_paths {
            match fs::read(path) {
                Err(e) => return Err(format!("failed to read cert {path}: {e}")),
                Ok(data) => match parse_x509_pem(&data) {
                    Err(e) => return Err(format!("failed to parse PEM {path}: {e}")),
                    Ok((_, pem)) => match pem.parse_x509() {
                        Err(e) => return Err(format!("failed to parse x509 {path}: {e}")),
                        Ok(cert) => {
                            let not_after = cert.validity().not_after.timestamp();
                            let secs_remaining = not_after - now;
                            if secs_remaining < self.expiry_threshold_secs {
                                return Err(format!(
                                    "cert {path} expires in {secs_remaining}s (threshold {}s)",
                                    self.expiry_threshold_secs
                                ));
                            }
                            tracing::debug!("cert {path} valid for {secs_remaining}s");
                        }
                    },
                },
            }
        }
        Ok(())
    }

    /// Returns true if all expected connections are established.
    fn connections_healthy(&self) -> bool {
        if !self.cassini_connected {
            return false;
        }
        if self.expects_neo4j && !self.neo4j_connected {
            return false;
        }
        true
    }
}

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
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_EXPIRY_THRESHOLD_SECS);

        let tick_secs = std::env::var(POLAR_HEALTH_TICK_SECS_ENV)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_TICK_SECS);

        Ok(HealthCheckActorState {
            cassini_connected: false,
            neo4j_connected: false,
            expects_neo4j: args.expects_neo4j,
            expiry_threshold_secs,
            tick_secs,
            cert_paths,
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

                // Start the periodic health tick now that we're connected
                let _ = myself
                    .send_after(Duration::from_secs(state.tick_secs), || HealthCheckMessage::Tick)
                    .await;
            }
            HealthCheckMessage::CassiniDisconnected => {
                state.cassini_connected = false;
                tracing::warn!("healthcheck: cassini disconnected");
                // Write unhealthy immediately — don't wait for next tick
                HealthState::unhealthy(false, state.neo4j_connected).write();
                return Err(ActorProcessingErr::from("cassini disconnected"));
            }
            HealthCheckMessage::Neo4jConnected => {
                state.neo4j_connected = true;
                tracing::debug!("healthcheck: neo4j connected");
            }
            HealthCheckMessage::Neo4jDisconnected => {
                state.neo4j_connected = false;
                tracing::warn!("healthcheck: neo4j disconnected");
                if state.expects_neo4j {
                    HealthState::unhealthy(state.cassini_connected, false).write();
                    return Err(ActorProcessingErr::from("neo4j disconnected"));
                }
            }
            HealthCheckMessage::Tick => {
                // Check certs first
                if let Err(reason) = state.check_certs() {
                    tracing::error!("healthcheck: cert check failed: {reason}");
                    HealthState::unhealthy(state.cassini_connected, state.neo4j_connected).write();
                    return Err(ActorProcessingErr::from(reason));
                }

                // Check connections
                if !state.connections_healthy() {
                    let reason = format!(
                        "unhealthy connections: cassini={} neo4j={} expects_neo4j={}",
                        state.cassini_connected, state.neo4j_connected, state.expects_neo4j
                    );
                    tracing::error!("healthcheck: {reason}");
                    HealthState::unhealthy(state.cassini_connected, state.neo4j_connected).write();
                    return Err(ActorProcessingErr::from(reason));
                }

                // All good — write healthy state and reschedule
                HealthState::healthy(state.cassini_connected, state.neo4j_connected).write();
                let _ = myself
                    .send_after(Duration::from_secs(state.tick_secs), || {
                        HealthCheckMessage::Tick
                    })
                    .await;
            }
        }
        Ok(())
    }
}
