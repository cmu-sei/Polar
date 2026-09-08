/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use neo4rs::BoltType;
use polar::graph::controller::GraphNodeKey;

/// Node key vocabulary for the Credentials Agent's own graph
/// projection -- disposable, rebuildable observability, per
/// credentials-agent-architecture.md, never the source of truth (the
/// event log is). Lives here, in credentials_agent_common, matching
/// jira_common's own established pattern of keeping agent-specific
/// node vocabulary alongside that agent's own data types, rather than
/// in the shared lib.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CredentialsNodeKey {
    /// A grant, keyed on the identity it was made to. Never deleted
    /// on consumption -- soft-tracked via a `consumed` property
    /// instead, matching how a revoked certificate is marked rather
    /// than removed. A stale, consumed grant node is genuinely useful
    /// audit information, not clutter.
    Grant { target_identity: String },
    /// An issued certificate, keyed on its own serial number --
    /// matching the same join key used everywhere else this codebase
    /// already treats a certificate's identity as (cert-issuer's own
    /// telemetry, credentials-agent's own Revoke events).
    Certificate { serial: String },
    /// An issued agent escrow-identity (CertType::Service)
    /// certificate, keyed on its own serial number. A distinct node
    /// type from Certificate, not folded into it -- these are
    /// genuinely different kinds of certificate (a service's durable
    /// identity vs. a pod-instance's ephemeral mTLS credential), and
    /// today's shared serial_index/Revoke fold logic doesn't even
    /// track these two kinds of certificate together (see
    /// AgentEscrowCertificateRecord's own doc comment, in
    /// credentials-agent/consume's state.rs).
    AgentEscrowCertificate { serial: String },
    /// A singleton node tracking exactly how far this projection has
    /// caught up with the event log -- there is and only ever will be
    /// exactly one of these.
    ProjectionCheckpoint,
}

impl GraphNodeKey for CredentialsNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            CredentialsNodeKey::Grant { target_identity } => {
                let k = format!("{prefix}_target_identity");
                (
                    format!("({prefix}:CredentialsGrant {{ target_identity: ${k} }})"),
                    vec![(k, BoltType::String(target_identity.clone().into()))],
                )
            }
            CredentialsNodeKey::Certificate { serial } => {
                let k = format!("{prefix}_serial");
                (
                    format!("({prefix}:CredentialsCertificate {{ serial: ${k} }})"),
                    vec![(k, BoltType::String(serial.clone().into()))],
                )
            }
            CredentialsNodeKey::AgentEscrowCertificate { serial } => {
                let k = format!("{prefix}_serial");
                (
                    format!("({prefix}:CredentialsAgentEscrowCertificate {{ serial: ${k} }})"),
                    vec![(k, BoltType::String(serial.clone().into()))],
                )
            }
            CredentialsNodeKey::ProjectionCheckpoint => {
                let k = format!("{prefix}_id");
                (
                    format!("({prefix}:CredentialsProjectionCheckpoint {{ id: ${k} }})"),
                    vec![(k, BoltType::String("singleton".into()))],
                )
            }
        }
    }
}
