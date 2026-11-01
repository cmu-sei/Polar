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

use credentials_agent_common::events::{
    CredentialsEvent, GrantReason, ReportedCertificate, RevocationReason,
};
use credentials_agent_common::issuance::WorkloadCertType;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// How long after its certificate ends a revoked serial is still published: for verifiers whose clocks run behind. Two
/// hours, twice the life of a client certificate (the maintainer, 2026-10-27).
pub const REVOCATION_MARGIN_SECS: i64 = 2 * 3600;

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
    /// Which of the pod instance's certificates this is.
    pub cert_type: WorkloadCertType,
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
    /// The pod UID its certificate names as its instance, when its workload is multi-instance: a reissue supersedes only
    /// a certificate of the same instance (docs/architecture/authority-model.md, "Multi-instance workloads").
    pub instance: Option<String>,
    pub revoked_at: Option<i64>,
    /// Learned from cert-issuer's record, not issued by the credentials agent itself: a bootstrap service's, or one
    /// it asked for and lost (docs/architecture/cert-issuer-isolation.md, "Step 2").
    pub reported: bool,
}

/// A currently-issued agent escrow-identity (CertType::Service)
/// certificate -- keyed on identity alone, per
/// AgentEscrowIssuanceCompleted's own doc comment, not
/// (identity, instance_binding). Not marked revoked in place: a
/// Revoke of its serial puts it in the revoked set, and from then on
/// it is not the identity's current escrow certificate
/// (`CredentialsState::agent_escrow_certificate_for`), so it is not
/// encrypted to and does not stop the identity being issued another.
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

/// A secret as the ledger holds it: the ciphertext, and the identity that escrowed it, which is the owner
/// policy speaks of. Both are the ledger's word; nothing a request says changes either.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSecret {
    pub secret: credentials_agent_common::escrow_secret::EscrowedSecret,
    pub stored_by: String,
    /// What it was stored with. Policy speaks of them (`resource.kind`), and they cannot change.
    pub labels: std::collections::BTreeMap<String, String>,
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
    /// instance_binding, type)` -- exactly what the renewal-prevention
    /// check needs (see credentials-agent-architecture.md, "Pods
    /// cannot request their own renewal"): a pod instance may hold one
    /// client certificate and one server certificate, and may not be
    /// issued a second of either.
    valid_certificates: HashMap<(String, String, WorkloadCertType), CertificateRecord>,

    /// `serial -> (identity, instance_binding, type)`, so a `Revoke` event
    /// (which only ever carries a serial) can find and remove the
    /// matching entry above without a linear scan over every
    /// currently-valid certificate.
    serial_index: HashMap<String, (String, String, WorkloadCertType)>,

    /// The current, escrowed secret for each name -- keyed on the
    /// storing agent's own chosen name. A second SecretStored for the
    /// same name overwrites the entry here (derived state only; the
    /// event log itself never rewrites history, so the superseded
    /// EscrowedSecret remains recoverable by replay regardless).
    stored_secrets: HashMap<String, StoredSecret>,

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

    /// The serial of every certificate revoked, whether or not a current record matches it: by an explicit
    /// `Revoke`, or superseded by a reissue for its identity and type, which is published as revoked too
    /// (docs/architecture/deployments.md, "The renewal rule"). Kept apart from the records because a reissue
    /// replaces a record, and a revoked certificate must go on being refused for as long as it would otherwise
    /// be valid.
    revoked_serials: BTreeSet<String>,
    /// Those of `revoked_serials` an administrator revoked for a compromise (`RevocationReason::Compromised`): the
    /// revoked set says so as it travels (`AuthorityBody::Revoked`); every other revocation is an ordinary end.
    compromised_serials: BTreeSet<String>,
    /// The serial of every certificate the ledger has recorded, by any issuance or report: what a page of cert-issuer's
    /// record is checked against, so nothing is recorded twice.
    recorded_serials: BTreeSet<String>,
    /// When each revoked serial's certificate ends, where the ledger knew the certificate as the serial was revoked
    /// (docs/architecture/authority-distribution.md, "Pruning the revoked set"): kept here because the record that held
    /// the certificate may be replaced since (a pod issued again in place).
    revoked_until: BTreeMap<String, i64>,
    /// Where cert-issuer's record has been read to: its log, and its last entry read.
    record_cursor: Option<(String, u64)>,

    /// The highest sequence number published to the agents (`AuthorityPublished`).
    last_authority_sequence: u64,
}

