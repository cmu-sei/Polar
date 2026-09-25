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

/// A currently-issued agent escrow-identity (CertType::Service)
/// certificate -- keyed on identity alone, per
/// AgentEscrowIssuanceCompleted's own doc comment, not
/// (identity, instance_binding). Deliberately does not track
/// revocation yet -- Revoke events today only ever look up (and
/// mark) entries in valid_certificates, never this map. A real,
/// separate expansion, named here rather than silently assumed:
/// service-certificate revocation.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEscrowCertificateRecord {
    pub instance_binding: String,
    pub serial: String,
    pub timestamp: i64,
    pub certificate_pem: String,
}

/// A Polar account an admin (or the bootstrap identity) has
/// provisioned but that hasn't yet been claimed with a CSR.
#[derive(Debug, Clone, PartialEq)]
pub struct ProvisionedAccountRecord {
    pub account_id: String,
    pub email: String,
    pub permissions: Vec<cert_issuer_common::permission::Permission>,
    pub provisioned_by: String,
    pub timestamp: i64,
    pub reason: GrantReason,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub title: Option<String>,
    pub telephone_number: Option<String>,
}

/// A currently-issued certificate for a Polar account, keyed on
/// account_id alone -- mirrors AgentEscrowCertificateRecord. Does not
/// track revocation yet, same deliberately-named gap as that record.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountCertificateRecord {
    pub account_id: String,
    pub username: String,
    pub serial: String,
    pub timestamp: i64,
    pub certificate_pem: String,
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

    /// Every currently-issued agent escrow-identity certificate,
    /// keyed on identity alone -- see AgentEscrowCertificateRecord's
    /// own doc comment for why this is a separate map from
    /// valid_certificates, not folded into it.
    agent_escrow_certificates: HashMap<String, AgentEscrowCertificateRecord>,

    /// Provisioned-but-unclaimed Polar accounts, keyed by username --
    /// single-use, same as active_grants: removed the moment a
    /// matching AccountCertificateIssued is folded. This IS the
    /// no-self-renewal enforcement -- a second claim attempt with the
    /// same username simply finds nothing here, no separate check
    /// needed.
    active_provisioned_accounts: HashMap<String, ProvisionedAccountRecord>,

    /// Every currently-issued account certificate, keyed on
    /// account_id alone.
    account_certificates: HashMap<String, AccountCertificateRecord>,

    /// True once the one-time admin-bootstrap path has ever
    /// succeeded -- checked before allowing a via_bootstrap
    /// AccountProvisioned to fold a second time.
    bootstrap_completed: bool,
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
            CredentialsEvent::AgentEscrowIssuanceCompleted {
                identity,
                instance_binding,
                serial,
                timestamp,
                certificate_pem,
            } => {
                self.agent_escrow_certificates.insert(
                    identity.clone(),
                    AgentEscrowCertificateRecord {
                        instance_binding: instance_binding.clone(),
                        serial: serial.clone(),
                        timestamp: *timestamp,
                        certificate_pem: certificate_pem.clone(),
                    },
                );
            }
            CredentialsEvent::AccountProvisioned {
                account_id,
                username,
                email,
                permissions,
                provisioned_by,
                timestamp,
                reason,
                via_bootstrap,
                given_name,
                surname,
                title,
                telephone_number,
            } => {
                self.active_provisioned_accounts.insert(
                    username.clone(),
                    ProvisionedAccountRecord {
                        account_id: account_id.clone(),
                        email: email.clone(),
                        permissions: permissions.clone(),
                        provisioned_by: provisioned_by.clone(),
                        timestamp: *timestamp,
                        reason: *reason,
                        given_name: given_name.clone(),
                        surname: surname.clone(),
                        title: title.clone(),
                        telephone_number: telephone_number.clone(),
                    },
                );
                if *via_bootstrap {
                    self.bootstrap_completed = true;
                }
            }
            CredentialsEvent::AccountCertificateIssued {
                account_id,
                username,
                serial,
                timestamp,
                certificate_pem,
            } => {
                self.active_provisioned_accounts.remove(username);
                self.account_certificates.insert(
                    account_id.clone(),
                    AccountCertificateRecord {
                        account_id: account_id.clone(),
                        username: username.clone(),
                        serial: serial.clone(),
                        timestamp: *timestamp,
                        certificate_pem: certificate_pem.clone(),
                    },
                );
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

    /// True if `identity` already holds a currently-issued agent
    /// escrow-identity certificate -- the check
    /// AgentEscrowIssuanceHandler uses in place of the pod-instance-
    /// scoped renewal-prevention check human/workload mTLS issuance
    /// uses instead.
    pub fn has_agent_escrow_certificate(&self, identity: &str) -> bool {
        self.agent_escrow_certificates.contains_key(identity)
    }

    /// The agent escrow-identity certificate record for `identity`,
    /// if one has ever been issued -- backs the agent escrow-key
    /// query (credentials-agent-architecture.md's own "Escrow-key and
    /// certificate-lookup queries" section, third query).
    pub fn agent_escrow_certificate_for(
        &self,
        identity: &str,
    ) -> Option<&AgentEscrowCertificateRecord> {
        self.agent_escrow_certificates.get(identity)
    }

    /// The provisioned-but-unclaimed account record for `username`,
    /// if any -- `None` if never provisioned, or already claimed.
    pub fn provisioned_account_for(&self, username: &str) -> Option<&ProvisionedAccountRecord> {
        self.active_provisioned_accounts.get(username)
    }

    /// True once the one-time admin-bootstrap path has ever
    /// succeeded.
    pub fn bootstrap_completed(&self) -> bool {
        self.bootstrap_completed
    }

    /// The current account certificate record for `account_id`, if
    /// one has ever been issued.
    pub fn account_certificate_for(&self, account_id: &str) -> Option<&AccountCertificateRecord> {
        self.account_certificates.get(account_id)
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

    fn agent_escrow_issued(identity: &str, instance_binding: &str, serial: &str) -> CredentialsEvent {
        CredentialsEvent::AgentEscrowIssuanceCompleted {
            identity: identity.to_string(),
            instance_binding: instance_binding.to_string(),
            serial: serial.to_string(),
            timestamp: 2000,
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake-service-cert\n-----END CERTIFICATE-----\n"
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

    #[test]
    fn agent_escrow_issuance_is_visible_after_being_folded() {
        let mut state = CredentialsState::new();
        assert!(!state.has_agent_escrow_certificate("git-observer"));
        assert!(state.agent_escrow_certificate_for("git-observer").is_none());

        state.apply(&agent_escrow_issued("git-observer", "pod-uid-1", "service-serial-1"));

        assert!(state.has_agent_escrow_certificate("git-observer"));
        let record = state
            .agent_escrow_certificate_for("git-observer")
            .expect("must exist after folding");
        assert_eq!(record.serial, "service-serial-1");
        assert_eq!(record.instance_binding, "pod-uid-1");
    }

    #[test]
    fn agent_escrow_certificate_is_genuinely_separate_from_valid_certificates() {
        // Same identity, same instance_binding -- an ordinary mTLS
        // client certificate and a service escrow certificate for the
        // same agent must not collide or interfere with each other's
        // own map, proving these are genuinely two separate lookups,
        // not accidentally sharing state.
        let mut state = CredentialsState::new();
        state.apply(&issued("git-observer", "pod-uid-1", "client-serial-1"));
        state.apply(&agent_escrow_issued("git-observer", "pod-uid-1", "service-serial-1"));

        assert!(state.has_valid_certificate("git-observer", "pod-uid-1"));
        assert!(state.has_agent_escrow_certificate("git-observer"));
        assert_eq!(
            state.certificate_for("git-observer").unwrap().serial,
            "client-serial-1"
        );
        assert_eq!(
            state
                .agent_escrow_certificate_for("git-observer")
                .unwrap()
                .serial,
            "service-serial-1"
        );
    }

    #[test]
    fn a_second_agent_escrow_issuance_for_the_same_identity_overwrites_the_first() {
        // Mirrors reissue-supersession's own spirit for ordinary
        // certificates -- the newest one wins in derived state, even
        // though this map (deliberately, per its own doc comment)
        // doesn't yet track revocation the way valid_certificates
        // does for the superseded entry.
        let mut state = CredentialsState::new();
        state.apply(&agent_escrow_issued("git-observer", "pod-uid-1", "service-serial-1"));
        state.apply(&agent_escrow_issued("git-observer", "pod-uid-2", "service-serial-2"));

        let record = state.agent_escrow_certificate_for("git-observer").unwrap();
        assert_eq!(record.serial, "service-serial-2");
        assert_eq!(record.instance_binding, "pod-uid-2");
    }
}
