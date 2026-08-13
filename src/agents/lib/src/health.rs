use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub const POLAR_HEALTH_FILE_ENV: &str = "POLAR_HEALTH_FILE";
pub const POLAR_HEALTH_FILE_DEFAULT: &str = "/tmp/polar-health.json";
pub const POLAR_HEALTH_CERTS_ENV: &str = "POLAR_HEALTH_CERTS";
pub const POLAR_HEALTH_EXPIRY_SECS_ENV: &str = "POLAR_HEALTH_EXPIRY_SECS";
pub const POLAR_HEALTH_STALE_SECS_ENV: &str = "POLAR_HEALTH_STALE_SECS";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthState {
    /// "healthy" | "degraded" | "unhealthy"
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
