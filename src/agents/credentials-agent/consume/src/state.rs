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
use std::collections::{HashMap, HashSet};

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
    pub capabilities: Vec<cert_issuer_common::capability::Capability>,
    pub provisioned_by: String,
    pub timestamp: i64,
    pub reason: GrantReason,
    /// True only for the one-time bootstrap account. Carried to the
    /// certificate record when the claim succeeds, and what makes
    /// `bootstrap_completed` true then -- not at provisioning, so a
    /// claim that fails can be retried.
    pub via_bootstrap: bool,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub title: Option<String>,
    pub telephone_number: Option<String>,
}

/// A currently-issued certificate for a Polar account, keyed on
/// account_id alone. Carries the permissions the account was
/// provisioned with (copied from the pending record when the
/// certificate is folded, so they survive the claim) and is marked
/// revoked in place, never removed, so the audit trail survives.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountCertificateRecord {
    pub account_id: String,
    pub username: String,
    pub serial: String,
    pub timestamp: i64,
    pub certificate_pem: String,
    pub permissions: Vec<cert_issuer_common::permission::Permission>,
    pub capabilities: Vec<cert_issuer_common::capability::Capability>,
    /// True for the one-time bootstrap account's certificate. The
    /// bootstrap cert is valid for exactly one provisioning; the admin-action
    /// handler counts what it has already authorised.
    pub via_bootstrap: bool,
    /// How many `ProvisionAccount` admin actions this account's certificate
    /// has authorised. Counted when the action is recorded, whether or
    /// not its derived effect then succeeds: a recorded action is a spent
    /// one. The bootstrap certificate is valid for exactly one.
    pub provisionings_made: u32,
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

    /// `serial -> account_id`, so a `Revoke` (which only carries a
    /// serial) and a signed-action lookup can find an account's
    /// certificate without a scan.
    account_serial_index: HashMap<String, String>,

    /// True once the one-time admin-bootstrap path has ever
    /// succeeded -- checked before allowing a via_bootstrap
    /// AccountProvisioned to fold a second time.
    bootstrap_completed: bool,

    /// Hex hashes of every admin-action payload already recorded --
    /// what refuses a replayed signed action. Rebuilt by replay.
    admin_action_hashes: HashSet<String>,
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
                if let Some(account_id) = self.account_serial_index.get(serial)
                    && let Some(record) = self.account_certificates.get_mut(account_id)
                    && record.serial == *serial
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
                capabilities,
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
                        capabilities: capabilities.clone(),
                        provisioned_by: provisioned_by.clone(),
                        timestamp: *timestamp,
                        reason: *reason,
                        via_bootstrap: *via_bootstrap,
                        given_name: given_name.clone(),
                        surname: surname.clone(),
                        title: title.clone(),
                        telephone_number: telephone_number.clone(),
                    },
                );
            }
            CredentialsEvent::AccountCertificateIssued {
                account_id,
                username,
                serial,
                timestamp,
                certificate_pem,
            } => {
                // Carry the provisioned permissions and capabilities
                // across the claim. A missing pending record can't
                // happen through the handlers, and if it ever did, none
                // is the safe default.
                let (permissions, capabilities, via_bootstrap) = self
                    .active_provisioned_accounts
                    .remove(username)
                    .map(|pending| {
                        (
                            pending.permissions,
                            pending.capabilities,
                            pending.via_bootstrap,
                        )
                    })
                    .unwrap_or_default();
                // Bootstrap is complete when its certificate exists, not
                // when its account was provisioned: a claim that failed
                // part-way leaves the pending record and can be retried.
                if via_bootstrap {
                    self.bootstrap_completed = true;
                }
                self.account_serial_index
                    .insert(serial.clone(), account_id.clone());
                self.account_certificates.insert(
                    account_id.clone(),
                    AccountCertificateRecord {
                        account_id: account_id.clone(),
                        username: username.clone(),
                        serial: serial.clone(),
                        timestamp: *timestamp,
                        certificate_pem: certificate_pem.clone(),
                        permissions,
                        capabilities,
                        via_bootstrap,
                        provisionings_made: 0,
                        revoked: false,
                        revoked_reason: None,
                        revoked_by: None,
                        revoked_at: None,
                    },
                );
            }
            CredentialsEvent::AdminActionRecorded {
                payload_hash,
                signed,
                author_account_id,
                ..
            } => {
                self.admin_action_hashes.insert(payload_hash.clone());
                // Count provisionings per author certificate, so the
                // bootstrap certificate can be held to exactly one.
                if let Ok(body) = signed.decode_body()
                    && matches!(
                        body.action,
                        credentials_agent_common::admin_action::AdminAction::ProvisionAccount { .. }
                    )
                    && let Some(author) = self.account_certificates.get_mut(author_account_id)
                {
                    author.provisionings_made += 1;
                }
                // The effect is derived, never stored separately: one
                // append, one fold. Also returns any serials the
                // derived event superseded, for the projection.
                if let Some(effect) = event.admin_effect() {
                    superseded_serials = self.apply(&effect);
                }
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

    /// The account certificate with this serial, revoked or not: the
    /// caller decides what a revoked one means. Backs signed-admin-action
    /// verification, which arrives carrying only the author's serial.
    pub fn account_certificate_by_serial(&self, serial: &str) -> Option<&AccountCertificateRecord> {
        self.account_serial_index
            .get(serial)
            .and_then(|account_id| self.account_certificates.get(account_id))
            .filter(|record| record.serial == serial)
    }

    /// The account that holds (or last held) this username, if any.
    /// Re-provisioning after a revocation reuses its account_id, so the
    /// identity survives credential replacement.
    pub fn account_for_username(&self, username: &str) -> Option<&AccountCertificateRecord> {
        self.account_certificates
            .values()
            .find(|record| record.username == username)
    }

    /// True if a signed admin action with this payload hash was already
    /// recorded -- the replay check.
    pub fn admin_action_seen(&self, payload_hash: &str) -> bool {
        self.admin_action_hashes.contains(payload_hash)
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

    fn agent_escrow_issued(
        identity: &str,
        instance_binding: &str,
        serial: &str,
    ) -> CredentialsEvent {
        CredentialsEvent::AgentEscrowIssuanceCompleted {
            identity: identity.to_string(),
            instance_binding: instance_binding.to_string(),
            serial: serial.to_string(),
            timestamp: 2000,
            certificate_pem:
                "-----BEGIN CERTIFICATE-----\nfake-service-cert\n-----END CERTIFICATE-----\n"
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
        state.apply(&issued(
            "admin-bootstrap-identity",
            "admin-tool-uid",
            "serial-99",
        ));
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

        state.apply(&agent_escrow_issued(
            "git-observer",
            "pod-uid-1",
            "service-serial-1",
        ));

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
        state.apply(&agent_escrow_issued(
            "git-observer",
            "pod-uid-1",
            "service-serial-1",
        ));

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
        state.apply(&agent_escrow_issued(
            "git-observer",
            "pod-uid-1",
            "service-serial-1",
        ));
        state.apply(&agent_escrow_issued(
            "git-observer",
            "pod-uid-2",
            "service-serial-2",
        ));

        let record = state.agent_escrow_certificate_for("git-observer").unwrap();
        assert_eq!(record.serial, "service-serial-2");
        assert_eq!(record.instance_binding, "pod-uid-2");
    }

    fn provisioned(username: &str, account_id: &str) -> CredentialsEvent {
        CredentialsEvent::AccountProvisioned {
            account_id: account_id.to_string(),
            username: username.to_string(),
            email: format!("{username}@example.org"),
            permissions: vec![cert_issuer_common::permission::Permission {
                scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                target: None,
            }],
            capabilities: vec![cert_issuer_common::capability::Capability::Grant],
            provisioned_by: "admin".to_string(),
            timestamp: 1000,
            reason: GrantReason::UserOnboarding,
            via_bootstrap: false,
            given_name: None,
            surname: None,
            title: None,
            telephone_number: None,
        }
    }

    fn account_issued(username: &str, account_id: &str, serial: &str) -> CredentialsEvent {
        CredentialsEvent::AccountCertificateIssued {
            account_id: account_id.to_string(),
            username: username.to_string(),
            serial: serial.to_string(),
            timestamp: 2000,
            certificate_pem:
                "-----BEGIN CERTIFICATE-----\nfake-account\n-----END CERTIFICATE-----\n".to_string(),
        }
    }

    #[test]
    fn account_certificate_keeps_the_provisioned_capabilities_after_the_claim() {
        let mut state = CredentialsState::new();
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s1"));

        let record = state.account_certificate_by_serial("s1").unwrap();
        assert_eq!(
            record.capabilities,
            vec![cert_issuer_common::capability::Capability::Grant]
        );
    }

    #[test]
    fn account_certificate_keeps_the_provisioned_permissions_after_the_claim() {
        let mut state = CredentialsState::new();
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s1"));

        assert!(state.provisioned_account_for("dana").is_none());
        let record = state.account_certificate_by_serial("s1").unwrap();
        assert_eq!(record.account_id, "acct-1");
        assert_eq!(record.permissions.len(), 1);
        assert!(!record.revoked);
    }

    #[test]
    fn revoking_an_account_serial_marks_it_revoked_in_place() {
        let mut state = CredentialsState::new();
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s1"));
        state.apply(&revoked("s1"));

        let record = state.account_certificate_by_serial("s1").unwrap();
        assert!(record.revoked);
        assert_eq!(record.revoked_by.as_deref(), Some("admin"));
    }

    #[test]
    fn a_stale_revocation_does_not_revoke_the_replacement_certificate() {
        let mut state = CredentialsState::new();
        // Dana's first certificate is issued, then revoked.
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s1"));
        state.apply(&revoked("s1"));
        // An admin re-provisions Dana (same account_id); certificate s2 is issued.
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s2"));
        // The old revocation for s1 arrives again.
        state.apply(&revoked("s1"));

        assert!(!state.account_certificate_by_serial("s2").unwrap().revoked);
    }

    #[test]
    fn a_replaced_certificates_serial_no_longer_resolves_to_an_account() {
        let mut state = CredentialsState::new();
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s1"));
        state.apply(&revoked("s1"));
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s2"));

        assert!(state.account_certificate_by_serial("s1").is_none());
        assert_eq!(
            state
                .account_certificate_by_serial("s2")
                .unwrap()
                .account_id,
            "acct-1"
        );
    }

    #[test]
    fn account_for_username_survives_revocation() {
        let mut state = CredentialsState::new();
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s1"));
        state.apply(&revoked("s1"));

        assert_eq!(
            state.account_for_username("dana").unwrap().account_id,
            "acct-1"
        );
        assert!(state.account_for_username("nobody").is_none());
    }

    #[test]
    fn a_recorded_grant_takes_effect_and_its_hash_is_remembered_across_replay() {
        use credentials_agent_common::admin_action::{
            AdminAction, AdminActionBody, SignedAdminAction,
        };
        let payload = AdminActionBody {
            nonce: "n1".to_string(),
            action: AdminAction::Grant {
                target_identity: "alice".to_string(),
                permissions: vec![],
                reason: GrantReason::UserOnboarding,
            },
        }
        .to_payload()
        .unwrap();
        let signed = SignedAdminAction {
            payload,
            author_serial: "a1".to_string(),
            signature: vec![],
        };
        let hash = signed.payload_hash();
        let event = CredentialsEvent::AdminActionRecorded {
            payload_hash: hash.clone(),
            signed,
            author_account_id: "admin-acct".to_string(),
            timestamp: 5000,
            account_id: None,
        };

        let state = CredentialsState::from_events(&[event]);

        assert_eq!(state.grant_for("alice").unwrap().granted_by, "admin-acct");
        assert!(state.admin_action_seen(&hash));
        assert!(!state.admin_action_seen("some-other-hash"));
    }
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    use credentials_agent_common::events::GrantReason;

    fn bootstrap_provisioned() -> CredentialsEvent {
        CredentialsEvent::AccountProvisioned {
            account_id: "acct-1".to_string(),
            username: "bootstrap".to_string(),
            email: "bootstrap@polar.invalid".to_string(),
            permissions: vec![],
            capabilities: vec![cert_issuer_common::capability::Capability::ProvisionAccount],
            provisioned_by: "bootstrap".to_string(),
            timestamp: 1,
            reason: GrantReason::UserOnboarding,
            via_bootstrap: true,
            given_name: None,
            surname: None,
            title: None,
            telephone_number: None,
        }
    }

    fn bootstrap_certificate() -> CredentialsEvent {
        CredentialsEvent::AccountCertificateIssued {
            account_id: "acct-1".to_string(),
            username: "bootstrap".to_string(),
            serial: "aa".to_string(),
            timestamp: 2,
            certificate_pem: "pem".to_string(),
        }
    }

    #[test]
    fn provisioning_the_bootstrap_account_does_not_complete_bootstrap() {
        let mut state = CredentialsState::default();
        state.apply(&bootstrap_provisioned());
        assert!(!state.bootstrap_completed());
    }

    #[test]
    fn issuing_the_bootstrap_certificate_completes_bootstrap() {
        let mut state = CredentialsState::default();
        state.apply(&bootstrap_provisioned());
        state.apply(&bootstrap_certificate());
        assert!(state.bootstrap_completed());
    }

    #[test]
    fn the_bootstrap_certificate_record_remembers_where_it_came_from() {
        let mut state = CredentialsState::default();
        state.apply(&bootstrap_provisioned());
        state.apply(&bootstrap_certificate());
        let record = state
            .account_certificate_by_serial("aa")
            .expect("the certificate resolves by serial");
        assert!(record.via_bootstrap);
    }
}

#[cfg(test)]
mod provisioning_count_tests {
    use super::*;
    use cert_issuer_common::capability::Capability;
    use credentials_agent_common::admin_action::{AdminAction, AdminActionBody, SignedAdminAction};
    use credentials_agent_common::events::GrantReason;

    fn bootstrap_account_with_certificate() -> CredentialsState {
        let mut state = CredentialsState::new();
        state.apply(&CredentialsEvent::AccountProvisioned {
            account_id: "acct-boot".to_string(),
            username: "polar-bootstrap".to_string(),
            email: "bootstrap@polar.invalid".to_string(),
            permissions: vec![],
            capabilities: vec![Capability::ProvisionAccount],
            provisioned_by: "polar-bootstrap".to_string(),
            timestamp: 1,
            reason: GrantReason::UserOnboarding,
            via_bootstrap: true,
            given_name: None,
            surname: None,
            title: None,
            telephone_number: None,
        });
        state.apply(&CredentialsEvent::AccountCertificateIssued {
            account_id: "acct-boot".to_string(),
            username: "polar-bootstrap".to_string(),
            serial: "b1".to_string(),
            timestamp: 2,
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                .to_string(),
        });
        state
    }

    fn recorded(
        action: AdminAction,
        nonce: &str,
        author_account_id: &str,
        account_id: Option<&str>,
    ) -> CredentialsEvent {
        let payload = AdminActionBody {
            nonce: nonce.to_string(),
            action,
        }
        .to_payload()
        .unwrap();
        let signed = SignedAdminAction {
            payload,
            author_serial: "b1".to_string(),
            signature: vec![],
        };
        CredentialsEvent::AdminActionRecorded {
            payload_hash: signed.payload_hash(),
            signed,
            author_account_id: author_account_id.to_string(),
            timestamp: 3,
            account_id: account_id.map(str::to_string),
        }
    }

    fn provision(username: &str) -> AdminAction {
        AdminAction::ProvisionAccount {
            username: username.to_string(),
            email: format!("{username}@example.org"),
            permissions: vec![],
            capabilities: vec![Capability::ProvisionAccount],
            reason: GrantReason::UserOnboarding,
            given_name: None,
            surname: None,
            title: None,
            telephone_number: None,
        }
    }

    fn grant() -> AdminAction {
        AdminAction::Grant {
            target_identity: "alice".to_string(),
            permissions: vec![],
            reason: GrantReason::UserOnboarding,
        }
    }

    #[test]
    fn a_new_certificate_has_authorised_nothing() {
        let state = bootstrap_account_with_certificate();
        let record = state.account_certificate_by_serial("b1").unwrap();
        assert!(record.via_bootstrap);
        assert_eq!(record.provisionings_made, 0);
    }

    #[test]
    fn a_recorded_provisioning_is_counted_against_its_author() {
        let mut state = bootstrap_account_with_certificate();
        state.apply(&recorded(
            provision("alice"),
            "n1",
            "acct-boot",
            Some("acct-2"),
        ));

        let record = state.account_certificate_by_serial("b1").unwrap();
        assert_eq!(record.provisionings_made, 1);
    }

    #[test]
    fn other_actions_are_not_counted_as_provisionings() {
        let mut state = bootstrap_account_with_certificate();
        state.apply(&recorded(grant(), "n1", "acct-boot", None));

        let record = state.account_certificate_by_serial("b1").unwrap();
        assert_eq!(record.provisionings_made, 0);
    }

    #[test]
    fn the_count_is_rebuilt_by_replaying_the_log() {
        let mut live = bootstrap_account_with_certificate();
        let event = recorded(provision("alice"), "n1", "acct-boot", Some("acct-2"));
        live.apply(&event);

        // A restart folds the same events again from empty.
        let replayed = CredentialsState::from_events(&[
            CredentialsEvent::AccountProvisioned {
                account_id: "acct-boot".to_string(),
                username: "polar-bootstrap".to_string(),
                email: "bootstrap@polar.invalid".to_string(),
                permissions: vec![],
                capabilities: vec![Capability::ProvisionAccount],
                provisioned_by: "polar-bootstrap".to_string(),
                timestamp: 1,
                reason: GrantReason::UserOnboarding,
                via_bootstrap: true,
                given_name: None,
                surname: None,
                title: None,
                telephone_number: None,
            },
            CredentialsEvent::AccountCertificateIssued {
                account_id: "acct-boot".to_string(),
                username: "polar-bootstrap".to_string(),
                serial: "b1".to_string(),
                timestamp: 2,
                certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                    .to_string(),
            },
            event,
        ]);

        assert_eq!(
            replayed
                .account_certificate_by_serial("b1")
                .unwrap()
                .provisionings_made,
            live.account_certificate_by_serial("b1")
                .unwrap()
                .provisionings_made
        );
        assert_eq!(
            replayed
                .account_certificate_by_serial("b1")
                .unwrap()
                .provisionings_made,
            1
        );
    }
}
