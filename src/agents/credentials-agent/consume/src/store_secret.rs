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

//! Store half of the general secret-escrow mechanism
//! (docs/architecture/credentials-policy.md, "Storing is governed by policy").
//!
//! In order, refusing at the first failure, and saying nothing about whether a secret exists to anyone policy
//! does not permit:
//!
//! 1. the publish is attributed (broker-verified);
//! 2. the name is one a secret can have, and the labels are well formed: both reach policy, so nothing in them
//!    may end a resource or a rule early;
//! 3. the requester's client certificate is on record: policy reads its permissions;
//! 4. policy lets this identity store this secret. Cedar is asked first (action `store`). If it says nothing, or no
//!    policy is loaded, the floor decides: an identity may store under its own namespace (`<identity>/...`) and
//!    as `agent-escrow-keypair:<identity>`. That floor is what lets every agent escrow its keypair on a fresh
//!    cluster, before any policy has been seeded. If policy cannot be reached at all that is a failure to
//!    retry, never the floor;
//! 5. the secret is genuinely encrypted to this agent's own current escrow key;
//! 6. the ledger appends it, atomically with its check that the name is the requester's to store and that the
//!    labels are those it was stored with.

use crate::escrow::EscrowKeypair;
use crate::ledger::{LedgerMessage, SecretStoreError};
use crate::retrieve_secret::{call_check_certificate, call_check_secret};
use credentials_agent_common::agent_escrow_keypair_bootstrap::escrow_keypair_owner;
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::retrieve_secret::is_valid_secret_name;
use credentials_agent_common::secret::{
    StoreRefusalReason, StoreSecretReply, StoreSecretRequest, namespace_of, validate_labels,
};
use polar_agent_builder::{LookupError, PolicyLookup};
use ractor::ActorRef;
use std::sync::Arc;
use tracing::{debug, error};

pub struct StoreSecretHandler {
    pub escrow_keypair: Arc<EscrowKeypair>,
    pub ledger: ActorRef<LedgerMessage>,
    pub policy: PolicyLookup,
}

/// What an identity may store when policy has nothing to say: a name in its own namespace. The name an escrow keypair
/// is stored under is never policy's or the floor's: it is its workload's custodian's, once (`store_escrow_keypair`).
pub fn floor_allows(requester: &str, name: &str) -> bool {
    namespace_of(name) == Some(requester)
}

fn refused(reason: StoreRefusalReason) -> StoreSecretReply {
    StoreSecretReply::Refused { reason }
}

impl StoreSecretHandler {
    /// [`handle_from`](Self::handle_from), from a publisher with no instance (a single-instance workload's).
    pub async fn handle(
        &self,
        request: StoreSecretRequest,
        stored_by: Option<String>,
    ) -> StoreSecretReply {
        self.handle_from(request, stored_by, None).await
    }

