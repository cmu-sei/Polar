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