impl CredentialsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the ledger has recorded a certificate with this serial, by any issuance or report.
    pub fn knows_serial(&self, serial: &str) -> bool {
        self.recorded_serials.contains(serial)
    }

    /// Where cert-issuer's record has been read to: its log, and its last entry read. `None` before any page.
    pub fn record_cursor(&self) -> Option<(String, u64)> {
        self.record_cursor.clone()
    }

    /// Records a client or server certificate, issued or reported, and returns the serials it supersedes.
    #[allow(clippy::too_many_arguments)]
    fn record_workload_certificate(
        &mut self,
        identity: &str,
        instance_binding: &str,
        cert_type: WorkloadCertType,
        serial: &str,
        timestamp: i64,
        certificate_pem: &str,
        reported: bool,
    ) -> Vec<String> {
        let mut superseded = Vec::new();
        self.recorded_serials.insert(serial.to_string());
        // A reissue for the same identity supersedes any
        // prior, still-valid certificate of the same type for it
        // -- regardless of instance_binding, since a pod restart
        // gets a fresh one -- and, for a multi-instance workload, of the
        // same instance: the pod its certificate names (a pod restarted in
        // place, the same UID, still supersedes its own; two pods do not;
        // cert_issuer_common::instance::same_instance). A certificate whose
        // instance names do not read is taken as naming none. A client certificate and a server
        // certificate are different things, and neither
        // retires the other. Marked revoked in place, not
        // removed, so the record survives. Done before
        // inserting the new entry, so this can't mark the new
        // one itself.
        let instance = cert_issuer_common::instance::extract_instance(certificate_pem)
            .ok()
            .flatten()
            .map(|instance| instance.uid);
        for record in self.valid_certificates.values_mut() {
            if record.identity == identity
                && record.cert_type == cert_type
                && !record.revoked
                && cert_issuer_common::instance::same_instance(
                    instance.as_deref(),
                    record.instance.as_deref(),
                )
            {
                record.revoked = true;
                record.revoked_reason = Some(RevocationReason::Superseded);
                record.revoked_by = Some("system".to_string());
                record.revoked_at = Some(timestamp);
                superseded.push(record.serial.clone());
                self.revoked_serials.insert(record.serial.clone());
                if let Some(end) =
                    cert_issuer_common::serial::not_after_from_pem(&record.certificate_pem)
                {
                    self.revoked_until.insert(record.serial.clone(), end);
                }
            }
        }

        let key = (
            identity.to_string(),
            instance_binding.to_string(),
            cert_type,
        );
        // The same pod issued again (restarted in place): its old serial no longer names this record.
        if let Some(old) = self.valid_certificates.get(&key) {
            self.serial_index.remove(&old.serial);
        }
        self.valid_certificates.insert(
            key.clone(),
            CertificateRecord {
                identity: identity.to_string(),
                instance_binding: instance_binding.to_string(),
                cert_type,
                serial: serial.to_string(),
                timestamp,
                certificate_pem: certificate_pem.to_string(),
                revoked: false,
                revoked_reason: None,
                revoked_by: None,
                revoked_at: None,
                reported,
                instance,
            },
        );
        self.serial_index.insert(serial.to_string(), key);
        superseded
    }

    /// The event a page of cert-issuer's record makes, or none: the certificates in it the ledger has not recorded, each
    /// flagged unexpected when it was signed on a token alone for an identity not in `bootstrap` (the authority model's
    /// bootstrap services; with no model, `None`, nothing can be called unexpected), and where the record was read to,
    /// which in the same log never moves back (an answer to an older request can arrive after a newer one). None when
    /// the page holds nothing new and moves nothing.
    pub fn reported_page(
        &self,
        page: &cert_issuer_common::record::RecordReply,
        bootstrap: Option<&BTreeSet<String>>,
        now: i64,
    ) -> Option<CredentialsEvent> {
        use cert_issuer_common::record::IssuePath;
        // Certificates and tickets share the record's numbers.
        let last = page
            .entries
            .iter()
            .map(|e| e.seq)
            .chain(page.tickets.iter().map(|t| t.seq))
            .max()?;
        // In the same log the cursor never moves back: a page can arrive after a later one.
        let read = match &self.record_cursor {
            Some((log, read)) if *log == page.log => Some(*read),
            _ => None,
        };
        let through = read.map_or(last, |read| read.max(last));
        let certificates: Vec<ReportedCertificate> = page
            .entries
            .iter()
            .filter(|e| !self.knows_serial(&e.serial))
            .map(|e| ReportedCertificate {
                seq: e.seq,
                identity: e.identity.clone(),
                instance_binding: e.instance_binding.clone(),
                cert_type: e.cert_type.clone(),
                path: e.path,
                serial: e.serial.clone(),
                signed_at: e.signed_at,
                not_after: e.not_after,
                certificate_pem: e.certificate_pem.clone(),
                // A bootstrap service's custodian is as expected as the service (docs/architecture/key-custody.md,
                // "Part A's bootstrap: the custodian").
                unexpected: e.path == IssuePath::TokenOnly
                    && bootstrap.is_some_and(|b| {
                        let identity = cert_issuer_common::custody::custodian_of(&e.identity)
                            .unwrap_or(&e.identity);
                        !b.contains(identity)
                    }),
            })
            .collect();
        // A ticket has no serial: what is new of them is what is past where this log was read to.
        let tickets: Vec<credentials_agent_common::events::ReportedTicket> = page
            .tickets
            .iter()
            .filter(|t| read.is_none_or(|read| t.seq > read))
            .map(|t| credentials_agent_common::events::ReportedTicket {
                seq: t.seq,
                identity: t.identity.clone(),
                instance_binding: t.instance_binding.clone(),
                taken_at: t.taken_at,
                expires_at: t.expires_at,
                issued_at: t.issued_at,
            })
            .collect();
        if certificates.is_empty() && tickets.is_empty() && read.is_some_and(|read| through <= read)
        {
            return None;
        }
        Some(CredentialsEvent::CertificatesReported {
            log: page.log.clone(),
            through,
            certificates,
            tickets,
            timestamp: now,
        })
    }

    /// Records a certificate cert-issuer reported: a client or server certificate as an issued one, an agent's escrow
    /// certificate as its escrow certificate, and an account's by its serial alone, since which account it is cannot
    /// be known from the record. Returns the serials it supersedes.
    fn record_reported(&mut self, c: &ReportedCertificate) -> Vec<String> {
        use cert_issuer_common::CertType;
        use cert_issuer_common::record::IssuePath;
        self.recorded_serials.insert(c.serial.clone());
        let binding = c.instance_binding.clone().unwrap_or_default();
        match (c.path, &c.cert_type) {
            (IssuePath::Internal, _) => Vec::new(),
            (_, CertType::Service) => {
                self.agent_escrow_certificates.insert(
                    c.identity.clone(),
                    AgentEscrowCertificateRecord {
                        instance_binding: binding,
                        serial: c.serial.clone(),
                        timestamp: c.signed_at,
                        certificate_pem: c.certificate_pem.clone(),
                    },
                );
                Vec::new()
            }
            (_, cert_type) => {
                let cert_type = match cert_type {
                    CertType::Server => WorkloadCertType::Server,
                    CertType::Custody => WorkloadCertType::Custody,
                    _ => WorkloadCertType::Client,
                };
                // cert-issuer's record says when in seconds; the ledger's times are milliseconds.
                self.record_workload_certificate(
                    &c.identity,
                    &binding,
                    cert_type,
                    &c.serial,
                    c.signed_at.saturating_mul(1000),
                    &c.certificate_pem,
                    true,
                )
            }
        }
    }

    /// The serial of every certificate that was explicitly revoked, in order.
    pub fn revoked_serials(&self) -> Vec<String> {
        self.revoked_serials.iter().cloned().collect()
    }

    /// The certificate the ledger holds for `serial` now: a workload's, an account's or an agent's escrow certificate.
    fn certificate_of(&self, serial: &str) -> Option<String> {
        if let Some(key) = self.serial_index.get(serial)
            && let Some(record) = self.valid_certificates.get(key)
        {
            return Some(record.certificate_pem.clone());
        }
        if let Some(account_id) = self.account_serial_index.get(serial)
            && let Some(record) = self.account_certificates.get(account_id)
            && record.serial == serial
        {
            return Some(record.certificate_pem.clone());
        }
        self.agent_escrow_certificates
            .values()
            .find(|record| record.serial == serial)
            .map(|record| record.certificate_pem.clone())
    }

    /// The revoked set as it is published at `now` (Unix seconds), and its version
    /// (docs/architecture/authority-distribution.md, "Pruning the revoked set"): every revoked serial but those whose
    /// certificate ended more than `REVOCATION_MARGIN_SECS` ago, since every verifier refuses an expired certificate on
    /// its own. The version is the serials ever revoked plus those ended: it rises with each and never falls while time
    /// goes forward, and is never below the size, which was the version before.
    pub fn published_revocations(&self, now: i64) -> (Vec<String>, u64) {
        let published: Vec<String> = self
            .revoked_serials
            .iter()
            .filter(|serial| {
                self.revoked_until
                    .get(*serial)
                    .is_none_or(|end| now - end <= REVOCATION_MARGIN_SECS)
            })
            .cloned()
            .collect();
        let ended = self.revoked_serials.len() - published.len();
        let version = (self.revoked_serials.len() + ended) as u64;
        (published, version)
    }

    /// Those of `serials` revoked for a compromise, in the order given: what the published revoked set says of them; every
    /// other is an ordinary end, a supersession included.
    pub fn compromised_among(&self, serials: &[String]) -> Vec<String> {
        serials
            .iter()
            .filter(|serial| self.compromised_serials.contains(*serial))
            .cloned()
            .collect()
    }

    /// The highest sequence number published to the agents; 0 before any.
    pub fn last_authority_sequence(&self) -> u64 {
        self.last_authority_sequence
    }

    /// Every account certificate the ledger holds that is not revoked: the persons the authority model judges a
    /// change for (docs/architecture/authority-model.md, "Decisions": every account in the ledger).
    pub fn active_account_certificates(&self) -> Vec<AccountCertificateRecord> {
        let mut active: Vec<AccountCertificateRecord> = self
            .account_certificates
            .values()
            .filter(|record| !record.revoked)
            .cloned()
            .collect();
        active.sort_by(|a, b| a.account_id.cmp(&b.account_id));
        active
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
                cert_type,
                certificate_pem,
                grant_key: _,
            } => {
                // Single-use: remove the matching grant, if any. A
                // harmless no-op when there isn't one -- exactly what
                // lets admin bootstrap (which has no grant at all)
                // fold through this same logic with zero
                // special-casing. Only a client certificate uses it up:
                // a grant is permissions, which a server certificate
                // does not carry, and a server certificate issued first
                // must not take them from the client certificate that
                // follows.
                if let Some(key) = event.consumed_grant_key() {
                    self.active_grants.remove(key);
                }

                superseded_serials.extend(self.record_workload_certificate(
                    identity,
                    instance_binding,
                    *cert_type,
                    serial,
                    *timestamp,
                    certificate_pem,
                    false,
                ));
            }
            CredentialsEvent::Revoke {
                serial,
                reason,
                revoked_by,
                timestamp,
            } => {
                self.revoked_serials.insert(serial.clone());
                if reason.is_compromise() {
                    self.compromised_serials.insert(serial.clone());
                }
                if let Some(pem) = self.certificate_of(serial)
                    && let Some(end) = cert_issuer_common::serial::not_after_from_pem(&pem)
                {
                    self.revoked_until.entry(serial.clone()).or_insert(end);
                }
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
            CredentialsEvent::AuthorityPublished { sequence, .. } => {
                self.last_authority_sequence = self.last_authority_sequence.max(*sequence);
            }
            // A refused document changes nothing the state holds.
            CredentialsEvent::AuthorityRefused { .. } => {}
            // A page of cert-issuer's record: each certificate as the kind it is, and where the record was read to.
            CredentialsEvent::CertificatesReported {
                log,
                through,
                certificates,
                ..
            } => {
                for c in certificates {
                    superseded_serials.extend(self.record_reported(c));
                }
                self.record_cursor = Some((log.clone(), *through));
            }
            CredentialsEvent::SecretStored {
                name,
                secret,
                stored_by,
                labels,
                ..
            } => {
                // Overwrites any prior entry for this name -- the
                // event log itself still holds the superseded
                // SecretStored event in full, so nothing is ever
                // truly lost, only no longer current.
                self.stored_secrets.insert(
                    name.clone(),
                    StoredSecret {
                        secret: secret.clone(),
                        stored_by: stored_by.clone(),
                        labels: labels.clone(),
                    },
                );
            }
            CredentialsEvent::AgentEscrowIssuanceCompleted {
                identity,
                instance_binding,
                serial,
                timestamp,
                certificate_pem,
            } => {
                self.recorded_serials.insert(serial.clone());
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
                self.recorded_serials.insert(serial.clone());
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

    /// When `identity`'s pod `instance_binding` was last issued a certificate of `cert_type`, in milliseconds,
    /// revoked or not: what the renewal bound is measured from.
    pub fn last_issued(
        &self,
        identity: &str,
        instance_binding: &str,
        cert_type: WorkloadCertType,
    ) -> Option<i64> {
        self.valid_certificates
            .get(&(
                identity.to_string(),
                instance_binding.to_string(),
                cert_type,
            ))
            .map(|record| record.timestamp)
    }

    /// The key (its SubjectPublicKeyInfo) of the certificate [`last_issued`](Self::last_issued) dates, when it reads:
    /// which request the renewal rule's bound runs from (`crate::renewal::Admissions`).
    pub fn last_issued_key(
        &self,
        identity: &str,
        instance_binding: &str,
        cert_type: WorkloadCertType,
    ) -> Option<Vec<u8>> {
        self.valid_certificates
            .get(&(
                identity.to_string(),
                instance_binding.to_string(),
                cert_type,
            ))
            .and_then(|record| {
                cert_issuer_common::public_key::certificate_key(&record.certificate_pem)
            })
    }

    /// True if this exact `(identity, instance_binding)` pair
    /// currently holds a valid, unrevoked certificate. Not the renewal
    /// rule, which is [`crate::renewal::Admissions`]: a pod restarted in
    /// place holds one and is issued again.
    pub fn has_valid_certificate(
        &self,
        identity: &str,
        instance_binding: &str,
        cert_type: WorkloadCertType,
    ) -> bool {
        self.valid_certificates
            .get(&(
                identity.to_string(),
                instance_binding.to_string(),
                cert_type,
            ))
            .is_some_and(|record| !record.revoked)
    }
    /// The current, non-revoked certificate for a given identity, if
    /// any -- regardless of instance_binding, since a caller looking
    /// up another agent's cert has no way of knowing that in advance.
    /// A reissue for the same identity marks any prior entry revoked
    /// (see apply()'s own IssuanceCompleted handling), so there's at
    /// most one non-revoked entry per identity at any given time.
    /// The live custodian certificate `identity` holds: the custody daemon's own, named `custody.<X>`
    /// (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian").
    pub fn custodian_certificate_for(&self, identity: &str) -> Option<&CertificateRecord> {
        self.valid_certificates.values().find(|record| {
            record.identity == identity
                && record.cert_type == WorkloadCertType::Custody
                && !record.revoked
        })
    }

    /// The live custodian certificate of `identity` for the pod `instance` (a multi-instance workload's custodian's, its
    /// instance stamped by the broker): the one naming that pod, by the supersession rule
    /// (`cert_issuer_common::instance::same_instance`); with no instance, any live one, of which a single-instance
    /// workload has one.
    pub fn custodian_certificate_of(
        &self,
        identity: &str,
        instance: Option<&str>,
    ) -> Option<&CertificateRecord> {
        self.valid_certificates.values().find(|record| {
            record.identity == identity
                && record.cert_type == WorkloadCertType::Custody
                && !record.revoked
                && cert_issuer_common::instance::same_instance(record.instance.as_deref(), instance)
        })
    }

    pub fn certificate_for(&self, identity: &str) -> Option<&CertificateRecord> {
        // The requester's certificate is its client certificate. A server
        // certificate is the listener's, and says nothing of what the
        // requester may do.
        self.valid_certificates.values().find(|record| {
            record.identity == identity && record.cert_type.is_client() && !record.revoked
        })
    }

    /// The current, escrowed secret stored under `name`, if any --
    /// always the most recently stored one; a superseded prior value
    /// remains in the event log but is no longer returned here.
    /// Whether `by` may store a secret under `name` with `labels`: it may if no secret is held by that name, or
    /// if the one held was stored by `by` with the same labels. The identity that stored a secret owns it, and
    /// only it replaces it; and a label cannot change once stored, so a replacement carries the same ones. Another
    /// identity is told the name is taken before anything is said about labels.
    pub fn check_store(
        &self,
        name: &str,
        by: &str,
        labels: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), crate::ledger::SecretStoreError> {
        match self.stored_secrets.get(name) {
            None => Ok(()),
            Some(held) if held.stored_by != by => Err(crate::ledger::SecretStoreError::NameTaken),
            Some(held) if held.labels != *labels => {
                Err(crate::ledger::SecretStoreError::LabelsChanged)
            }
            Some(_) => Ok(()),
        }
    }

    pub fn secret_for(&self, name: &str) -> Option<&StoredSecret> {
        self.stored_secrets.get(name)
    }

    /// True if `identity` already holds a currently-issued agent
    /// escrow-identity certificate -- the check
    /// AgentEscrowIssuanceHandler uses in place of the pod-instance-
    /// scoped renewal-prevention check human/workload mTLS issuance
    /// uses instead.
    pub fn has_agent_escrow_certificate(&self, identity: &str) -> bool {
        self.agent_escrow_certificate_for(identity).is_some()
    }

    /// The agent escrow-identity certificate record for `identity`,
    /// if one has ever been issued -- backs the agent escrow-key
    /// query (credentials-agent-architecture.md's own "Escrow-key and
    /// certificate-lookup queries" section, third query).
    pub fn agent_escrow_certificate_for(
        &self,
        identity: &str,
    ) -> Option<&AgentEscrowCertificateRecord> {
        // A revoked escrow certificate is no one's to encrypt to, and does not count toward the one per identity: its
        // workload may be issued another (an orphan, issued and never stored, revoked by an administrator, is how a
        // workload stuck without its key gets one again).
        self.agent_escrow_certificates
            .get(identity)
            .filter(|record| !self.revoked_serials.contains(&record.serial))
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

    /// The certificate `identity` connects with, whose groups policy reads: its workload's current client certificate,
    /// or else its account's, while that is not revoked. None for anyone else.
    pub fn requester_certificate(&self, identity: &str) -> Option<&str> {
        self.certificate_for(identity)
            .or_else(|| self.custodian_certificate_for(identity))
            .map(|record| record.certificate_pem.as_str())
            .or_else(|| {
                self.account_certificate_for(identity)
                    .filter(|record| !record.revoked)
                    .map(|record| record.certificate_pem.as_str())
            })
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

    /// Each account's username and the account ID it is known by: every account with a certificate, revoked or not,
    /// and every one provisioned and not yet claimed. What the credentials agent converts a policy's or a groups
    /// document's usernames by (docs/architecture/authority-model.md, "Decisions").
    pub fn account_names(&self) -> std::collections::BTreeMap<String, String> {
        self.account_certificates
            .values()
            .map(|record| (record.username.clone(), record.account_id.clone()))
            .chain(
                self.active_provisioned_accounts
                    .iter()
                    .map(|(username, record)| (username.clone(), record.account_id.clone())),
            )
            .collect()
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
pub(crate) mod tests {
    use super::*;

    #[test]
    fn an_issuance_under_a_grant_key_uses_up_that_grant_and_not_one_named_like_the_certificate() {
        let mut state = CredentialsState::default();
        // A grant written from the identity provider's claim, and an unrelated one that happens to be named
        // like the certificate's identity.
        state.apply(&grant("jdoe"));
        state.apply(&grant("jdoe.polar-auth"));
        state.apply(&issued_under_grant(
            "jdoe.polar-auth",
            "jdoe",
            "pod-1",
            "s1",
        ));
        assert!(
            state.grant_for("jdoe").is_none(),
            "the grant it was issued under is used up"
        );
        assert!(
            state.grant_for("jdoe.polar-auth").is_some(),
            "a different grant is left alone"
        );
        assert!(state.certificate_for("jdoe.polar-auth").is_some());
        assert!(state.certificate_for("jdoe").is_none());
    }

    #[test]
    fn an_issuance_written_before_grant_keys_existed_uses_up_the_grant_under_its_identity() {
        let mut state = CredentialsState::default();
        state.apply(&grant("jdoe"));
        state.apply(&issued("jdoe", "pod-1", "s1"));
        assert!(state.grant_for("jdoe").is_none());
    }

    #[test]
    fn only_the_identity_that_stored_a_secret_may_store_that_name_again() {
        let keypair = crate::escrow::tests::test_keypair();
        let mut state = CredentialsState::default();
        assert_eq!(
            state.check_store("github-sei", "anyone", &Default::default()),
            Ok(()),
            "a free name is anyones"
        );
        state.apply(&CredentialsEvent::SecretStored {
            name: "github-sei".to_string(),
            secret: credentials_agent_common::escrow_secret::EscrowedSecret::encrypt(
                b"one",
                keypair.public_key_pem(),
            )
            .unwrap(),
            stored_by: "git-repo-observer".to_string(),
            timestamp: 1,
            labels: Default::default(),
        });
        let none = std::collections::BTreeMap::new();
        assert_eq!(
            state.check_store("github-sei", "git-repo-observer", &none),
            Ok(())
        );
        assert_eq!(
            state.check_store("github-sei", "someone-else", &none),
            Err(crate::ledger::SecretStoreError::NameTaken)
        );
        assert_eq!(
            state.check_store("github-sei", "git-repo-observer ", &none),
            Err(crate::ledger::SecretStoreError::NameTaken),
            "the name is exact"
        );
        assert_eq!(
            state.check_store("another-name", "someone-else", &none),
            Ok(())
        );
    }

    fn labelled(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn stored_with_labels(by: &str, labels: &[(&str, &str)]) -> CredentialsEvent {
        let keypair = crate::escrow::tests::test_keypair();
        CredentialsEvent::SecretStored {
            name: "git-repo-observer/github".to_string(),
            secret: credentials_agent_common::escrow_secret::EscrowedSecret::encrypt(
                b"one",
                keypair.public_key_pem(),
            )
            .unwrap(),
            stored_by: by.to_string(),
            timestamp: 1,
            labels: labelled(labels),
        }
    }

    #[test]
    fn a_stored_secret_keeps_its_labels() {
        let mut state = CredentialsState::default();
        state.apply(&stored_with_labels(
            "git-repo-observer",
            &[("kind", "token")],
        ));
        assert_eq!(
            state.secret_for("git-repo-observer/github").unwrap().labels,
            labelled(&[("kind", "token")])
        );
    }

    #[test]
    fn the_owner_may_replace_the_value_with_the_same_labels_and_not_with_different_ones() {
        let mut state = CredentialsState::default();
        state.apply(&stored_with_labels(
            "git-repo-observer",
            &[("kind", "token"), ("team", "a")],
        ));
        let name = "git-repo-observer/github";
        // The same labels, however they are listed: allowed.
        assert_eq!(
            state.check_store(
                name,
                "git-repo-observer",
                &labelled(&[("team", "a"), ("kind", "token")])
            ),
            Ok(())
        );
        for different in [
            labelled(&[("kind", "user-password"), ("team", "a")]),
            labelled(&[("kind", "token")]),
            labelled(&[("kind", "token"), ("team", "a"), ("extra", "x")]),
            labelled(&[]),
        ] {
            assert_eq!(
                state.check_store(name, "git-repo-observer", &different),
                Err(crate::ledger::SecretStoreError::LabelsChanged),
                "{different:?}"
            );
        }
    }

    #[test]
    fn another_identity_is_told_the_name_is_taken_before_anything_about_labels() {
        let mut state = CredentialsState::default();
        state.apply(&stored_with_labels(
            "git-repo-observer",
            &[("kind", "token")],
        ));
        assert_eq!(
            state.check_store(
                "git-repo-observer/github",
                "someone-else",
                &labelled(&[("kind", "other")])
            ),
            Err(crate::ledger::SecretStoreError::NameTaken)
        );
    }

    #[test]
    fn a_stored_secret_remembers_who_stored_it_and_a_later_store_replaces_both() {
        let keypair = crate::escrow::tests::test_keypair();
        let stored = |by: &str, plaintext: &[u8]| CredentialsEvent::SecretStored {
            name: "github-sei".to_string(),
            secret: credentials_agent_common::escrow_secret::EscrowedSecret::encrypt(
                plaintext,
                keypair.public_key_pem(),
            )
            .unwrap(),
            stored_by: by.to_string(),
            timestamp: 1,
            labels: Default::default(),
        };
        let mut state = CredentialsState::default();
        assert!(state.secret_for("github-sei").is_none());
        state.apply(&stored("git-repo-observer", b"one"));
        assert_eq!(
            state.secret_for("github-sei").unwrap().stored_by,
            "git-repo-observer"
        );
        state.apply(&stored("someone-else", b"two"));
        assert_eq!(
            state.secret_for("github-sei").unwrap().stored_by,
            "someone-else"
        );
    }

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

    fn issued_under_grant(
        identity: &str,
        grant_key: &str,
        instance_binding: &str,
        serial: &str,
    ) -> CredentialsEvent {
        match issued(identity, instance_binding, serial) {
            CredentialsEvent::IssuanceCompleted {
                identity,
                instance_binding,
                serial,
                timestamp,
                cert_type,
                certificate_pem,
                ..
            } => CredentialsEvent::IssuanceCompleted {
                identity,
                instance_binding,
                serial,
                timestamp,
                cert_type,
                grant_key: Some(grant_key.to_string()),
                certificate_pem,
            },
            other => other,
        }
    }

    fn issued(identity: &str, instance_binding: &str, serial: &str) -> CredentialsEvent {
        issued_as(identity, instance_binding, serial, WorkloadCertType::Client)
    }

    fn issued_as(
        identity: &str,
        instance_binding: &str,
        serial: &str,
        cert_type: WorkloadCertType,
    ) -> CredentialsEvent {
        CredentialsEvent::IssuanceCompleted {
            identity: identity.to_string(),
            instance_binding: instance_binding.to_string(),
            serial: serial.to_string(),
            timestamp: 2000,
            cert_type,
            grant_key: None,
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
    fn an_explicit_revocation_is_remembered_by_serial_even_when_no_certificate_matches_it() {
        let mut state = CredentialsState::new();
        assert!(state.revoked_serials().is_empty());
        // A certificate that was replaced since (or never seen here) is still revoked: the leaves must go on
        // refusing it for as long as it would otherwise be valid.
        state.apply(&revoked("0a"));
        state.apply(&issued("alice", "pod-1", "0b"));
        state.apply(&revoked("0b"));
        state.apply(&issued("alice", "pod-2", "0c"));
        assert_eq!(state.revoked_serials(), ["0a", "0b"]);
        // Revoking twice is one entry.
        state.apply(&revoked("0a"));
        assert_eq!(state.revoked_serials().len(), 2);
    }

    // docs/architecture/deployments.md, "The renewal rule": a superseded certificate is published as revoked, so the
    // agents refuse it, the old pod's custody daemon destroys its key, and the broker ends its sessions.
    #[test]
    fn a_certificate_replaced_by_a_reissue_is_revoked() {
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "pod-1", "0b"));
        state.apply(&issued("alice", "pod-2", "0c"));
        assert_eq!(state.revoked_serials(), ["0b"], "superseded is revoked");
    }

    // A pod restarted in place is issued again under its own UID: the new certificate takes the old one's place, and
    // the old serial, revoked, no longer names the new record.
    #[test]
    fn a_pod_reissued_in_place_is_not_revoked_by_its_old_serial() {
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "pod-1", "0b"));
        state.apply(&issued("alice", "pod-1", "0c"));
        assert_eq!(state.revoked_serials(), ["0b"]);
        state.apply(&revoked("0b"));
        assert!(state.has_valid_certificate("alice", "pod-1", WorkloadCertType::Client));
        assert_eq!(state.certificate_for("alice").unwrap().serial, "0c");
    }

    #[test]
    fn the_last_issuance_to_a_pod_is_its_newest_by_type() {
        let mut state = CredentialsState::new();
        assert_eq!(
            state.last_issued("alice", "pod-1", WorkloadCertType::Client),
            None
        );
        state.apply(&issued("alice", "pod-1", "0b"));
        assert_eq!(
            state.last_issued("alice", "pod-1", WorkloadCertType::Client),
            Some(2000)
        );
        assert_eq!(
            state.last_issued("alice", "pod-1", WorkloadCertType::Server),
            None
        );
        assert_eq!(
            state.last_issued("alice", "pod-2", WorkloadCertType::Client),
            None
        );
    }

    #[test]
    fn the_last_authority_sequence_is_the_highest_published_and_never_falls() {
        let published = |sequence| CredentialsEvent::AuthorityPublished {
            sequence,
            kind: "policy".to_string(),
            version: 1,
            timestamp: 4000,
            effect: None,
        };
        let mut state = CredentialsState::new();
        assert_eq!(state.last_authority_sequence(), 0);
        state.apply(&published(3));
        state.apply(&published(2));
        assert_eq!(state.last_authority_sequence(), 3);
        // Replay reaches the same place.
        let replayed = CredentialsState::from_events(&[published(1), published(5), published(4)]);
        assert_eq!(replayed.last_authority_sequence(), 5);
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
        assert!(state.has_valid_certificate("alice", "pod-uid-1", WorkloadCertType::Client));
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
        assert!(state.has_valid_certificate(
            "admin-bootstrap-identity",
            "admin-tool-uid",
            WorkloadCertType::Client
        ));
    }

    #[test]
    fn revoke_removes_the_certificate() {
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "pod-uid-1", "serial-1"));
        assert!(state.has_valid_certificate("alice", "pod-uid-1", WorkloadCertType::Client));

        state.apply(&revoked("serial-1"));
        assert!(!state.has_valid_certificate("alice", "pod-uid-1", WorkloadCertType::Client));
    }

    #[test]
    fn revoking_an_unknown_serial_is_a_harmless_no_op() {
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "pod-uid-1", "serial-1"));
        state.apply(&revoked("serial-does-not-exist"));
        // The real, still-valid certificate must be unaffected.
        assert!(state.has_valid_certificate("alice", "pod-uid-1", WorkloadCertType::Client));
    }

    #[test]
    fn rolling_restart_produces_a_different_instance_binding_correctly() {
        // Real property this whole design exists for: same identity,
        // different pod UID, must NOT be treated as already holding a
        // valid cert.
        let mut state = CredentialsState::new();
        state.apply(&issued("alice", "old-pod-uid", "serial-1"));

        assert!(state.has_valid_certificate("alice", "old-pod-uid", WorkloadCertType::Client));
        assert!(
            !state.has_valid_certificate("alice", "new-pod-uid", WorkloadCertType::Client),
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
        assert!(state.has_valid_certificate("alice", "pod-uid-1", WorkloadCertType::Client));
        assert!(!state.has_valid_certificate("bob", "pod-uid-1", WorkloadCertType::Client));
    }

    // ---- a pod instance holds one client and one server certificate ----------------------------------------

    use credentials_agent_common::issuance::WorkloadCertType::{Client, Server};

    #[test]
    fn a_pod_instance_may_hold_one_client_and_one_server_certificate() {
        let mut state = CredentialsState::new();
        state.apply(&issued_as("git-server", "pod-1", "client-1", Client));
        state.apply(&issued_as("git-server", "pod-1", "server-1", Server));
        assert!(state.has_valid_certificate("git-server", "pod-1", Client));
        assert!(state.has_valid_certificate("git-server", "pod-1", Server));
    }

    #[test]
    fn what_the_renewal_check_blocks_is_a_second_certificate_of_a_type_the_pod_already_holds() {
        let mut state = CredentialsState::new();
        state.apply(&issued_as("kube", "pod-1", "client-1", Client));
        // It holds a client certificate and no server one.
        assert!(state.has_valid_certificate("kube", "pod-1", Client));
        assert!(!state.has_valid_certificate("kube", "pod-1", Server));
        // Another pod of the same identity holds neither.
        assert!(!state.has_valid_certificate("kube", "pod-2", Client));
        assert!(!state.has_valid_certificate("kube", "pod-2", Server));
    }

    #[test]
    fn a_server_certificate_does_not_supersede_a_client_certificate_nor_the_reverse() {
        let mut state = CredentialsState::new();
        state.apply(&issued_as("x", "pod-1", "client-1", Client));
        let superseded = state.apply(&issued_as("x", "pod-1", "server-1", Server));
        assert!(superseded.is_empty(), "{superseded:?}");
        let superseded = state.apply(&issued_as("y", "pod-1", "server-2", Server));
        assert!(superseded.is_empty(), "{superseded:?}");
        let superseded = state.apply(&issued_as("y", "pod-1", "client-2", Client));
        assert!(superseded.is_empty(), "{superseded:?}");
        assert!(state.has_valid_certificate("x", "pod-1", Client));
        assert!(state.has_valid_certificate("y", "pod-1", Server));
    }

    #[test]
    fn a_new_pods_certificate_supersedes_only_the_old_pods_certificate_of_the_same_type() {
        let mut state = CredentialsState::new();
        state.apply(&issued_as("x", "old", "old-client", Client));
        state.apply(&issued_as("x", "old", "old-server", Server));
        // The replacement pod's client certificate retires the old client certificate, and only that.
        let superseded = state.apply(&issued_as("x", "new", "new-client", Client));
        assert_eq!(superseded, vec!["old-client".to_string()]);
        assert!(!state.has_valid_certificate("x", "old", Client));
        assert!(state.has_valid_certificate("x", "old", Server));
        let superseded = state.apply(&issued_as("x", "new", "new-server", Server));
        assert_eq!(superseded, vec!["old-server".to_string()]);
    }

    // ---- a multi-instance workload (docs/architecture/authority-model.md, "Multi-instance workloads") ---------

    /// A client certificate of `identity` naming the pod `uid` as its instance, or none.
    pub(crate) fn certificate_naming(identity: &str, uid: Option<&str>) -> String {
        let mut params = rcgen::CertificateParams::new(vec![identity.to_string()]).unwrap();
        if let Some(uid) = uid {
            for name in [format!("i:uid:{uid}"), format!("i:pod:{uid}-pod")] {
                params
                    .subject_alt_names
                    .push(rcgen::SanType::URI(name.try_into().unwrap()));
            }
        }
        params
            .self_signed(&rcgen::KeyPair::generate().unwrap())
            .unwrap()
            .pem()
    }

    pub(crate) fn issued_naming(
        identity: &str,
        uid: &str,
        serial: &str,
        named: bool,
    ) -> CredentialsEvent {
        CredentialsEvent::IssuanceCompleted {
            identity: identity.to_string(),
            instance_binding: uid.to_string(),
            serial: serial.to_string(),
            timestamp: 2000,
            cert_type: Client,
            grant_key: None,
            certificate_pem: certificate_naming(identity, named.then_some(uid)),
        }
    }

    #[test]
    fn two_pods_of_a_multi_instance_workload_hold_two_certificates_and_a_restart_supersedes_its_own()
     {
        let mut state = CredentialsState::new();
        assert!(
            state
                .apply(&issued_naming("b", "pod-a", "a-1", true))
                .is_empty()
        );
        assert!(
            state
                .apply(&issued_naming("b", "pod-b", "b-1", true))
                .is_empty(),
            "the second pod supersedes nothing"
        );
        assert!(state.has_valid_certificate("b", "pod-a", Client));
        assert!(state.has_valid_certificate("b", "pod-b", Client));
        // Pod a restarted in place: the same UID, a new certificate, which supersedes its own and only its own.
        assert_eq!(
            state.apply(&issued_naming("b", "pod-a", "a-2", true)),
            vec!["a-1".to_string()]
        );
        assert!(state.has_valid_certificate("b", "pod-b", Client));
        assert!(state.revoked_serials().contains(&"a-1".to_string()));
        assert!(!state.revoked_serials().contains(&"b-1".to_string()));
        // A single-instance workload's reissue supersedes, whatever the pod, as before.
        state.apply(&issued_naming("one", "pod-a", "o-1", false));
        assert_eq!(
            state.apply(&issued_naming("one", "pod-b", "o-2", false)),
            vec!["o-1".to_string()]
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

        /// For any sequence of issuances to the pods of one multi-instance workload: every pod issued holds exactly one
        /// valid client certificate, its latest, and what is revoked is exactly each pod's earlier ones.
        #[test]
        fn each_pod_of_a_multi_instance_workload_holds_its_latest_certificate_and_only_that(
            pods in proptest::collection::vec(0u8..4, 1..12),
        ) {
            let mut state = CredentialsState::new();
            let mut latest = std::collections::BTreeMap::new();
            let mut earlier = std::collections::BTreeSet::new();
            for (n, pod) in pods.iter().enumerate() {
                let uid = format!("pod-{pod}");
                let serial = format!("s{n}");
                state.apply(&issued_naming("b", &uid, &serial, true));
                if let Some(old) = latest.insert(uid, serial) {
                    earlier.insert(old);
                }
            }
            for (uid, serial) in &latest {
                proptest::prop_assert!(state.has_valid_certificate("b", uid, Client));
                proptest::prop_assert!(!state.revoked_serials().contains(serial), "{}", serial);
            }
            let revoked: std::collections::BTreeSet<String> = state.revoked_serials().into_iter().collect();
            proptest::prop_assert_eq!(revoked, earlier);
        }
    }

    #[test]
    fn the_certificate_looked_up_for_an_identity_is_its_client_certificate() {
        // The retrieve handlers read it for the requester's permissions: a server certificate, issued later,
        // is not the requester's.
        let mut state = CredentialsState::new();
        state.apply(&issued_as("x", "pod-1", "client-1", Client));
        state.apply(&issued_as("x", "pod-1", "server-1", Server));
        assert_eq!(state.certificate_for("x").unwrap().serial, "client-1");
        // And an identity with only a server certificate has no requester certificate.
        state.apply(&issued_as("only-server", "pod-1", "server-2", Server));
        assert!(state.certificate_for("only-server").is_none());
    }

    #[test]
    fn a_server_certificate_does_not_use_up_a_grant_and_a_client_certificate_does() {
        let mut state = CredentialsState::new();
        state.apply(&grant("x"));
        state.apply(&issued_as("x", "pod-1", "server-1", Server));
        assert!(
            state.grant_for("x").is_some(),
            "the permissions are for the client certificate, which has not been asked for yet"
        );
        state.apply(&issued_as("x", "pod-1", "client-1", Client));
        assert!(state.grant_for("x").is_none());
    }

    #[test]
    fn a_server_certificate_is_revoked_by_serial_and_the_clients_stays() {
        let mut state = CredentialsState::new();
        state.apply(&issued_as("x", "pod-1", "client-1", Client));
        state.apply(&issued_as("x", "pod-1", "server-1", Server));
        state.apply(&revoked("server-1"));
        assert!(!state.has_valid_certificate("x", "pod-1", Server));
        assert!(state.has_valid_certificate("x", "pod-1", Client));
        assert!(state.revoked_serials().contains(&"server-1".to_string()));
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
        assert!(!state.has_valid_certificate("alice", "pod-uid-1", WorkloadCertType::Client));
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

        assert!(state.has_valid_certificate("git-observer", "pod-uid-1", WorkloadCertType::Client));
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

    pub(crate) fn provisioned(username: &str, account_id: &str) -> CredentialsEvent {
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

    /// The certificate a requester connects with, whose groups policy reads at the request gate
    /// (docs/architecture/authority-model.md, slice 9): a workload's client certificate, or else an account's, while
    /// it is not revoked.
    #[test]
    fn a_requester_s_certificate_is_its_workload_client_certificate_or_its_account_s_while_not_revoked()
     {
        let mut state = CredentialsState::new();
        state.apply(&account_issued("dana", "acct-1", "s1"));
        assert_eq!(
            state.requester_certificate("acct-1"),
            Some("-----BEGIN CERTIFICATE-----\nfake-account\n-----END CERTIFICATE-----\n")
        );
        state.apply(&revoked("s1"));
        assert_eq!(state.requester_certificate("acct-1"), None);

        let issued =
            |identity: &str, serial: &str, cert_type| CredentialsEvent::IssuanceCompleted {
                identity: identity.to_string(),
                instance_binding: "pod-1".to_string(),
                serial: serial.to_string(),
                timestamp: 3000,
                grant_key: None,
                certificate_pem: format!("pem {serial}"),
                cert_type,
            };
        state.apply(&issued("w", "w1", WorkloadCertType::Client));
        assert_eq!(state.requester_certificate("w"), Some("pem w1"));
        // A server certificate is the listener's, and says nothing of what the requester may do.
        state.apply(&issued("srv", "srv1", WorkloadCertType::Server));
        assert_eq!(state.requester_certificate("srv"), None);
        assert_eq!(state.requester_certificate("nobody"), None);
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

    /// The name a person is written by and the one they are known by: every account the ledger holds, claimed or
    /// only provisioned, revoked or not, since a revoked account provisioned again keeps its account ID.
    #[test]
    fn every_account_s_username_names_its_account_id() {
        let mut state = CredentialsState::new();
        state.apply(&provisioned("dana", "acct-1"));
        state.apply(&account_issued("dana", "acct-1", "s1"));
        state.apply(&revoked("s1"));
        state.apply(&provisioned("erin", "acct-2"));
        state.apply(&account_issued("erin", "acct-2", "s2"));
        // Provisioned, not yet claimed.
        state.apply(&provisioned("fay", "acct-3"));
        assert_eq!(
            state.account_names(),
            std::collections::BTreeMap::from([
                ("dana".to_string(), "acct-1".to_string()),
                ("erin".to_string(), "acct-2".to_string()),
                ("fay".to_string(), "acct-3".to_string()),
            ])
        );
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
    // -- cert-issuer's record (docs/architecture/cert-issuer-isolation.md, "Step 2") ----------------------------------

    fn reported(
        seq: u64,
        identity: &str,
        cert_type: cert_issuer_common::CertType,
        serial: &str,
    ) -> ReportedCertificate {
        ReportedCertificate {
            seq,
            identity: identity.to_string(),
            instance_binding: Some(format!("pod-{seq}")),
            cert_type,
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            serial: serial.to_string(),
            signed_at: 1000 + seq as i64,
            not_after: 5000,
            certificate_pem: format!("pem {serial}"),
            unexpected: false,
        }
    }

    fn page(log: &str, through: u64, certificates: Vec<ReportedCertificate>) -> CredentialsEvent {
        CredentialsEvent::CertificatesReported {
            log: log.to_string(),
            through,
            certificates,
            tickets: Vec::new(),
            timestamp: 3000,
        }
    }

    const CASSINI: &str = "cassini.polar.serviceaccount.cluster.local";

    // cert-issuer's record says when in seconds; the ledger's issuances are in milliseconds, and so is what it records.
    #[test]
    fn a_reported_certificate_is_timed_in_milliseconds() {
        let mut state = CredentialsState::default();
        state.apply(&page(
            "log-1",
            1,
            vec![reported(
                1,
                CASSINI,
                cert_issuer_common::CertType::Client,
                "r1",
            )],
        ));
        assert_eq!(
            state.last_issued(CASSINI, "pod-1", WorkloadCertType::Client),
            Some(1001 * 1000)
        );
    }

    #[test]
    fn a_reported_certificate_is_found_superseded_and_revoked_like_an_issued_one() {
        let mut state = CredentialsState::default();
        state.apply(&page(
            "log-1",
            1,
            vec![reported(
                1,
                CASSINI,
                cert_issuer_common::CertType::Client,
                "r1",
            )],
        ));
        let found = state
            .certificate_for(CASSINI)
            .expect("a reported certificate is found by its identity");
        assert_eq!(found.serial, "r1");
        assert!(found.reported, "and marked as reported by cert-issuer");

        // A newer one for the same identity supersedes it.
        let superseded = state.apply(&page(
            "log-1",
            2,
            vec![reported(
                2,
                CASSINI,
                cert_issuer_common::CertType::Client,
                "r2",
            )],
        ));
        assert_eq!(superseded, vec!["r1".to_string()]);
        assert_eq!(state.certificate_for(CASSINI).unwrap().serial, "r2");

        // And it can be revoked by its serial.
        state.apply(&CredentialsEvent::Revoke {
            serial: "r2".to_string(),
            reason: RevocationReason::Compromised,
            revoked_by: "an-admin".to_string(),
            timestamp: 4000,
        });
        assert!(state.certificate_for(CASSINI).is_none());
        assert!(state.revoked_serials().contains(&"r2".to_string()));
    }

    #[test]
    fn a_reported_service_certificate_is_its_identity_s_escrow_certificate() {
        let mut state = CredentialsState::default();
        state.apply(&page(
            "log-1",
            1,
            vec![reported(
                1,
                "kube-consumer.polar.serviceaccount.cluster.local",
                cert_issuer_common::CertType::Service,
                "e1",
            )],
        ));
        let escrow = state
            .agent_escrow_certificate_for("kube-consumer.polar.serviceaccount.cluster.local")
            .expect("an escrow certificate");
        assert_eq!(escrow.serial, "e1");
        assert!(
            state
                .certificate_for("kube-consumer.polar.serviceaccount.cluster.local")
                .is_none()
        );
    }

    #[test]
    fn a_reported_account_certificate_is_known_by_its_serial_alone() {
        let mut state = CredentialsState::default();
        let mut account = reported(1, "an-account", cert_issuer_common::CertType::Client, "a1");
        account.path = cert_issuer_common::record::IssuePath::Internal;
        state.apply(&page("log-1", 1, vec![account]));
        assert!(state.knows_serial("a1"));
        assert!(
            state.certificate_for("an-account").is_none(),
            "which account it is cannot be known from the record"
        );
    }

    #[test]
    fn every_serial_the_ledger_has_recorded_is_known_and_no_other() {
        let mut state = CredentialsState::default();
        state.apply(&issued(CASSINI, "pod-1", "w1"));
        state.apply(&agent_escrow_issued(
            "kube-consumer.polar.serviceaccount.cluster.local",
            "pod-2",
            "e1",
        ));
        // A second escrow certificate for the same identity replaces the first in its record; the first's serial is
        // still one the ledger recorded.
        state.apply(&agent_escrow_issued(
            "kube-consumer.polar.serviceaccount.cluster.local",
            "pod-3",
            "e2",
        ));
        state.apply(&account_issued("alice", "alice-id", "a1"));
        state.apply(&page(
            "log-1",
            1,
            vec![reported(
                1,
                CASSINI,
                cert_issuer_common::CertType::Client,
                "r1",
            )],
        ));
        for serial in ["w1", "e1", "e2", "a1", "r1"] {
            assert!(state.knows_serial(serial), "{serial}");
        }
        assert!(!state.knows_serial("never"));
    }

    #[test]
    fn the_cursor_is_the_last_page_s_and_comes_back_the_same_on_a_replay() {
        let events = vec![
            page(
                "log-1",
                3,
                vec![reported(
                    2,
                    CASSINI,
                    cert_issuer_common::CertType::Client,
                    "r1",
                )],
            ),
            page("log-1", 7, vec![]),
        ];
        let mut state = CredentialsState::default();
        assert_eq!(state.record_cursor(), None, "nothing read yet");
        for e in &events {
            state.apply(e);
        }
        assert_eq!(state.record_cursor(), Some(("log-1".to_string(), 7)));
        let mut replayed = CredentialsState::default();
        for e in &events {
            replayed.apply(e);
        }
        assert_eq!(replayed.record_cursor(), state.record_cursor());
        assert_eq!(
            replayed.certificate_for(CASSINI).map(|r| r.serial.clone()),
            Some("r1".to_string())
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        /// Whatever the ledger records, by issuance or by report, it knows the serial of; and it knows no serial it
        /// never recorded.
        #[test]
        fn the_ledger_knows_exactly_the_serials_it_recorded(
            events in proptest::collection::vec((0u8..4, 0u32..40), 0..30),
        ) {
            let mut state = CredentialsState::default();
            let mut recorded = std::collections::BTreeSet::new();
            for (n, (kind, id)) in events.iter().enumerate() {
                let serial = format!("s{n}-{id}");
                let identity = format!("w{id}.polar.serviceaccount.cluster.local");
                let event = match kind {
                    0 => issued(&identity, "pod", &serial),
                    1 => agent_escrow_issued(&identity, "pod", &serial),
                    2 => account_issued(&format!("u{n}"), &format!("acct-{n}"), &serial),
                    _ => page("log", n as u64 + 1, vec![reported(n as u64 + 1, &identity, cert_issuer_common::CertType::Client, &serial)]),
                };
                state.apply(&event);
                recorded.insert(serial);
            }
            for serial in &recorded {
                proptest::prop_assert!(state.knows_serial(serial), "{}", serial);
            }
            proptest::prop_assert!(!state.knows_serial("not-recorded"));
        }
    }

    fn entry(
        seq: u64,
        identity: &str,
        path: cert_issuer_common::record::IssuePath,
        serial: &str,
    ) -> cert_issuer_common::record::RecordEntry {
        cert_issuer_common::record::RecordEntry {
            log: "log-1".to_string(),
            seq,
            serial: serial.to_string(),
            identity: identity.to_string(),
            cert_type: cert_issuer_common::CertType::Client,
            path,
            instance_binding: Some("pod".to_string()),
            signed_at: 1000,
            not_after: 5000,
            certificate_pem: format!("pem {serial}"),
        }
    }

    fn reply(
        log: &str,
        entries: Vec<cert_issuer_common::record::RecordEntry>,
    ) -> cert_issuer_common::record::RecordReply {
        cert_issuer_common::record::RecordReply {
            log: log.to_string(),
            entries,
            tickets: Vec::new(),
            more: false,
        }
    }

    fn bootstrap() -> BTreeSet<String> {
        [CASSINI.to_string()].into_iter().collect()
    }

    fn reported_of(event: &CredentialsEvent) -> Vec<(String, bool)> {
        match event {
            CredentialsEvent::CertificatesReported { certificates, .. } => certificates
                .iter()
                .map(|c| (c.serial.clone(), c.unexpected))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_page_s_certificates_the_ledger_does_not_hold_are_recorded_and_those_it_holds_are_not() {
        use cert_issuer_common::record::IssuePath;
        let mut state = CredentialsState::default();
        state.apply(&issued(
            "kube-observer.polar.serviceaccount.cluster.local",
            "pod",
            "issued-here",
        ));
        let page = reply(
            "log-1",
            vec![
                entry(
                    1,
                    "kube-observer.polar.serviceaccount.cluster.local",
                    IssuePath::Vouched,
                    "issued-here",
                ),
                entry(2, CASSINI, IssuePath::TokenOnly, "cassini-1"),
            ],
        );
        let event = state
            .reported_page(&page, Some(&bootstrap()), 7)
            .expect("an event");
        assert_eq!(reported_of(&event), vec![("cassini-1".to_string(), false)]);
        match &event {
            CredentialsEvent::CertificatesReported {
                log,
                through,
                timestamp,
                ..
            } => {
                assert_eq!((log.as_str(), *through, *timestamp), ("log-1", 2, 7));
            }
            other => panic!("{other:?}"),
        }
    }

    /// docs/architecture/core-restarts.md, "Step 5, the ticket": a successor ticket cert-issuer took is in its record,
    /// numbered with the certificates, and the ledger learns of it as it does of them: reported once, and read through.
    #[test]
    fn a_page_s_successor_ticket_is_reported_and_read_through_once() {
        let mut state = CredentialsState::default();
        let mut page = reply("log-1", vec![]);
        page.tickets = vec![cert_issuer_common::record::TicketEntry {
            log: "log-1".into(),
            seq: 3,
            identity: "credentials-agent.polar.serviceaccount.cluster.local".into(),
            instance_binding: "pod-uid".into(),
            taken_at: 100,
            expires_at: 400,
            issued_at: 99,
        }];
        let event = state
            .reported_page(&page, Some(&bootstrap()), 7)
            .expect("an event");
        match &event {
            CredentialsEvent::CertificatesReported {
                through,
                certificates,
                tickets,
                ..
            } => {
                assert_eq!(*through, 3);
                assert!(certificates.is_empty());
                assert_eq!(
                    tickets
                        .iter()
                        .map(|t| (t.seq, t.instance_binding.as_str()))
                        .collect::<Vec<_>>(),
                    vec![(3, "pod-uid")]
                );
            }
            other => panic!("{other:?}"),
        }
        state.apply(&event);
        assert!(
            state.reported_page(&page, Some(&bootstrap()), 8).is_none(),
            "the same page again: nothing new"
        );
    }

    #[test]
    fn a_report_written_before_tickets_replays_with_none() {
        let old =
            r#"{"CertificatesReported":{"log":"l","through":2,"certificates":[],"timestamp":1}}"#;
        match serde_json::from_str::<CredentialsEvent>(old).unwrap() {
            CredentialsEvent::CertificatesReported { tickets, .. } => assert!(tickets.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_token_only_certificate_for_anything_but_a_bootstrap_service_is_unexpected() {
        use cert_issuer_common::record::IssuePath;
        let state = CredentialsState::default();
        let page = reply(
            "log-1",
            vec![
                entry(1, CASSINI, IssuePath::TokenOnly, "bootstrap"),
                entry(
                    2,
                    "kube-observer.polar.serviceaccount.cluster.local",
                    IssuePath::TokenOnly,
                    "not-bootstrap",
                ),
                entry(
                    3,
                    "kube-observer.polar.serviceaccount.cluster.local",
                    IssuePath::Vouched,
                    "vouched",
                ),
                entry(
                    4,
                    "cert-issuer.polar.svc.cluster.local",
                    IssuePath::Own,
                    "own",
                ),
            ],
        );
        let event = state.reported_page(&page, Some(&bootstrap()), 7).unwrap();
        assert_eq!(
            reported_of(&event),
            vec![
                ("bootstrap".to_string(), false),
                ("not-bootstrap".to_string(), true),
                ("vouched".to_string(), false),
                ("own".to_string(), false),
            ]
        );
    }

    #[test]
    fn with_no_model_no_certificate_can_be_called_unexpected() {
        use cert_issuer_common::record::IssuePath;
        let state = CredentialsState::default();
        let page = reply(
            "log-1",
            vec![entry(
                1,
                "kube-observer.polar.serviceaccount.cluster.local",
                IssuePath::TokenOnly,
                "t",
            )],
        );
        let event = state.reported_page(&page, None, 7).unwrap();
        assert_eq!(reported_of(&event), vec![("t".to_string(), false)]);
    }

    #[test]
    fn a_page_that_moves_nothing_makes_no_event_and_one_that_moves_the_cursor_does() {
        use cert_issuer_common::record::IssuePath;
        let mut state = CredentialsState::default();
        let first = reply("log-1", vec![entry(1, CASSINI, IssuePath::TokenOnly, "c1")]);
        state.apply(&state.reported_page(&first, Some(&bootstrap()), 7).unwrap());
        assert!(
            state.reported_page(&first, Some(&bootstrap()), 8).is_none(),
            "the same page again moves nothing"
        );
        assert!(
            state
                .reported_page(&reply("log-1", vec![]), Some(&bootstrap()), 8)
                .is_none(),
            "an empty page moves nothing"
        );
        // Only known certificates, further on: the cursor moves, and nothing is recorded twice.
        let known = reply("log-1", vec![entry(2, CASSINI, IssuePath::TokenOnly, "c1")]);
        let event = state
            .reported_page(&known, Some(&bootstrap()), 9)
            .expect("the cursor moves");
        assert!(reported_of(&event).is_empty());
    }

    #[test]
    fn a_late_page_s_unknown_certificates_are_recorded_and_the_cursor_never_moves_back() {
        use cert_issuer_common::record::IssuePath;
        let mut state = CredentialsState::default();
        state.apply(
            &state
                .reported_page(
                    &reply(
                        "log-1",
                        vec![entry(10, CASSINI, IssuePath::TokenOnly, "c10")],
                    ),
                    Some(&bootstrap()),
                    7,
                )
                .unwrap(),
        );
        // An answer to an older request, arriving after a newer one.
        let late = reply("log-1", vec![entry(1, CASSINI, IssuePath::TokenOnly, "c1")]);
        let event = state
            .reported_page(&late, Some(&bootstrap()), 8)
            .expect("its certificate is not lost");
        assert_eq!(reported_of(&event), vec![("c1".to_string(), false)]);
        state.apply(&event);
        assert_eq!(
            state.record_cursor(),
            Some(("log-1".to_string(), 10)),
            "not moved back"
        );
    }

    #[test]
    fn a_page_from_another_log_is_taken_from_its_start() {
        use cert_issuer_common::record::IssuePath;
        let mut state = CredentialsState::default();
        state.apply(
            &state
                .reported_page(
                    &reply("log-1", vec![entry(5, CASSINI, IssuePath::TokenOnly, "c5")]),
                    Some(&bootstrap()),
                    7,
                )
                .unwrap(),
        );
        // cert-issuer's log was replaced: its numbers start again.
        let fresh = reply(
            "log-2",
            vec![entry(1, CASSINI, IssuePath::TokenOnly, "fresh")],
        );
        let event = state
            .reported_page(&fresh, Some(&bootstrap()), 8)
            .expect("another log's page is taken");
        assert_eq!(reported_of(&event), vec![("fresh".to_string(), false)]);
        state.apply(&event);
        assert_eq!(state.record_cursor(), Some(("log-2".to_string(), 1)));
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        /// However the pages come (repeated, overlapping, out of order, from a replaced log), no certificate is
        /// recorded twice, every certificate in any page ends up recorded, and the cursor never moves back in a log.
        #[test]
        fn no_certificate_is_recorded_twice_whatever_the_pages(
            pages in proptest::collection::vec(
                (0u8..2, proptest::collection::vec(0u64..12, 0..5)),
                0..12,
            ),
        ) {
            use cert_issuer_common::record::IssuePath;
            let mut state = CredentialsState::default();
            let mut recorded: Vec<String> = Vec::new();
            let mut offered = std::collections::BTreeSet::new();
            for (log, seqs) in &pages {
                let log = format!("log-{log}");
                let mut seqs = seqs.clone();
                seqs.sort();
                seqs.dedup();
                let entries: Vec<_> = seqs.iter().map(|n| {
                    let serial = format!("{log}-{n}");
                    offered.insert(serial.clone());
                    let mut e = entry(*n + 1, CASSINI, IssuePath::TokenOnly, &serial);
                    e.log = log.clone();
                    e
                }).collect();
                let before = state.record_cursor();
                if let Some(event) = state.reported_page(&reply(&log, entries), Some(&bootstrap()), 7) {
                    recorded.extend(reported_of(&event).into_iter().map(|(s, _)| s));
                    state.apply(&event);
                }
                if let (Some((was_log, was)), Some((now_log, now))) = (before, state.record_cursor())
                    && was_log == now_log
                {
                    proptest::prop_assert!(now >= was, "the cursor moved back: {} to {}", was, now);
                }
            }
            let distinct: std::collections::BTreeSet<_> = recorded.iter().cloned().collect();
            proptest::prop_assert_eq!(distinct.len(), recorded.len(), "recorded twice: {:?}", recorded);
            for serial in &offered {
                proptest::prop_assert!(state.knows_serial(serial), "{}", serial);
            }
        }
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

// docs/architecture/authority-distribution.md, "Pruning the revoked set".
#[cfg(test)]
mod pruning_tests {
    use super::*;
    use proptest::prelude::*;

    /// A certificate whose life ends at `end` (Unix seconds).
    fn certificate_ending(end: i64) -> String {
        let mut params = rcgen::CertificateParams::new(vec!["pod.test".to_string()]).unwrap();
        params.not_before = time::OffsetDateTime::from_unix_timestamp(end - 3600).unwrap();
        params.not_after = time::OffsetDateTime::from_unix_timestamp(end).unwrap();
        params
            .self_signed(&rcgen::KeyPair::generate().unwrap())
            .unwrap()
            .pem()
    }

    fn issued_ending(identity: &str, pod: &str, serial: &str, end: i64) -> CredentialsEvent {
        CredentialsEvent::IssuanceCompleted {
            identity: identity.to_string(),
            instance_binding: pod.to_string(),
            serial: serial.to_string(),
            timestamp: 2000,
            cert_type: WorkloadCertType::Client,
            grant_key: None,
            certificate_pem: certificate_ending(end),
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

    const T: i64 = 1_800_000_000;

    #[test]
    fn a_superseded_serial_is_published_until_its_certificate_ended_more_than_the_margin_ago() {
        let mut state = CredentialsState::new();
        // A pod restarted in place: its record is replaced, and the old certificate's end still known.
        state.apply(&issued_ending("alice", "pod-1", "0a", T));
        state.apply(&issued_ending("alice", "pod-1", "0b", T + 3600));
        assert_eq!(state.revoked_serials(), ["0a"]);
        assert_eq!(state.published_revocations(T).0, ["0a"]);
        assert_eq!(
            state.published_revocations(T + REVOCATION_MARGIN_SECS).0,
            ["0a"],
            "not before the margin has passed"
        );
        assert!(
            state
                .published_revocations(T + REVOCATION_MARGIN_SECS + 1)
                .0
                .is_empty()
        );
        assert_eq!(state.revoked_serials(), ["0a"], "the ledger keeps it");
    }

    /// Revocation records why, and the published set says it (docs/architecture/authority-model.md, "Multi-instance
    /// workloads"): a revocation for a compromise is named as one; any other reason, and a supersession, is an ordinary
    /// end, which is how every revocation reads where nothing said otherwise.
    #[test]
    fn the_published_set_names_the_serials_revoked_for_a_compromise_and_no_other() {
        let mut state = CredentialsState::new();
        // Superseded: an ordinary end.
        state.apply(&issued_ending("alice", "pod-1", "0a", T));
        state.apply(&issued_ending("alice", "pod-2", "0b", T + 3600));
        state.apply(&revoked("0c"));
        state.apply(&CredentialsEvent::Revoke {
            serial: "0d".to_string(),
            reason: RevocationReason::Decommissioned,
            revoked_by: "admin".to_string(),
            timestamp: 3000,
        });
        let (serials, _) = state.published_revocations(T);
        assert_eq!(serials, ["0a", "0c", "0d"]);
        assert_eq!(state.compromised_among(&serials), ["0c"]);
        assert!(state.compromised_among(&[]).is_empty());
    }

    proptest! {
        /// For any revocations, each with any reason: what the published set names as compromised is exactly the
        /// published serials whose revocation said `Compromised`, and never a serial that is not published.
        #[test]
        fn the_compromised_are_exactly_the_published_serials_revoked_for_a_compromise(
            revocations in prop::collection::vec((0u8..6, any::<bool>()), 0..20),
        ) {
            let mut state = CredentialsState::new();
            let mut compromised = std::collections::BTreeSet::new();
            for (serial, compromise) in &revocations {
                let serial = format!("{serial:02x}");
                state.apply(&CredentialsEvent::Revoke {
                    serial: serial.clone(),
                    reason: if *compromise { RevocationReason::Compromised } else { RevocationReason::AccessNoLongerRequired },
                    revoked_by: "admin".to_string(),
                    timestamp: 3000,
                });
                if *compromise {
                    compromised.insert(serial);
                }
            }
            let (serials, _) = state.published_revocations(T);
            let named = state.compromised_among(&serials);
            prop_assert!(named.iter().all(|s| serials.contains(s)));
            let expected: Vec<String> = serials.iter().filter(|s| compromised.contains(*s)).cloned().collect();
            prop_assert_eq!(named, expected);
        }
    }

    #[test]
    fn an_explicitly_revoked_serial_ends_with_its_certificate_and_one_never_seen_is_kept() {
        let mut state = CredentialsState::new();
        state.apply(&issued_ending("alice", "pod-1", "0a", T));
        state.apply(&revoked("0a"));
        state.apply(&revoked("ff"));
        let later = T + REVOCATION_MARGIN_SECS + 1;
        assert_eq!(state.published_revocations(T).0, ["0a", "ff"]);
        assert_eq!(state.published_revocations(later).0, ["ff"]);
        assert_eq!(
            state.published_revocations(later * 2).0,
            ["ff"],
            "no end known"
        );
    }

    #[test]
    fn the_version_rises_for_a_serial_revoked_and_for_one_ended_and_never_falls_below_the_count() {
        let mut state = CredentialsState::new();
        assert_eq!(state.published_revocations(T).1, 0);
        state.apply(&issued_ending("alice", "pod-1", "0a", T));
        state.apply(&issued_ending("alice", "pod-2", "0b", T + 7200));
        let (_, v1) = state.published_revocations(T);
        assert_eq!(v1, 1, "as the size was");
        let (set, v2) = state.published_revocations(T + REVOCATION_MARGIN_SECS + 1);
        assert!(set.is_empty());
        assert_eq!(v2, 2, "one revoked, one ended");
        state.apply(&revoked("ff"));
        let (set, v3) = state.published_revocations(T + REVOCATION_MARGIN_SECS + 1);
        assert_eq!(set, ["ff"]);
        assert_eq!(v3, 3);
    }

    #[test]
    fn a_replay_gives_the_same() {
        let events = [
            issued_ending("alice", "pod-1", "0a", T),
            issued_ending("alice", "pod-1", "0b", T + 60),
            revoked("0b"),
            revoked("ff"),
        ];
        let fold = || {
            let mut state = CredentialsState::new();
            for e in &events {
                state.apply(e);
            }
            state
        };
        for now in [
            T,
            T + REVOCATION_MARGIN_SECS + 1,
            T + REVOCATION_MARGIN_SECS + 61,
        ] {
            assert_eq!(
                fold().published_revocations(now),
                fold().published_revocations(now)
            );
        }
    }

    /// A step: a pod of alice's issued (superseding the one before) with a certificate ending `ends` seconds after
    /// T, a serial revoked explicitly (one known, or never seen), or time moving on.
    #[derive(Debug, Clone)]
    enum Step {
        Issue { pod: u8, ends: i64 },
        Revoke { known: bool, which: u8 },
        Wait(i64),
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (0u8..3, 0i64..20_000).prop_map(|(pod, ends)| Step::Issue { pod, ends }),
            (any::<bool>(), 0u8..8).prop_map(|(known, which)| Step::Revoke { known, which }),
            (0i64..8000).prop_map(Step::Wait),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// For any sequence of issuances, revocations and time moving on: what is published is exactly the revoked
        /// serials whose certificate has not ended by more than the margin (or has no known end), and its version rises
        /// whenever that set changes and never falls. (It may rise with the set unchanged, a serial revoked after its
        /// certificate had ended: an update that changes nothing, which a leaf takes harmlessly.)
        #[test]
        fn the_published_set_is_the_revoked_not_ended_and_its_version_rises_with_each_change(
            steps in prop::collection::vec(step(), 1..12),
        ) {
            let mut state = CredentialsState::new();
            let mut now = T;
            // The model: each serial issued, with its certificate's end.
            let mut ends: std::collections::BTreeMap<String, i64> = Default::default();
            let mut revoked_ever: std::collections::BTreeSet<String> = Default::default();
            let mut n = 0;
            let (mut last_set, mut last_version) = state.published_revocations(now);
            for step in steps {
                match step {
                    Step::Issue { pod, ends: e } => {
                        n += 1;
                        let serial = format!("{n:04x}");
                        state.apply(&issued_ending("alice", &format!("pod-{pod}"), &serial, T + e));
                        ends.insert(serial, T + e);
                    }
                    Step::Revoke { known, which } => {
                        let serial = if known && !ends.is_empty() {
                            ends.keys().nth(which as usize % ends.len()).unwrap().clone()
                        } else {
                            format!("ff{which:02x}")
                        };
                        state.apply(&revoked(&serial));
                    }
                    Step::Wait(seconds) => now += seconds,
                }
                revoked_ever.extend(state.revoked_serials());
                let expected: Vec<String> = revoked_ever
                    .iter()
                    .filter(|s| ends.get(*s).is_none_or(|end| now - end <= REVOCATION_MARGIN_SECS))
                    .cloned()
                    .collect();
                let (set, version) = state.published_revocations(now);
                prop_assert_eq!(&set, &expected);
                prop_assert!(version >= last_version, "the version fell");
                prop_assert!(set == last_set || version > last_version, "the set changed and the version did not rise");
                prop_assert!(version >= state.revoked_serials().len() as u64);
                last_set = set;
                last_version = version;
            }
        }
    }
}
