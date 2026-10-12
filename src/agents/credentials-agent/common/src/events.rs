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

/// Why a certificate was revoked. A deliberately closed set -- if a
/// real revocation doesn't fit one of these, that's a gap in this
/// enum worth naming and adding a variant for, not a reason to fall
/// back to free text.
///
/// Loosely modeled on RFC 5280's own standard CRL revocation reason
/// codes (https://www.rfc-editor.org/rfc/rfc5280#section-5.3.1),
/// adapted for Polar's actual domain -- workload/service identities
/// and human users alike, not just the TLS-server-certificate context
/// RFC 5280 was originally written for. Several variants here are
/// Polar-specific refinements within one broader RFC 5280 category
/// (see each variant's own doc comment for exactly which), and
/// IssuedInError has no clean RFC 5280 equivalent at all -- noted
/// honestly rather than forcing a strained mapping.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    Archive,
    SerdeDeserialize,
    SerdeSerialize,
    clap::ValueEnum,
)]
pub enum RevocationReason {
    /// The private key or certificate is suspected or confirmed
    /// compromised -- includes a lost or stolen device carrying the
    /// key, even with no direct evidence of misuse yet; "suspected"
    /// is enough. Corresponds directly to RFC 5280's keyCompromise.
    Compromised,

    /// System-generated only -- never an admin's own choice via
    /// polar-admin-grant's CLI. Applied automatically when a reissue
    /// for the same identity supersedes a prior, still-valid
    /// certificate (see CredentialsState::apply's own
    /// IssuanceCompleted handling). Corresponds directly to RFC
    /// 5280's superseded.
    Superseded,

    /// The workload/service this credential was issued to has been
    /// permanently decommissioned -- not just restarted or scaled
    /// down, genuinely removed from the system for good. A
    /// Polar-specific refinement of RFC 5280's cessationOfOperation,
    /// scoped specifically to non-human identities -- Polar
    /// deliberately splits what that one RFC category covers into
    /// this and UserOffboarded below, for better operational
    /// granularity than the RFC itself needs.
    Decommissioned,

    /// The human user this credential was issued to has left the
    /// organization or project entirely. Distinct from
    /// Decommissioned, which is about a service/workload shutting
    /// down, not a person leaving -- an operator reviewing revocation
    /// history should be able to tell these apart. Also a
    /// Polar-specific refinement of RFC 5280's cessationOfOperation,
    /// scoped specifically to human identities.
    UserOffboarded,

    /// A legitimate, non-punitive routine removal -- a project ended,
    /// a workload's role changed, access was time-bounded and its
    /// time is simply up. No wrongdoing implied, unlike
    /// PolicyViolation below. The variant that actually maps most
    /// directly to RFC 5280's own cessationOfOperation text ("no
    /// longer needed for the purpose for which it was issued, but no
    /// reason to believe the certificate will be misused") --
    /// Decommissioned and UserOffboarded above are Polar's own,
    /// more specific splits of that same broad RFC category.
    AccessNoLongerRequired,

    /// Revoked for a policy reason not covered by the other variants
    /// -- an admin's own deliberate judgment call, distinct from
    /// AccessNoLongerRequired's routine, no-wrongdoing framing.
    /// Corresponds directly to RFC 5280's privilegeWithdrawn.
    PolicyViolation,

    /// The certificate itself was wrong from the start -- bad
    /// permissions baked into the grant it was issued from, a bug in
    /// the issuance path, an admin's own data-entry error. Distinct
    /// from Compromised (nothing malicious happened) and
    /// PolicyViolation (this isn't a policy problem, it's a
    /// correctness problem). Genuinely has no clean RFC 5280
    /// equivalent -- noted here rather than forcing one.
    IssuedInError,
}