    /// A store from `stored_by`, the pod `instance` of it when the broker stamped one.
    pub async fn handle_from(
        &self,
        request: StoreSecretRequest,
        stored_by: Option<String>,
        instance: Option<String>,
    ) -> StoreSecretReply {
        let Some(stored_by) = stored_by else {
            return refused(StoreRefusalReason::AnonymousPublisherNotAllowed);
        };
        if !is_valid_secret_name(&request.name) {
            return refused(StoreRefusalReason::InvalidSecretName);
        }
        if let Err(problem) = validate_labels(&request.labels) {
            debug!(%stored_by, %problem, "a store was refused for its labels");
            return refused(StoreRefusalReason::InvalidLabels);
        }
        // An agent's escrow key is its custodian's to store, once, and no one else's (docs/architecture/key-custody.md,
        // "Part A's bootstrap: the custodian"): not the agent's, whose workload must never hold the key, and not
        // policy's to widen.
        if let Some(owner) = escrow_keypair_owner(&request.name).map(str::to_string) {
            return self
                .store_escrow_keypair(&owner, request, stored_by, instance.as_deref())
                .await;
        }

        let requester_certificate = match call_check_certificate(&self.ledger, &stored_by).await {
            Ok(Some(record)) => record,
            Ok(None) => return refused(StoreRefusalReason::RequesterCertificateNotFound),
            Err(e) => {
                error!("Failed to check requester certificate for {stored_by}: {e}");
                return StoreSecretReply::Failed { detail: e };
            }
        };

        // The ledger's record of the secret: its owner is what policy speaks of. An absent secret simply has no
        // owner, so a refusal below is the same whether or not one exists.
        let existing = match call_check_secret(&self.ledger, &request.name).await {
            Ok(existing) => existing,
            Err(e) => {
                error!("Failed to check stored secret {}: {e}", request.name);
                return StoreSecretReply::Failed { detail: e };
            }
        };
        let facts = polar_policy::Facts {
            owner: existing.as_ref().map(|s| s.stored_by.clone()),
            namespace: namespace_of(&request.name).map(str::to_string),
            labels: request.labels.clone(),
            topic: None,
        };
        let decision = self
            .policy
            .decide(
                &stored_by,
                Some(requester_certificate.certificate_pem.clone()),
                r#"Action::"store""#,
                &format!(r#"Secret::"{}""#, request.name),
                facts,
            )
            .await;
        let allowed = match decision {
            Ok(polar_policy::Decision::Allow) => true,
            Ok(polar_policy::Decision::Deny) => false,
            // Policy does not speak to it, or none is loaded yet: the floor decides.
            Ok(polar_policy::Decision::NotDetermined) | Err(LookupError::NoPolicyLoaded) => {
                floor_allows(&stored_by, &request.name)
            }
            // No decision is never a grant, and never a reason to fall back: try again.
            Err(LookupError::Unavailable(detail)) => {
                error!("Policy could not decide a store for {stored_by}: {detail}");
                return StoreSecretReply::Failed { detail };
            }
        };
        if !allowed {
            return refused(StoreRefusalReason::PolicyDenied);
        }

        self.append(request, stored_by).await
    }

    /// The escrow key of `owner`, stored by its custodian, within its window, its certificate naming the key, when
    /// nothing is stored under the name yet. Refused otherwise, saying nothing more than policy would.
    async fn store_escrow_keypair(
        &self,
        owner: &str,
        request: StoreSecretRequest,
        stored_by: String,
        instance: Option<&str>,
    ) -> StoreSecretReply {
        match crate::custodian::custodian(
            &self.ledger,
            Some(&stored_by),
            instance,
            cert_issuer_common::custody::CustodyPermission::Escrow,
        )
        .await
        {
            Ok(custodian) if custodian.workload == owner => {}
            Ok(_) | Err(_) => {
                tracing::warn!(%stored_by, %owner, "an escrow key stored by someone other than its custodian: refused");
                return refused(StoreRefusalReason::PolicyDenied);
            }
        }
        match call_check_secret(&self.ledger, &request.name).await {
            Ok(None) => {}
            Ok(Some(_)) => return refused(StoreRefusalReason::NameTaken),
            Err(e) => {
                error!("Failed to check stored secret {}: {e}", request.name);
                return StoreSecretReply::Failed { detail: e };
            }
        }
        self.append(request, stored_by).await
    }

    /// Stores `request` as `stored_by`'s, once it is sealed to this agent's own key.
    async fn append(&self, request: StoreSecretRequest, stored_by: String) -> StoreSecretReply {
        if request.secret.encrypted_to_pem() != self.escrow_keypair.public_key_pem() {
            return refused(StoreRefusalReason::WrongEscrowKey);
        }

        let event = CredentialsEvent::SecretStored {
            name: request.name,
            secret: request.secret,
            stored_by,
            timestamp: polar::time::now_millis(),
            labels: request.labels,
        };

        match ractor::rpc::call(
            &self.ledger,
            |reply| LedgerMessage::StoreSecret { event, reply },
            None,
        )
        .await
        {
            Ok(ractor::rpc::CallResult::Success(Ok(()))) => StoreSecretReply::Stored,
            Ok(ractor::rpc::CallResult::Success(Err(SecretStoreError::NameTaken))) => {
                refused(StoreRefusalReason::NameTaken)
            }
            Ok(ractor::rpc::CallResult::Success(Err(SecretStoreError::LabelsChanged))) => {
                refused(StoreRefusalReason::LabelsChanged)
            }
            Ok(ractor::rpc::CallResult::Success(Err(SecretStoreError::Ledger(e)))) => {
                tracing::error!("Failed to append SecretStored to the ledger: {e}");
                StoreSecretReply::Failed { detail: e }
            }
            Ok(other) => StoreSecretReply::Failed {
                detail: format!("unexpected CallResult: {other:?}"),
            },
            Err(e) => StoreSecretReply::Failed {
                detail: e.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieve_secret::tests::{
        registered_policy, seed_mtls_certificate, spawn_test_ledger,
    };
    use credentials_agent_common::escrow_secret::EscrowedSecret;

    struct World {
        handler: StoreSecretHandler,
        ledger: ActorRef<LedgerMessage>,
        keypair: Arc<EscrowKeypair>,
    }

    /// What the policy actor holds.
    enum Policy<'a> {
        /// Running, with no rules loaded: a fresh cluster before the seed.
        NoRules,
        /// Running, holding these rules.
        Rules(&'a str),
        /// Nothing is running under the name the handler looks for.
        NotRunning,
    }

    /// A handler, and the certificates on record for the identities a test acts as.
    async fn world(policy: Policy<'_>, identities: &[&str]) -> World {
        let ledger = spawn_test_ledger().await;
        let keypair = crate::escrow::tests::test_keypair();
        for identity in identities {
            seed_mtls_certificate(&ledger, identity).await;
        }
        let lookup = match policy {
            Policy::NoRules => registered_policy(None).await,
            Policy::Rules(source) => registered_policy(Some(source.to_string())).await,
            Policy::NotRunning => polar_agent_builder::PolicyLookup::named("no-such-policy-actor"),
        };
        World {
            handler: StoreSecretHandler {
                escrow_keypair: keypair.clone(),
                ledger: ledger.clone(),
                policy: lookup,
            },
            ledger,
            keypair,
        }
    }

    fn request(w: &World, name: &str, plaintext: &[u8]) -> StoreSecretRequest {
        request_with(w, name, plaintext, &[])
    }

    fn request_with(
        w: &World,
        name: &str,
        plaintext: &[u8],
        labels: &[(&str, &str)],
    ) -> StoreSecretRequest {
        StoreSecretRequest {
            name: name.to_string(),
            secret: EscrowedSecret::encrypt(plaintext, w.keypair.public_key_pem()).unwrap(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    async fn held(w: &World, name: &str) -> Option<crate::state::StoredSecret> {
        ractor::call!(w.ledger, |reply| LedgerMessage::CheckSecret {
            name: name.to_string(),
            reply,
        })
        .unwrap()
    }

    fn refused(reason: StoreRefusalReason) -> StoreSecretReply {
        StoreSecretReply::Refused { reason }
    }

    const PERMIT_ANY_STORE: &str = r#"permit (principal, action == Action::"store", resource);"#;

    // ---- the floor: no policy has spoken, so an identity stores under its own name --------------------------

    #[tokio::test]
    async fn with_no_policy_loaded_an_identity_may_store_under_its_own_namespace() {
        let w = world(Policy::NoRules, &["alice"]).await;
        let reply = w
            .handler
            .handle(
                request_with(&w, "alice/github", b"one", &[("kind", "token")]),
                Some("alice".into()),
            )
            .await;
        assert_eq!(reply, StoreSecretReply::Stored);
        let stored = held(&w, "alice/github").await.unwrap();
        assert_eq!(stored.stored_by, "alice");
        assert_eq!(stored.labels.get("kind").map(String::as_str), Some("token"));
    }

    // ---- an escrow key: its custodian's to store, once, and no one else's, whatever policy says ------------------

    /// A world whose ledger holds the custodian of `crate::custodian::tests::X`, its certificate naming `permissions`.
    async fn world_with_custodian(
        policy: Policy<'_>,
        permissions: &[cert_issuer_common::custody::CustodyPermission],
        age_millis: i64,
    ) -> World {
        let mut w = world(policy, &[crate::custodian::tests::X]).await;
        let ledger = crate::custodian::tests::ledger_with_custodian(permissions, age_millis).await;
        seed_mtls_certificate(&ledger, crate::custodian::tests::X).await;
        w.handler.ledger = ledger.clone();
        w.ledger = ledger;
        w
    }

    #[tokio::test]
    async fn an_escrow_key_is_stored_by_its_workload_s_custodian_once() {
        use crate::custodian::tests::{CUSTODIAN, X};
        use cert_issuer_common::custody::CustodyPermission;
        let w = world_with_custodian(Policy::NoRules, &[CustodyPermission::Escrow], 0).await;
        let name = credentials_agent_common::agent_escrow_keypair_bootstrap::agent_escrow_keypair_secret_name(X);
        assert_eq!(
            w.handler
                .handle(request(&w, &name, b"k"), Some(CUSTODIAN.into()))
                .await,
            StoreSecretReply::Stored
        );
        assert_eq!(held(&w, &name).await.unwrap().stored_by, CUSTODIAN);
        // Never replaced, not even by the custodian that stored it.
        assert_eq!(
            w.handler
                .handle(request(&w, &name, b"another"), Some(CUSTODIAN.into()))
                .await,
            refused(StoreRefusalReason::NameTaken)
        );
    }

    #[tokio::test]
    async fn an_escrow_key_is_stored_by_no_one_else_however_policy_reads() {
        use crate::custodian::tests::{CUSTODIAN, X};
        use cert_issuer_common::custody::CustodyPermission;
        use credentials_agent_common::agent_escrow_keypair_bootstrap::agent_escrow_keypair_secret_name;
        use credentials_agent_common::custodian::CUSTODIAN_WINDOW_SECS;
        for policy in [Policy::NoRules, Policy::Rules(PERMIT_ANY_STORE)] {
            let w = world_with_custodian(policy, &[CustodyPermission::Escrow], 0).await;
            for (name, by) in [
                // the workload itself, whose workload must never hold the key
                (agent_escrow_keypair_secret_name(X), X),
                // the custodian, for another workload's key
                (
                    agent_escrow_keypair_secret_name("other.polar.serviceaccount.cluster.local"),
                    CUSTODIAN,
                ),
            ] {
                assert_eq!(
                    w.handler
                        .handle(request(&w, &name, b"k"), Some(by.into()))
                        .await,
                    refused(StoreRefusalReason::PolicyDenied),
                    "{name} by {by}"
                );
            }
        }
        // A custodian whose certificate does not name the key, and one past its window.
        for (permissions, age) in [
            (vec![CustodyPermission::Client], 0),
            (
                vec![CustodyPermission::Escrow],
                (CUSTODIAN_WINDOW_SECS + 1) * 1000,
            ),
        ] {
            let w = world_with_custodian(Policy::Rules(PERMIT_ANY_STORE), &permissions, age).await;
            let name = agent_escrow_keypair_secret_name(X);
            assert_eq!(
                w.handler
                    .handle(request(&w, &name, b"k"), Some(CUSTODIAN.into()))
                    .await,
                refused(StoreRefusalReason::PolicyDenied),
                "{permissions:?} {age}"
            );
        }
    }

    #[tokio::test]
    async fn with_no_rule_about_it_the_floor_decides_and_refuses_what_is_not_the_identitys_own() {
        // A policy is loaded, and says nothing about stores.
        let w = world(
            Policy::Rules(r#"permit (principal, action == Action::"retrieve", resource) when { resource has owner && resource.owner == principal };"#),
            &["alice"],
        )
        .await;
        assert_eq!(
            w.handler
                .handle(request(&w, "alice/github", b"x"), Some("alice".into()))
                .await,
            StoreSecretReply::Stored
        );
        for name in [
            "github",
            "bob/github",
            "alice",
            "alice/",
            "/github",
            "agent-escrow-keypair:bob",
        ] {
            assert_eq!(
                w.handler
                    .handle(request(&w, name, b"x"), Some("alice".into()))
                    .await,
                refused(StoreRefusalReason::PolicyDenied),
                "{name:?}"
            );
            assert!(held(&w, name).await.is_none(), "{name:?} was stored");
        }
    }

    #[tokio::test]
    async fn an_identity_cannot_store_under_another_identitys_namespace() {
        let w = world(Policy::NoRules, &["alice", "bob"]).await;
        assert_eq!(
            w.handler
                .handle(request(&w, "alice/github", b"mine"), Some("bob".into()))
                .await,
            refused(StoreRefusalReason::PolicyDenied)
        );
    }

    // ---- Cedar first -------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_rule_may_permit_more_than_the_floor_does() {
        let w = world(Policy::Rules(PERMIT_ANY_STORE), &["alice"]).await;
        assert_eq!(
            w.handler
                .handle(request(&w, "shared/github", b"x"), Some("alice".into()))
                .await,
            StoreSecretReply::Stored
        );
    }

    #[tokio::test]
    async fn a_forbid_overrides_the_floor() {
        let w = world(
            Policy::Rules(r#"forbid (principal, action == Action::"store", resource);"#),
            &["alice"],
        )
        .await;
        assert_eq!(
            w.handler
                .handle(request(&w, "alice/github", b"x"), Some("alice".into()))
                .await,
            refused(StoreRefusalReason::PolicyDenied)
        );
        assert!(held(&w, "alice/github").await.is_none());
    }

    #[tokio::test]
    async fn a_rule_can_speak_of_the_labels_a_secret_is_stored_with() {
        let w = world(
            Policy::Rules(r#"forbid (principal, action == Action::"store", resource) when { resource.hasTag("kind") && resource.getTag("kind") == "user-password" };"#),
            &["alice"],
        )
        .await;
        assert_eq!(
            w.handler
                .handle(
                    request_with(&w, "alice/a", b"x", &[("kind", "token")]),
                    Some("alice".into())
                )
                .await,
            StoreSecretReply::Stored
        );
        assert_eq!(
            w.handler
                .handle(
                    request_with(&w, "alice/b", b"x", &[("kind", "user-password")]),
                    Some("alice".into())
                )
                .await,
            refused(StoreRefusalReason::PolicyDenied)
        );
    }

    #[tokio::test]
    async fn a_policy_that_cannot_be_reached_is_a_failure_to_retry_and_never_the_floor() {
        let w = world(Policy::NotRunning, &["alice"]).await;
        let reply = w
            .handler
            .handle(request(&w, "alice/github", b"x"), Some("alice".into()))
            .await;
        assert!(
            matches!(reply, StoreSecretReply::Failed { .. }),
            "{reply:?}"
        );
        assert!(held(&w, "alice/github").await.is_none());
    }

    // ---- what is checked, and in what order --------------------------------------------------------------

    #[tokio::test]
    async fn a_name_that_cannot_be_a_policy_resource_is_refused_before_policy_is_asked() {
        // Policy is not running, so were it asked the reply would be a failure.
        let w = world(Policy::NotRunning, &["alice"]).await;
        for name in [
            "",
            "alice/with\"quote",
            "alice/with space",
            &"a".repeat(129),
        ] {
            assert_eq!(
                w.handler
                    .handle(request(&w, name, b"x"), Some("alice".into()))
                    .await,
                refused(StoreRefusalReason::InvalidSecretName),
                "{name:?}"
            );
        }
    }

    #[tokio::test]
    async fn labels_that_are_not_well_formed_are_refused() {
        let w = world(Policy::NoRules, &["alice"]).await;
        for labels in [
            vec![("Kind", "token")],
            vec![("owner", "me")],
            vec![("kind", "")],
        ] {
            assert_eq!(
                w.handler
                    .handle(
                        request_with(&w, "alice/github", b"x", &labels),
                        Some("alice".into())
                    )
                    .await,
                refused(StoreRefusalReason::InvalidLabels),
                "{labels:?}"
            );
        }
        assert!(held(&w, "alice/github").await.is_none());
    }

    #[tokio::test]
    async fn a_requester_with_no_certificate_on_record_is_refused() {
        let w = world(Policy::NoRules, &[]).await;
        assert_eq!(
            w.handler
                .handle(request(&w, "alice/github", b"x"), Some("alice".into()))
                .await,
            refused(StoreRefusalReason::RequesterCertificateNotFound)
        );
    }

    #[tokio::test]
    async fn an_anonymous_store_and_one_to_the_wrong_key_are_still_refused() {
        let w = world(Policy::NoRules, &["alice"]).await;
        assert_eq!(
            w.handler.handle(request(&w, "alice/s", b"x"), None).await,
            refused(StoreRefusalReason::AnonymousPublisherNotAllowed)
        );
        let other_key = crate::escrow::EscrowKeypair::generate().unwrap();
        let wrong = StoreSecretRequest {
            name: "alice/s".to_string(),
            secret: EscrowedSecret::encrypt(b"x", other_key.public_key_pem()).unwrap(),
            labels: Default::default(),
        };
        assert_eq!(
            w.handler.handle(wrong, Some("alice".into())).await,
            refused(StoreRefusalReason::WrongEscrowKey)
        );
        assert!(held(&w, "alice/s").await.is_none());
    }

    #[tokio::test]
    async fn policy_is_asked_before_the_key_is_looked_at() {
        let other_key = crate::escrow::EscrowKeypair::generate().unwrap();
        let wrong_key = |name: &str| StoreSecretRequest {
            name: name.to_string(),
            secret: EscrowedSecret::encrypt(b"x", other_key.public_key_pem()).unwrap(),
            labels: Default::default(),
        };
        // Refused by policy: the reply is the policy's, not a word about the key.
        let w = world(Policy::NoRules, &["alice"]).await;
        assert_eq!(
            w.handler
                .handle(wrong_key("bob/github"), Some("alice".into()))
                .await,
            refused(StoreRefusalReason::PolicyDenied)
        );
        // Policy cannot be reached: that is the reply, not a word about the key either.
        let w = world(Policy::NotRunning, &["alice"]).await;
        assert!(matches!(
            w.handler
                .handle(wrong_key("alice/github"), Some("alice".into()))
                .await,
            StoreSecretReply::Failed { .. }
        ));
    }

    // ---- ownership and labels, in the ledger -------------------------------------------------------------

    #[tokio::test]
    async fn the_identity_that_stored_a_secret_may_replace_it_with_the_same_labels() {
        let w = world(Policy::NoRules, &["alice"]).await;
        let labels = [("kind", "token")];
        assert_eq!(
            w.handler
                .handle(
                    request_with(&w, "alice/github", b"one", &labels),
                    Some("alice".into())
                )
                .await,
            StoreSecretReply::Stored
        );
        let before = held(&w, "alice/github").await.unwrap();
        assert_eq!(
            w.handler
                .handle(
                    request_with(&w, "alice/github", b"two", &labels),
                    Some("alice".into())
                )
                .await,
            StoreSecretReply::Stored
        );
        let after = held(&w, "alice/github").await.unwrap();
        assert_ne!(
            after.secret, before.secret,
            "the new value replaced the old"
        );
        assert_eq!(after.labels, before.labels);
    }

    #[tokio::test]
    async fn a_replacement_with_other_labels_is_refused_and_changes_nothing() {
        let w = world(Policy::NoRules, &["alice"]).await;
        w.handler
            .handle(
                request_with(&w, "alice/github", b"one", &[("kind", "token")]),
                Some("alice".into()),
            )
            .await;
        let before = held(&w, "alice/github").await.unwrap();
        for labels in [vec![("kind", "user-password")], vec![]] {
            assert_eq!(
                w.handler
                    .handle(
                        request_with(&w, "alice/github", b"two", &labels),
                        Some("alice".into())
                    )
                    .await,
                refused(StoreRefusalReason::LabelsChanged),
                "{labels:?}"
            );
        }
        assert_eq!(held(&w, "alice/github").await.unwrap(), before);
    }

    #[tokio::test]
    async fn another_identity_is_told_nothing_about_a_name_it_may_not_store_under() {
        let w = world(Policy::NoRules, &["alice", "bob"]).await;
        w.handler
            .handle(request(&w, "alice/github", b"mine"), Some("alice".into()))
            .await;
        // The same reply for a name that exists and one that does not: policy answers before the ledger.
        let existing = w
            .handler
            .handle(request(&w, "alice/github", b"x"), Some("bob".into()))
            .await;
        let absent = w
            .handler
            .handle(request(&w, "alice/other", b"x"), Some("bob".into()))
            .await;
        assert_eq!(existing, refused(StoreRefusalReason::PolicyDenied));
        assert_eq!(existing, absent);
    }

    #[tokio::test]
    async fn when_policy_lets_another_identity_in_it_still_cannot_take_a_secret_over() {
        let w = world(Policy::Rules(PERMIT_ANY_STORE), &["alice", "bob"]).await;
        w.handler
            .handle(request(&w, "shared/github", b"mine"), Some("alice".into()))
            .await;
        let before = held(&w, "shared/github").await.unwrap();
        assert_eq!(
            w.handler
                .handle(request(&w, "shared/github", b"yours"), Some("bob".into()))
                .await,
            refused(StoreRefusalReason::NameTaken)
        );
        assert_eq!(
            held(&w, "shared/github").await.unwrap(),
            before,
            "neither the value nor the owner changed"
        );
    }

    #[tokio::test]
    async fn two_identities_racing_for_a_fresh_name_cannot_both_have_it() {
        for _ in 0..20 {
            let w = world(Policy::Rules(PERMIT_ANY_STORE), &["one", "two"]).await;
            let (one, two) = tokio::join!(
                w.handler
                    .handle(request(&w, "contested", b"one"), Some("one".into())),
                w.handler
                    .handle(request(&w, "contested", b"two"), Some("two".into())),
            );
            let stored = [&one, &two]
                .iter()
                .filter(|r| ***r == StoreSecretReply::Stored)
                .count();
            assert_eq!(stored, 1, "{one:?} / {two:?}");
            let owner = held(&w, "contested").await.unwrap().stored_by;
            let winner = if one == StoreSecretReply::Stored {
                "one"
            } else {
                "two"
            };
            assert_eq!(owner, winner);
        }
    }
}
