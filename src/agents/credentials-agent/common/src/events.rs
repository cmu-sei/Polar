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

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// A grant or revocation event -- identical whether it arrives over
/// Cassini (rkyv, for wire transport between agents) or is read back
/// from the Credentials Agent's own durable event log (serde JSON, for
/// human-readable, replayable persistence -- see lib::event_log). No
/// transformation happens between wire and log for these two, unlike
/// the issuance flow's request/reply/outcome shapes, which are
/// genuinely different at each stage and modeled separately.
///
/// `timestamp` is Unix milliseconds, not `chrono::DateTime` directly --
/// `rkyv` has no native support for `chrono` types, a real, structural
/// gap this codebase has consistently worked around elsewhere by never
/// storing a `chrono` type directly on anything that crosses an `rkyv`
/// boundary (see `kubernetes/consume`'s and `git/process`'s own i64-millis
/// convention, which this matches exactly). Use `lib::time::now_millis()`
/// to produce one and `lib::time::format_millis_rfc3339()` to render one
/// for a human reader.
///
/// This is the admin's own domain timestamp (when they took the
/// action), distinct from the operational timestamp `EventLog` records
/// automatically on append (when the consumer actually wrote this
/// entry) -- these can genuinely differ in an async system with real
/// network/queueing latency between the two.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub enum CredentialsEvent {
    /// Published by an admin's tool to the `credentials-grants` topic.
    /// `target_identity` must be the exact claim value the relevant
    /// issuer's `workload_identity_claim` will later extract from the
    /// user's real token -- the admin is responsible for confirming
    /// this against the IdP directly before publishing.
    Grant {
        target_identity: String,
        /// Deliberately a plain string list for now -- the concrete
        /// shape of a "permission" (how it becomes SAN entries, what
        /// vocabulary it's drawn from) is real, undesigned follow-up
        /// work, not something to speculatively over-structure here
        /// ahead of that design.
        permissions: Vec<String>,
        granted_by: String,
        timestamp: i64,
    },
    /// Published by an admin's tool to the `credentials-revocations`
    /// topic. Uniform for human- and workload-issued certificates
    /// alike -- both are ultimately just certificates with serial
    /// numbers.
    Revoke {
        serial: String,
        reason: String,
        revoked_by: String,
        timestamp: i64,
    },
    /// Published internally by the consumer itself once a signed
    /// certificate has actually been produced -- not published by any
    /// external caller, unlike Grant/Revoke. Gives the Credentials
    /// Agent complete visibility into every certificate ever issued
    /// (the actual basis for a complete, authoritative CRL), and is
    /// the fact the renewal-prevention check folds over: no request
    /// may succeed for an `(identity, instance_binding)` pair that
    /// already has a currently valid, unrevoked certificate on record.
    ///
    /// `permissions` deliberately isn't duplicated here -- the log is
    /// fully replayable, so folding over history can always correlate
    /// this event back to whichever `Grant` it consumed by
    /// `identity`, making a second copy of the same data redundant
    /// rather than genuinely useful.
    IssuanceCompleted {
        identity: String,
        instance_binding: String,
        serial: String,
        timestamp: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_grant() -> CredentialsEvent {
        CredentialsEvent::Grant {
            target_identity: "jdoe".to_string(),
            permissions: vec!["read".to_string(), "write".to_string()],
            granted_by: "admin".to_string(),
            timestamp: 1767225600000,
        }
    }

    fn sample_revoke() -> CredentialsEvent {
        CredentialsEvent::Revoke {
            serial: "abc123".to_string(),
            reason: "compromised".to_string(),
            revoked_by: "admin".to_string(),
            timestamp: 1767225600000,
        }
    }

    fn sample_issuance_completed() -> CredentialsEvent {
        CredentialsEvent::IssuanceCompleted {
            identity: "jdoe.polar.keycloak-issuer".to_string(),
            instance_binding: "some-session-id".to_string(),
            serial: "abc123".to_string(),
            timestamp: 1767225600000,
        }
    }

    #[test]
    fn grant_round_trips_through_serde_json() {
        // The path EventLog actually uses for durable, replayable
        // persistence.
        let original = sample_grant();
        let json = serde_json::to_string(&original).unwrap();
        let restored: CredentialsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn revoke_round_trips_through_serde_json() {
        let original = sample_revoke();
        let json = serde_json::to_string(&original).unwrap();
        let restored: CredentialsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn grant_round_trips_through_rkyv() {
        // The path Cassini actually uses for wire transport between
        // agents -- the same type, the other serialization entirely,
        // proving both derive sets genuinely coexist without conflict.
        let original = sample_grant();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: CredentialsEvent =
            rkyv::from_bytes::<CredentialsEvent, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn revoke_round_trips_through_rkyv() {
        let original = sample_revoke();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: CredentialsEvent =
            rkyv::from_bytes::<CredentialsEvent, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn issuance_completed_round_trips_through_serde_json() {
        let original = sample_issuance_completed();
        let json = serde_json::to_string(&original).unwrap();
        let restored: CredentialsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn issuance_completed_round_trips_through_rkyv() {
        let original = sample_issuance_completed();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: CredentialsEvent =
            rkyv::from_bytes::<CredentialsEvent, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }
}
