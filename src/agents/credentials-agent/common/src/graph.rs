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

use neo4rs::BoltType;
use polar::graph::controller::{GraphNodeKey, NodeIdentity, NodeKeyVocabulary};

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
    /// A Polar user account, keyed on its own durable account_id --
    /// never on username, which is a human-facing nickname that could
    /// in principle change, not the identity itself.
    Account { account_id: String },
}

impl GraphNodeKey for CredentialsNodeKey {
    fn identity(&self) -> NodeIdentity {
        let s = |v: &String| BoltType::String(v.clone().into());
        match self {
            CredentialsNodeKey::Grant { target_identity } => NodeIdentity::fixed(
                &["CredentialsGrant"],
                vec![("target_identity", s(target_identity))],
            ),
            CredentialsNodeKey::Certificate { serial } => {
                NodeIdentity::fixed(&["CredentialsCertificate"], vec![("serial", s(serial))])
            }
            CredentialsNodeKey::AgentEscrowCertificate { serial } => NodeIdentity::fixed(
                &["CredentialsAgentEscrowCertificate"],
                vec![("serial", s(serial))],
            ),
            CredentialsNodeKey::Account { account_id } => {
                NodeIdentity::fixed(&["CredentialsAccount"], vec![("account_id", s(account_id))])
            }
            CredentialsNodeKey::ProjectionCheckpoint => NodeIdentity::fixed(
                &["CredentialsProjectionCheckpoint"],
                vec![("id", BoltType::String("singleton".into()))],
            ),
        }
    }
}

impl NodeKeyVocabulary for CredentialsNodeKey {
    fn every_variant() -> Vec<Self> {
        let s = || "x".to_string();
        let all = vec![
            CredentialsNodeKey::Grant {
                target_identity: s(),
            },
            CredentialsNodeKey::Certificate { serial: s() },
            CredentialsNodeKey::AgentEscrowCertificate { serial: s() },
            CredentialsNodeKey::ProjectionCheckpoint,
            CredentialsNodeKey::Account { account_id: s() },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                CredentialsNodeKey::Grant { .. }
                | CredentialsNodeKey::Certificate { .. }
                | CredentialsNodeKey::AgentEscrowCertificate { .. }
                | CredentialsNodeKey::ProjectionCheckpoint
                | CredentialsNodeKey::Account { .. } => {}
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar::graph::controller::pattern_problem;
    use proptest::prelude::*;

    proptest! {
        /// Every credentials key keeps the rules its patterns are composed by, whatever variable it is given.
        #[test]
        fn every_key_binds_the_variable_it_is_given(prefix in "[a-z][a-z0-9]{0,5}") {
            for key in CredentialsNodeKey::every_variant() {
                prop_assert_eq!(pattern_problem(&key, &prefix), None);
            }
        }
    }
}
