//! Structured event emission.
//!
//! Events are written to stdout as JSON lines (durable record) and
//! published to Cassini topic `polar.cert-issuer.events.v1` (real-time
//! stream). v1 only implements stdout emission; Cassini integration
//! arrives in Phase B.

use cert_issuer_common::IssueOutcome;
use serde::Serialize;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize)]
pub struct IssuanceEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub session_id: String,
    pub workload_identity: Option<String>,
    pub instance_binding: Option<String>,
    pub issuer: Option<String>,
    pub csr_san: Option<String>,
    pub outcome: IssueOutcome,
    pub failure_detail: Option<String>,
    pub latency_oidc_ms: Option<u64>,
    pub latency_ca_ms: Option<u64>,
    pub latency_total_ms: u64,
    pub serial_number: Option<String>,
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: &IssuanceEvent);
}

pub struct StdoutSink;

impl EventSink for StdoutSink {
    fn emit(&self, _event: &IssuanceEvent) {
        todo!("implement stdout JSON emission")
    }
}