/// Why a permission was granted. Unlike RevocationReason, this has no
/// RFC to anchor against -- genuinely Polar-specific, but held to the
/// same discipline: a deliberately closed set, not free text.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    Archive,
    SerdeDeserialize,
    SerdeSerialize,
    clap::ValueEnum,
)]
pub enum GrantReason {
    /// A new human user being set up for the first time.
    UserOnboarding,
    /// A new workload/service being stood up for the first time.
    ServiceOnboarding,
    /// An existing, already-known identity's responsibilities
    /// changed, requiring different or additional permissions than it
    /// previously held.
    RoleChange,
    /// Short-term, incident-response or debugging-driven elevated
    /// access -- expected to be revoked again soon. Names the reason
    /// category now, even though auto-expiring, time-bounded grants
    /// aren't a built feature yet -- that's real, separate future
    /// work this doesn't attempt to solve.
    TemporaryElevation,
    /// A prior grant was mistakenly missing or denied -- this grant
    /// corrects that. The positive-direction mirror of
    /// RevocationReason::IssuedInError.
    CorrectionOfError,
    /// Granted to satisfy a policy or compliance requirement not
    /// covered by the other, more specific categories -- the positive
    /// mirror of RevocationReason::PolicyViolation.
    PolicyRequirement,
}

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
#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum CredentialsEvent {
    /// Published by an admin's tool to the `credentials-grants` topic.
    /// `target_identity` must be the exact claim value the relevant
    /// issuer's `workload_identity_claim` will later extract from the
    /// user's real token -- the admin is responsible for confirming
    /// this against the IdP directly before publishing.
    Grant {
        target_identity: String,
        /// A real, structured shape now -- see
        /// cert_issuer_common::permission::Permission's own doc
        /// comment for the design (RBAC-style scope + optional FQDN
        /// target, embedded into the certificate's own SAN set at
        /// issuance time via Permission::to_san_string).
        permissions: Vec<cert_issuer_common::permission::Permission>,
        granted_by: String,
        timestamp: i64,
        reason: GrantReason,
    },
    /// Published by an admin's tool to the `credentials-revocations`
    /// topic. Uniform for human- and workload-issued certificates
    /// alike -- both are ultimately just certificates with serial
    /// numbers.
    Revoke {
        serial: String,
        reason: RevocationReason,
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
        /// The actual signed certificate, in plain PEM. Public by
        /// nature -- presented in plaintext during every mTLS
        /// handshake anyway -- so no encryption is needed here,
        /// unlike the full encrypted cert+key escrow relay this event
        /// log is not what stores. Enables the certificate-lookup
        /// query: one agent asking for another's public cert so it
        /// can encrypt a payload directly to it.
        certificate_pem: String,
    },
    /// Published internally by the consumer itself, once a
    /// StoreSecretRequest has been validated (genuinely encrypted to
    /// this agent's own current escrow key) and is being persisted.
    /// A second SecretStored for the same `name` supersedes the
    /// first in derived state (CredentialsState::apply's own folding
    /// logic) -- not by rewriting history here, this event log never
    /// does that, but by simply overwriting the map entry a later
    /// event produces during replay. The old event, and the old
    /// (now-superseded) EscrowedSecret it carried, remain in the log
    /// forever regardless -- nothing sent to a stale ciphertext form
    /// is ever irrecoverable, only no longer current.
    SecretStored {
        name: String,
        secret: crate::escrow_secret::EscrowedSecret,
        /// Broker-verified (published_by), never self-reported.
        stored_by: String,
        timestamp: i64,
    },
    /// Published internally by the consumer itself once a signed
    /// CertType::Service certificate has actually been produced, for
    /// the automated agent escrow-identity issuance lifecycle -- see
    /// credentials-agent-architecture.md's own "Automated agent
    /// escrow-identity issuance" section. Genuinely distinct from
    /// IssuanceCompleted, not a reuse of it: this is a service's
    /// durable identity, not a pod-instance's ephemeral mTLS
    /// certificate, so the derived lookup this folds into
    /// (CredentialsState's own agent escrow certificate map) is keyed
    /// on identity alone -- not (identity, instance_binding) the way
    /// IssuanceCompleted's own valid_certificates map is.
    /// `instance_binding` is still recorded here, for the same audit-
    /// visibility reason IssuanceCompleted records it, even though it
    /// plays no role in this event's own dedup/lookup semantics.
    AgentEscrowIssuanceCompleted {
        identity: String,
        instance_binding: String,
        serial: String,
        timestamp: i64,
        certificate_pem: String,
    },
    /// Published by credentials-agent itself once an admin (or the
    /// one-time bootstrap identity) has provisioned a Polar account --
    /// mirrors Grant's own shape, but for a durable account rather
    /// than a permission on an existing identity. Single-use, exactly
    /// like Grant: consumed the moment a matching
    /// AccountCertificateIssued is folded.
    AccountProvisioned {
        account_id: String,
        username: String,
        email: String,
        permissions: Vec<cert_issuer_common::permission::Permission>,
        /// Administrative capabilities the account holds once claimed.
        /// Absent on events written before capabilities existed, which
        /// replay as none.
        #[serde(default)]
        capabilities: Vec<cert_issuer_common::capability::Capability>,
        provisioned_by: String,
        timestamp: i64,
        reason: GrantReason,
        /// True only for the one-time admin-bootstrap path -- lets
        /// CredentialsState enforce "bootstrap happens at most once"
        /// without hardcoding the bootstrap identity string here.
        via_bootstrap: bool,
        given_name: Option<String>,
        surname: Option<String>,
        title: Option<String>,
        telephone_number: Option<String>,
    },
    /// Published once a signed certificate has actually been produced
    /// for a provisioned account. Identity-keyed on account_id, not
    /// (identity, instance_binding) -- mirrors
    /// AgentEscrowIssuanceCompleted's own reasoning exactly: an
    /// account is a durable thing, not a per-pod-instance one.
    AccountCertificateIssued {
        account_id: String,
        /// Carried so CredentialsState::apply can consume the
        /// matching AccountProvisioned entry (keyed by username) in
        /// the same fold.
        username: String,
        serial: String,
        timestamp: i64,
        certificate_pem: String,
    },
    /// One accepted, signature-verified admin action -- the only way an
    /// admin's provision/grant/revoke reaches the ledger. Carries the
    /// whole signed action, so anyone holding the ledger can re-verify
    /// it. Its effect is not a separate event: folding this event
    /// derives the effect (see `admin_effect`), so acceptance and
    /// effect are one atomic append, and the hash below is what refuses
    /// a replay.
    AdminActionRecorded {
        /// Hex SHA-256 of `signed.payload`.
        payload_hash: String,
        signed: crate::admin_action::SignedAdminAction,
        /// The author's account_id, from the ledger's own record of the
        /// certificate that signed this -- never from the payload.
        author_account_id: String,
        timestamp: i64,
        /// The account_id the consumer minted for a ProvisionAccount
        /// action; None for every other action.
        account_id: Option<String>,
    },
}

