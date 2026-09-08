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

use credentials_agent_common::events::{CredentialsEvent, GrantReason, RevocationReason};
use std::collections::HashMap;

/// A grant currently available to be consumed by a matching issuance.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantRecord {
    pub permissions: Vec<cert_issuer_common::permission::Permission>,
    pub granted_by: String,
    pub timestamp: i64,
    pub reason: GrantReason,
}

/// A currently-valid (issued, not yet revoked) certificate.
#[derive(Debug, Clone, PartialEq)]
pub struct CertificateRecord {
    pub identity: String,
    pub instance_binding: String,
    pub serial: String,
    pub timestamp: i64,
    /// The actual signed certificate PEM, carried through from
    /// IssuanceCompleted -- what a certificate-lookup query returns
    /// for this identity.
    pub certificate_pem: String,
    /// Marked in place rather than removed on revocation (explicit,
    /// via a Revoke event) or supersession (implicit, when a reissue
    /// for the same identity arrives) -- the record, and its own
    /// audit trail, survives either way.
    pub revoked: bool,
    pub revoked_reason: Option<RevocationReason>,
    pub revoked_by: Option<String>,
    pub revoked_at: Option<i64>,
}

/// The Credentials Agent's derived state -- entirely a function of
/// folding `credentials_agent_common::events::CredentialsEvent`
/// through `apply()`, in order, from an empty state. Never itself
/// persisted; rebuilt from `lib::event_log::EventLog` on every
/// startup. The Neo4j projection is, in turn, entirely derived from
/// this -- this struct, not Neo4j, is the real in-memory source of
/// truth this process operates against between restarts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CredentialsState {
    /// Active (unconsumed) grants, keyed by `target_identity`. A grant
    /// is single-use -- removed the moment a matching
    /// `IssuanceCompleted` event is folded, never mutated in place.
    active_grants: HashMap<String, GrantRecord>,

    /// Every currently-valid certificate, keyed on `(identity,
    /// instance_binding)` -- exactly the pair the renewal-prevention
    /// check needs (see credentials-agent-architecture.md, "Pods
    /// cannot request their own renewal").
    valid_certificates: HashMap<(String, String), CertificateRecord>,

    /// `serial -> (identity, instance_binding)`, so a `Revoke` event
    /// (which only ever carries a serial) can find and remove the
    /// matching entry above without a linear scan over every
    /// currently-valid certificate.
    serial_index: HashMap<String, (String, String)>,

    /// The current, escrowed secret for each name -- keyed on the
    /// storing agent's own chosen name. A second SecretStored for the
    /// same name overwrites the entry here (derived state only; the
    /// event log itself never rewrites history, so the superseded
    /// EscrowedSecret remains recoverable by replay regardless).
    stored_secrets: HashMap<String, credentials_agent_common::escrow_secret::EscrowedSecret>,
}

impl CredentialsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds state by folding a full, ordered sequence of events --
    /// exactly what `EventLog::open`'s replayed events should be
    /// passed through, on every startup.
    pub fn from_events(events: &[CredentialsEvent]) -> Self {
        let mut state = Self::new();
        for event in events {
            state.apply(event);
        }
        state
    }

    /// Folds one event into the current state. Order matters -- this
    /// must only ever be called in the exact order events were
    /// originally appended (or replayed) in.
    ///
    /// Returns the serial numbers of any certificates superseded as
    /// a side effect of this fold -- always empty except for a real
    /// reissue, where a prior, still-valid certificate for the same
    /// identity gets implicitly revoked. This is the one place that
    /// genuinely knows this happened at the moment it does, so both
    /// the steady-state projection path (ledger.rs) and the catch-up
    /// replay path (projection.rs) use this same, single source of
    /// truth to know which certificates need their own revocation op
    /// emitted to the graph, rather than duplicating this logic.
    pub fn apply(&mut self, event: &CredentialsEvent) -> Vec<String> {
        let mut superseded_serials = Vec::new();
        match event {
            CredentialsEvent::Grant {
                target_identity,
                permissions,
                granted_by,
                timestamp,
                reason,
            } => {
                self.active_grants.insert(
                    target_identity.clone(),
                    GrantRecord {
                        permissions: permissions.clone(),
                        granted_by: granted_by.clone(),
                        timestamp: *timestamp,
                        reason: *reason,
                    },
                );
            }
            CredentialsEvent::IssuanceCompleted {
                identity,
                instance_binding,
                serial,
                timestamp,
                certificate_pem,
            } => {
                // Single-use: remove the matching grant, if any. A
                // harmless no-op when there isn't one -- exactly what
                // lets admin bootstrap (which has no grant at all)
                // fold through this same logic with zero
                // special-casing.
                self.active_grants.remove(identity);

                // A reissue for the same identity supersedes any
                // prior, still-valid certificate for it -- regardless
                // of instance_binding, since a pod restart gets a
                // fresh one. Marked revoked in place, not removed, so
                // the record survives. Done before inserting the new
                // entry, so this can't mark the new one itself.
                for record in self.valid_certificates.values_mut() {
                    if record.identity == *identity && !record.revoked {
                        record.revoked = true;
                        record.revoked_reason = Some(RevocationReason::Superseded);
                        record.revoked_by = Some("system".to_string());
                        record.revoked_at = Some(*timestamp);
                        superseded_serials.push(record.serial.clone());
                    }
                }

                let key = (identity.clone(), instance_binding.clone());
                self.valid_certificates.insert(
                    key.clone(),
                    CertificateRecord {
                        identity: identity.clone(),
                        instance_binding: instance_binding.clone(),
                        serial: serial.clone(),
                        timestamp: *timestamp,
                        certificate_pem: certificate_pem.clone(),
                        revoked: false,
                        revoked_reason: None,
                        revoked_by: None,
                        revoked_at: None,
                    },
                );
                self.serial_index.insert(serial.clone(), key);
            }
            CredentialsEvent::Revoke {
                serial,
                reason,
                revoked_by,
                timestamp,
            } => {
                if let Some(key) = self.serial_index.get(serial)
                    && let Some(record) = self.valid_certificates.get_mut(key)
                {
                    record.revoked = true;
                    record.revoked_reason = Some(*reason);
                    record.revoked_by = Some(revoked_by.clone());
                    record.revoked_at = Some(*timestamp);
                }
            }
            CredentialsEvent::SecretStored { name, secret, .. } => {
                // Overwrites any prior entry for this name -- the
                // event log itself still holds the superseded
                // SecretStored event in full, so nothing is ever
                // truly lost, only no longer current.
                self.stored_secrets.insert(name.clone(), secret.clone());
            }
        }
        superseded_serials
    }

    /// The active grant for a given identity, if any -- `None` if
    /// never granted, or if already consumed by a prior issuance.
    pub fn grant_for(&self, identity: &str) -> Option<&GrantRecord> {
        self.active_grants.get(identity)
    }

    /// True if this exact `(identity, instance_binding)` pair
    /// currently holds a valid, unrevoked certificate -- the
    /// renewal-prevention check itself.
    pub fn has_valid_certificate(&self, identity: &str, instance_binding: &str) -> bool {
        self.valid_certificates
            .get(&(identity.to_string(), instance_binding.to_string()))
            .is_some_and(|record| !record.revoked)
    }
    /// The current, non-revoked certificate for a given identity, if
    /// any -- regardless of instance_binding, since a caller looking
    /// up another agent's cert has no way of knowing that in advance.
    /// A reissue for the same identity marks any prior entry revoked
    /// (see apply()'s own IssuanceCompleted handling), so there's at
    /// most one non-revoked entry per identity at any given time.
    pub fn certificate_for(&self, identity: &str) -> Option<&CertificateRecord> {
        self.valid_certificates
            .values()
            .find(|record| record.identity == identity && !record.revoked)
    }

    /// The current, escrowed secret stored under `name`, if any --
    /// always the most recently stored one; a superseded prior value
    /// remains in the event log but is no longer returned here.
    pub fn secret_for(
        &self,
        name: &str,
    ) -> Option<&credentials_agent_common::escrow_secret::EscrowedSecret> {
        self.stored_secrets.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(identity: &str) -> CredentialsEvent {
        CredentialsEvent::Grant {
            target_identity: identity.to_string(),
            permissions: vec![cert_issuer_common::permission::Permission {
                scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                target: None,
            }],
            granted_by: "admin".to_string(),
            timestamp: 1000,
            reason: GrantReason::UserOnboarding,
        }
    }

    fn issued(identity: &str, instance_binding: &str, serial: &str) -> CredentialsEvent {
        CredentialsEvent::IssuanceCompleted {
            identity: identity.to_string(),
            instance_binding: instance_binding.to_string(),
            serial: serial.to_string(),
            timestamp: 2000,
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                .to_string(),
        }
    }

    fn revoked(serial: &str) -> CredentialsEvent {
        CredentialsEvent::Revoke {
            serial: serial.to_string(),
            reason: RevocationReason::Compromised,
            revoked_by: "admin".to_string(),
            timestamp: 3000,
        }
    }

    #[test]
    fn grant_is_visible_before_being_consumed() {
        let mut state = CredentialsState::new();
        state.apply(&grant("alice"));
        assert!(state.grant_for("alice").is_some());
    }

    #[test]
    fn issuance_consumes_the_matching_grant() {
        let mut state = CredentialsState::new();
        state.apply(&grant("alice"));
        state.apply(&issued("alice", "pod-uid-1", "serial-1"));
        assert!(
            state.grant_for("alice").is_none(),
            "grant must be consumed, not still available"
        );
        assert!(state.has_valid_certificate("alice", "pod-uid-1"));
    }

    #[test]
    fn issuance_without_a_prior_grant_still_works() {
        // The admin-bootstrap case: no Grant event ever exists for
        // this flow at all. Must fold cleanly with zero special-casing.
        let mut state = CredentialsState::new();
        state.apply(&issued("admin-bootstrap-identity", "admin-tool-uid", "serial-99"));
        assert!(state.has_valid_certificate("admin-bootstrap-identity", "admin-tool-uid"));
    }

    #[test]
    fn revoke_removes_the_certificate() {
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "pod-uid-1", "serial-1"));
        assert!(state.has_valid_certificate("alice", "pod-uid-1"));

        state.apply(&revoked("serial-1"));
        assert!(!state.has_valid_certificate("alice", "pod-uid-1"));
    }

    #[test]
    fn revoking_an_unknown_serial_is_a_harmless_no_op() {
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "pod-uid-1", "serial-1"));
        state.apply(&revoked("serial-does-not-exist"));
        // The real, still-valid certificate must be unaffected.
        assert!(state.has_valid_certificate("alice", "pod-uid-1"));
    }

    #[test]
    fn rolling_restart_produces_a_different_instance_binding_correctly() {
        // Real property this whole design exists for: same identity,
        // different pod UID, must NOT be treated as already holding a
        // valid cert.
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "old-pod-uid", "serial-1"));

        assert!(state.has_valid_certificate("alice", "old-pod-uid"));
        assert!(
            !state.has_valid_certificate("alice", "new-pod-uid"),
            "a different instance_binding must not be blocked by an unrelated pod's cert"
        );
    }

    #[test]
    fn two_identities_do_not_interfere() {
        let mut state = CredentialsState::new();
        state.apply(&grant("alice"));
        state.apply(&grant("bob"));
        state.apply(&issued("alice", "pod-uid-1", "serial-1"));

        assert!(state.grant_for("alice").is_none());
        assert!(
            state.grant_for("bob").is_some(),
            "bob's grant must be untouched by alice's issuance"
        );
        assert!(state.has_valid_certificate("alice", "pod-uid-1"));
        assert!(!state.has_valid_certificate("bob", "pod-uid-1"));
    }

    #[test]
    fn from_events_replays_a_realistic_full_sequence() {
        let events = vec![
            grant("alice"),
            issued("alice", "pod-uid-1", "serial-1"),
            revoked("serial-1"),
        ];
        let state = CredentialsState::from_events(&events);

        assert!(state.grant_for("alice").is_none());
        assert!(!state.has_valid_certificate("alice", "pod-uid-1"));
    }
}