impl CredentialsEvent {
    /// The effect event an `AdminActionRecorded` stands for, or `None`
    /// for any other event (and for an action with no effect yet).
    /// State and the graph projection both fold this, so there is one
    /// derivation and the two cannot drift.
    pub fn admin_effect(&self) -> Option<CredentialsEvent> {
        use crate::admin_action::AdminAction;
        let CredentialsEvent::AdminActionRecorded {
            signed,
            author_account_id,
            timestamp,
            account_id,
            ..
        } = self
        else {
            return None;
        };
        let body = signed.decode_body().ok()?;
        Some(match body.action {
            AdminAction::ProvisionAccount {
                username,
                email,
                permissions,
                capabilities,
                reason,
                given_name,
                surname,
                title,
                telephone_number,
            } => CredentialsEvent::AccountProvisioned {
                account_id: account_id.clone()?,
                username,
                email,
                permissions,
                capabilities,
                provisioned_by: author_account_id.clone(),
                timestamp: *timestamp,
                reason,
                via_bootstrap: false,
                given_name,
                surname,
                title,
                telephone_number,
            },
            AdminAction::Grant {
                target_identity,
                permissions,
                reason,
            } => CredentialsEvent::Grant {
                target_identity,
                permissions,
                granted_by: author_account_id.clone(),
                timestamp: *timestamp,
                reason,
            },
            AdminAction::Revoke { serial, reason } => CredentialsEvent::Revoke {
                serial,
                reason,
                revoked_by: author_account_id.clone(),
                timestamp: *timestamp,
            },
            // Recorded for audit, but there is no policy state to fold
            // it into yet.
            AdminAction::PolicyUpdate { .. } => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_grant() -> CredentialsEvent {
        CredentialsEvent::Grant {
            target_identity: "jdoe".to_string(),
            permissions: vec![
                cert_issuer_common::permission::Permission {
                    scope: cert_issuer_common::permission::PermissionScope::UserToService,
                    target: Some("cassini.cluster.local".to_string()),
                },
                cert_issuer_common::permission::Permission {
                    scope: cert_issuer_common::permission::PermissionScope::UserToUser,
                    target: None,
                },
            ],
            granted_by: "admin".to_string(),
            timestamp: 1767225600000,
            reason: GrantReason::UserOnboarding,
        }
    }
    fn sample_revoke() -> CredentialsEvent {
        CredentialsEvent::Revoke {
            serial: "abc123".to_string(),
            reason: RevocationReason::Compromised,
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
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                .to_string(),
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

    fn recorded(
        action: crate::admin_action::AdminAction,
        minted: Option<&str>,
    ) -> CredentialsEvent {
        use crate::admin_action::{AdminActionBody, SignedAdminAction};
        let payload = AdminActionBody {
            nonce: "n1".to_string(),
            action,
        }
        .to_payload()
        .unwrap();
        let signed = SignedAdminAction {
            payload,
            author_serial: "a1".to_string(),
            signature: vec![],
        };
        CredentialsEvent::AdminActionRecorded {
            payload_hash: signed.payload_hash(),
            signed,
            author_account_id: "admin-acct".to_string(),
            timestamp: 4242,
            account_id: minted.map(str::to_string),
        }
    }

    #[test]
    fn a_recorded_grant_derives_a_grant_stamped_with_the_verified_author() {
        use crate::admin_action::AdminAction;
        let event = recorded(
            AdminAction::Grant {
                target_identity: "jdoe".to_string(),
                permissions: vec![],
                reason: GrantReason::UserOnboarding,
            },
            None,
        );
        match event.admin_effect() {
            Some(CredentialsEvent::Grant {
                target_identity,
                granted_by,
                timestamp,
                ..
            }) => {
                assert_eq!(target_identity, "jdoe");
                assert_eq!(granted_by, "admin-acct");
                assert_eq!(timestamp, 4242);
            }
            other => panic!("expected Grant, got {other:?}"),
        }
    }

    #[test]
    fn a_recorded_revoke_derives_a_revoke_stamped_with_the_verified_author() {
        use crate::admin_action::AdminAction;
        let event = recorded(
            AdminAction::Revoke {
                serial: "abc123".to_string(),
                reason: RevocationReason::Compromised,
            },
            None,
        );
        match event.admin_effect() {
            Some(CredentialsEvent::Revoke {
                serial, revoked_by, ..
            }) => {
                assert_eq!(serial, "abc123");
                assert_eq!(revoked_by, "admin-acct");
            }
            other => panic!("expected Revoke, got {other:?}"),
        }
    }

    #[test]
    fn a_recorded_provision_derives_account_provisioned_with_the_minted_id() {
        use crate::admin_action::AdminAction;
        let event = recorded(
            AdminAction::ProvisionAccount {
                username: "dana".to_string(),
                email: "dana@example.org".to_string(),
                permissions: vec![],
                capabilities: vec![cert_issuer_common::capability::Capability::Grant],
                reason: GrantReason::UserOnboarding,
                given_name: None,
                surname: None,
                title: None,
                telephone_number: None,
            },
            Some("acct-9"),
        );
        match event.admin_effect() {
            Some(CredentialsEvent::AccountProvisioned {
                account_id,
                provisioned_by,
                via_bootstrap,
                capabilities,
                ..
            }) => {
                assert_eq!(account_id, "acct-9");
                assert_eq!(provisioned_by, "admin-acct");
                assert!(
                    !via_bootstrap,
                    "only the internal bootstrap path may set this"
                );
                assert_eq!(capabilities.len(), 1);
            }
            other => panic!("expected AccountProvisioned, got {other:?}"),
        }
    }

    #[test]
    fn a_recorded_provision_without_a_minted_id_derives_nothing() {
        use crate::admin_action::AdminAction;
        let event = recorded(
            AdminAction::ProvisionAccount {
                username: "dana".to_string(),
                email: "dana@example.org".to_string(),
                permissions: vec![],
                capabilities: vec![],
                reason: GrantReason::UserOnboarding,
                given_name: None,
                surname: None,
                title: None,
                telephone_number: None,
            },
            None,
        );
        assert!(event.admin_effect().is_none());
    }

    #[test]
    fn a_policy_update_and_an_ordinary_event_derive_nothing() {
        use crate::admin_action::AdminAction;
        let policy = recorded(
            AdminAction::PolicyUpdate {
                document: "permit(principal, action, resource);".to_string(),
            },
            None,
        );
        assert!(policy.admin_effect().is_none());
        assert!(sample_grant().admin_effect().is_none());
    }
}
