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

//! Retrieve half of the general secret-escrow mechanism -- the real
//! decrypt-reencrypt-destroy relay credentials-agent-architecture.md's
//! own "Secret escrow" section describes. Mirrors StoreSecretHandler's
//! own shape and validation discipline, with the actual relay logic
//! this document's own EscrowedSecret::decrypt was built for two
//! weeks ago but never yet exercised in a real handler.

use crate::agent_escrow_key::call_check_agent_escrow_key;
use crate::escrow::EscrowKeypair;
use crate::ledger::LedgerMessage;
use credentials_agent_common::escrow_secret::EscrowedSecret;
use credentials_agent_common::retrieve_secret::{
    RetrieveRefusalReason, RetrieveSecretReply, RetrieveSecretRequest, is_valid_secret_name,
};
use ractor::ActorRef;
use std::sync::Arc;
use tracing::error;

pub struct RetrieveSecretHandler {
    pub escrow_keypair: Arc<EscrowKeypair>,
    pub ledger: ActorRef<LedgerMessage>,
    pub policy: polar_agent_builder::PolicyLookup,
}

impl RetrieveSecretHandler {
    /// In order, refusing at the first failure, and saying nothing about whether a secret exists until the
    /// requester is permitted it:
    ///
    /// 1. the publisher is known, and the name is one a secret can have;
    /// 2. the requester's mTLS certificate is on record: policy reads the permissions in it;
    /// 3. policy lets this identity retrieve this secret (fails closed: no rule, no secret);
    /// 4. the requester has an agent escrow key, the RSA service identity key a secret is encrypted to.
    ///    Never the mTLS key: that one signs, and is not for service operations;
    /// 5. the secret exists, and is decrypted with this agent's escrow key and encrypted at once to the
    ///    requester's.
    pub async fn handle(
        &self,
        request: RetrieveSecretRequest,
        requested_by: Option<String>,
    ) -> RetrieveSecretReply {
        let Some(requested_by) = requested_by else {
            return RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::AnonymousPublisherNotAllowed,
            };
        };
        if !is_valid_secret_name(&request.name) {
            return RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::InvalidSecretName,
            };
        }

        let requester_certificate = match call_check_certificate(&self.ledger, &requested_by).await
        {
            Ok(Some(record)) => record,
            Ok(None) => {
                return RetrieveSecretReply::Refused {
                    reason: RetrieveRefusalReason::RequesterCertificateNotFound,
                };
            }
            Err(e) => {
                error!("Failed to check requester certificate for {requested_by}: {e}");
                return RetrieveSecretReply::Failed { detail: e };
            }
        };

        // The ledger's own record of the secret: its owner is what policy speaks of. Whether it exists is
        // not said to anyone policy does not permit, since an absent secret simply has no owner and every
        // refusal below is the same reply.
        let stored = match call_check_secret(&self.ledger, &request.name).await {
            Ok(stored) => stored,
            Err(e) => {
                error!("Failed to check stored secret {}: {e}", request.name);
                return RetrieveSecretReply::Failed { detail: e };
            }
        };
        let facts = polar_policy::Facts {
            owner: stored.as_ref().map(|s| s.stored_by.clone()),
        };

        // No decision (the policy is not running yet, or failed) is never a grant.
        let decision = match self
            .policy
            .check(
                &requested_by,
                Some(requester_certificate.certificate_pem.clone()),
                r#"Action::"retrieve""#,
                &format!(r#"Secret::"{}""#, request.name),
                facts,
            )
            .await
        {
            Ok(decision) => decision,
            Err(detail) => {
                error!("Policy could not decide for {requested_by}: {detail}");
                return RetrieveSecretReply::Failed { detail };
            }
        };
        if decision != polar_policy::Decision::Allow {
            return RetrieveSecretReply::Denied {
                reason: "policy does not permit this identity to retrieve this secret".to_string(),
            };
        }

        let escrow_record = match call_check_agent_escrow_key(&self.ledger, &requested_by).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return RetrieveSecretReply::Refused {
                    reason: RetrieveRefusalReason::RequesterEscrowKeyNotFound,
                };
            }
            Err(e) => {
                error!("Failed to check agent escrow key for {requested_by}: {e}");
                return RetrieveSecretReply::Failed { detail: e };
            }
        };
        let target_public_key_pem = match cert_issuer_common::public_key::extract_public_key_pem(
            &escrow_record.certificate_pem,
        ) {
            Ok(pem) => pem,
            Err(e) => {
                error!("Escrow certificate for {requested_by} failed public-key extraction: {e}");
                return RetrieveSecretReply::Failed {
                    detail: e.to_string(),
                };
            }
        };

        let Some(stored) = stored else {
            return RetrieveSecretReply::NotFound;
        };
        let stored_secret = stored.secret;

        // The decrypted plaintext is Zeroizing: wiped the instant re-encryption completes, never left
        // to an ordinary drop.
        let plaintext = match stored_secret.decrypt(self.escrow_keypair.private_key()) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to decrypt stored secret {}: {e}", request.name);
                return RetrieveSecretReply::Failed {
                    detail: e.to_string(),
                };
            }
        };
        match EscrowedSecret::encrypt(&plaintext, &target_public_key_pem) {
            Ok(secret) => RetrieveSecretReply::Found { secret },
            Err(e) => {
                error!(
                    "Failed to re-encrypt secret {} for {requested_by}: {e}",
                    request.name
                );
                RetrieveSecretReply::Failed {
                    detail: e.to_string(),
                }
            }
        }
    }
}

pub(crate) async fn call_check_certificate(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
) -> Result<Option<crate::state::CertificateRecord>, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckCertificate {
            identity: identity.to_string(),
            reply,
        },
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(result)) => Ok(result),
        Ok(other) => Err(format!("unexpected CallResult: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) async fn call_check_secret(
    ledger: &ActorRef<LedgerMessage>,
    name: &str,
) -> Result<Option<crate::state::StoredSecret>, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckSecret {
            name: name.to_string(),
            reply,
        },
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(result)) => Ok(result),
        Ok(other) => Err(format!("unexpected CallResult: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ledger::{LedgerActor, LedgerActorArgs};
    use credentials_agent_common::events::CredentialsEvent;

    async fn spawn_test_ledger() -> ActorRef<LedgerMessage> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::mem::forget(dir);
        let (ledger, _) = ractor::Actor::spawn(
            None,
            LedgerActor,
            LedgerActorArgs {
                log_path: path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();
        ledger
    }

    /// A running policy actor, registered under a name of its own (names are process-wide and tests run
    /// side by side), holding `policy_source` if there is any, and the lookup that finds it. With none,
    /// nothing is permitted.
    pub(crate) async fn registered_policy(
        policy_source: Option<String>,
    ) -> polar_agent_builder::PolicyLookup {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let name = format!(
            "credentials-test-policy-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        let (permissions_actor, _) =
            ractor::Actor::spawn(None, polar_permissions::PermissionsActor, ())
                .await
                .unwrap();
        let (policy_actor, _) = ractor::Actor::spawn(
            Some(name.clone()),
            polar_policy::PolicyActor,
            polar_policy::PolicyActorArgs { permissions_actor },
        )
        .await
        .unwrap();
        if let Some(policy_source) = policy_source {
            ractor::call!(policy_actor, |reply| {
                polar_policy::PolicyMessage::UpdatePolicy {
                    policy_source,
                    reply,
                }
            })
            .unwrap()
            .unwrap();
        }
        polar_agent_builder::PolicyLookup::named(name)
    }

    /// A policy with one rule: `identity` may retrieve `Secret::"name"`. With none, nothing is.
    pub(crate) async fn policy_actor(
        permit: Option<(&str, &str)>,
    ) -> polar_agent_builder::PolicyLookup {
        registered_policy(permit.map(|(identity, name)| {
            format!(
                r#"permit(principal == Identity::"{identity}", action == Action::"retrieve", resource == Secret::"{name}");"#
            )
        }))
        .await
    }

    async fn append(ledger: &ActorRef<LedgerMessage>, event: CredentialsEvent) {
        let _ = ractor::rpc::call(ledger, |reply| LedgerMessage::Append { event, reply }, None)
            .await
            .unwrap();
    }

    /// The requester's mTLS certificate, as workloads really have them: it signs and does not
    /// encrypt (rcgen's default is ECDSA P-256), so a secret can never be encrypted to it.
    pub(crate) async fn seed_mtls_certificate(ledger: &ActorRef<LedgerMessage>, identity: &str) {
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let certificate_pem =
            rcgen::CertificateParams::new(vec![format!("{identity}.polar.svc.cluster.local")])
                .unwrap()
                .self_signed(&key_pair)
                .unwrap()
                .pem();
        append(
            ledger,
            CredentialsEvent::IssuanceCompleted {
                identity: identity.to_string(),
                instance_binding: "pod-uid-1".to_string(),
                serial: format!("{identity}-serial-1"),
                timestamp: 1000,
                certificate_pem,
            },
        )
        .await;
    }

    /// The requester's agent escrow certificate: the RSA service identity. Returns the private key.
    pub(crate) async fn seed_escrow_certificate(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
    ) -> rsa::RsaPrivateKey {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&pem).unwrap();
        let certificate_pem =
            rcgen::CertificateParams::new(vec![format!("{identity}.polar.svc.cluster.local")])
                .unwrap()
                .self_signed(&key_pair)
                .unwrap()
                .pem();
        append(
            ledger,
            CredentialsEvent::AgentEscrowIssuanceCompleted {
                identity: identity.to_string(),
                instance_binding: "pod-uid-1".to_string(),
                serial: format!("{identity}-escrow-serial-1"),
                timestamp: 1000,
                certificate_pem,
            },
        )
        .await;
        private_key
    }

    pub(crate) async fn store_secret(
        ledger: &ActorRef<LedgerMessage>,
        escrow_keypair: &EscrowKeypair,
        name: &str,
        plaintext: &[u8],
    ) {
        store_secret_by(
            ledger,
            escrow_keypair,
            name,
            plaintext,
            "some-storing-agent",
        )
        .await
    }

    pub(crate) async fn store_secret_by(
        ledger: &ActorRef<LedgerMessage>,
        escrow_keypair: &EscrowKeypair,
        name: &str,
        plaintext: &[u8],
        stored_by: &str,
    ) {
        let secret = EscrowedSecret::encrypt(plaintext, escrow_keypair.public_key_pem()).unwrap();
        append(
            ledger,
            CredentialsEvent::SecretStored {
                name: name.to_string(),
                secret,
                stored_by: stored_by.to_string(),
                timestamp: 1000,
            },
        )
        .await;
    }

    fn request(name: &str) -> RetrieveSecretRequest {
        RetrieveSecretRequest {
            name: name.to_string(),
        }
    }

    struct World {
        handler: RetrieveSecretHandler,
        ledger: ActorRef<LedgerMessage>,
        escrow_keypair: Arc<EscrowKeypair>,
    }

    async fn world(permit: Option<(&str, &str)>) -> World {
        let ledger = spawn_test_ledger().await;
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let handler = RetrieveSecretHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
            policy: policy_actor(permit).await,
        };
        World {
            handler,
            ledger,
            escrow_keypair,
        }
    }

    #[tokio::test]
    async fn a_permitted_secret_is_reencrypted_to_the_requesters_escrow_key_not_its_mtls_key() {
        let w = world(Some(("requester", "gitlab-token"))).await;
        seed_mtls_certificate(&w.ledger, "requester").await;
        let escrow_private_key = seed_escrow_certificate(&w.ledger, "requester").await;
        store_secret(
            &w.ledger,
            &w.escrow_keypair,
            "gitlab-token",
            b"a real secret",
        )
        .await;

        let reply = w
            .handler
            .handle(request("gitlab-token"), Some("requester".into()))
            .await;

        match reply {
            RetrieveSecretReply::Found { secret } => {
                // Decrypts with the requester's escrow private key, so it was encrypted to that key
                // and not passed through, and not encrypted to the mTLS key (which cannot encrypt).
                let decrypted = secret.decrypt(&escrow_private_key).unwrap();
                assert_eq!(&*decrypted, b"a real secret");
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    /// The first policy of a cluster (`infra/seeds/signed`): an owner may retrieve what it escrowed, and a frozen
    /// secret is retrievable by nobody.
    const BASELINE: &str = include_str!("../../../../../infra/seeds/signed/policy.cedar");

    async fn baseline_world() -> World {
        let mut w = world(None).await;
        w.handler.policy = registered_policy(Some(BASELINE.to_string())).await;
        w
    }

    #[tokio::test]
    async fn under_the_first_policy_an_agent_retrieves_what_it_escrowed_itself() {
        let w = baseline_world().await;
        seed_mtls_certificate(&w.ledger, "git-repo-observer").await;
        let key = seed_escrow_certificate(&w.ledger, "git-repo-observer").await;
        store_secret_by(
            &w.ledger,
            &w.escrow_keypair,
            "github-sei",
            b"token",
            "git-repo-observer",
        )
        .await;
        match w
            .handler
            .handle(request("github-sei"), Some("git-repo-observer".into()))
            .await
        {
            RetrieveSecretReply::Found { secret } => {
                assert_eq!(&*secret.decrypt(&key).unwrap(), b"token")
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn under_the_first_policy_another_agents_secret_is_denied_the_same_as_one_that_is_not_there()
     {
        let w = baseline_world().await;
        seed_mtls_certificate(&w.ledger, "intruder").await;
        seed_escrow_certificate(&w.ledger, "intruder").await;
        store_secret_by(
            &w.ledger,
            &w.escrow_keypair,
            "github-sei",
            b"token",
            "git-repo-observer",
        )
        .await;
        let theirs = w
            .handler
            .handle(request("github-sei"), Some("intruder".into()))
            .await;
        let absent = w
            .handler
            .handle(request("never-stored"), Some("intruder".into()))
            .await;
        assert!(
            matches!(theirs, RetrieveSecretReply::Denied { .. }),
            "{theirs:?}"
        );
        assert_eq!(theirs, absent);
    }

    #[tokio::test]
    async fn with_no_policy_running_a_secret_is_never_released_and_the_reply_is_a_failure() {
        let mut w = world(Some(("requester", "gitlab-token"))).await;
        // The agent's policy actor has not started (or is gone): nothing is registered under the name.
        w.handler.policy = polar_agent_builder::PolicyLookup::named("no-such-policy-actor");
        seed_mtls_certificate(&w.ledger, "requester").await;
        seed_escrow_certificate(&w.ledger, "requester").await;
        store_secret(&w.ledger, &w.escrow_keypair, "gitlab-token", b"x").await;

        let reply = w
            .handler
            .handle(request("gitlab-token"), Some("requester".into()))
            .await;
        match reply {
            RetrieveSecretReply::Failed { detail } => assert!(detail.contains("not running yet")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_secret_policy_does_not_permit_is_denied_and_nothing_is_said_about_whether_it_exists()
    {
        // A rule for a different secret: policy is loaded and says no.
        let w = world(Some(("requester", "something-else"))).await;
        seed_mtls_certificate(&w.ledger, "requester").await;
        seed_escrow_certificate(&w.ledger, "requester").await;
        store_secret(&w.ledger, &w.escrow_keypair, "gitlab-token", b"x").await;

        let exists = w
            .handler
            .handle(request("gitlab-token"), Some("requester".into()))
            .await;
        let absent = w
            .handler
            .handle(request("never-stored"), Some("requester".into()))
            .await;
        assert!(
            matches!(exists, RetrieveSecretReply::Denied { .. }),
            "{exists:?}"
        );
        assert_eq!(
            exists, absent,
            "a denial must not reveal which secrets exist"
        );
    }

    #[tokio::test]
    async fn with_no_policy_loaded_yet_nothing_is_retrieved_and_it_is_a_failure_not_a_denial() {
        // The credentials agent starts with no policy until one arrives. It fails closed, and as a
        // failure so that a client knows to try again, and again says nothing about what exists.
        let w = world(None).await;
        seed_mtls_certificate(&w.ledger, "requester").await;
        seed_escrow_certificate(&w.ledger, "requester").await;
        store_secret(&w.ledger, &w.escrow_keypair, "gitlab-token", b"x").await;

        let exists = w
            .handler
            .handle(request("gitlab-token"), Some("requester".into()))
            .await;
        let absent = w
            .handler
            .handle(request("never-stored"), Some("requester".into()))
            .await;
        assert!(
            matches!(exists, RetrieveSecretReply::Failed { .. }),
            "{exists:?}"
        );
        assert_eq!(exists, absent);
    }

    #[tokio::test]
    async fn another_identity_is_not_permitted_by_a_rule_for_this_one() {
        let w = world(Some(("someone-else", "gitlab-token"))).await;
        seed_mtls_certificate(&w.ledger, "requester").await;
        seed_escrow_certificate(&w.ledger, "requester").await;
        store_secret(&w.ledger, &w.escrow_keypair, "gitlab-token", b"x").await;
        let reply = w
            .handler
            .handle(request("gitlab-token"), Some("requester".into()))
            .await;
        assert!(
            matches!(reply, RetrieveSecretReply::Denied { .. }),
            "{reply:?}"
        );
    }

    #[tokio::test]
    async fn a_requester_with_no_escrow_key_is_refused_even_when_permitted() {
        let w = world(Some(("requester", "gitlab-token"))).await;
        seed_mtls_certificate(&w.ledger, "requester").await;
        store_secret(&w.ledger, &w.escrow_keypair, "gitlab-token", b"x").await;
        let reply = w
            .handler
            .handle(request("gitlab-token"), Some("requester".into()))
            .await;
        assert_eq!(
            reply,
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::RequesterEscrowKeyNotFound
            }
        );
    }

    #[tokio::test]
    async fn a_permitted_secret_that_was_never_stored_is_not_found() {
        let w = world(Some(("requester", "gitlab-token"))).await;
        seed_mtls_certificate(&w.ledger, "requester").await;
        seed_escrow_certificate(&w.ledger, "requester").await;
        let reply = w
            .handler
            .handle(request("gitlab-token"), Some("requester".into()))
            .await;
        assert_eq!(reply, RetrieveSecretReply::NotFound);
    }

    #[tokio::test]
    async fn anonymous_publisher_is_refused() {
        let w = world(None).await;
        let reply = w.handler.handle(request("gitlab-token"), None).await;
        assert_eq!(
            reply,
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::AnonymousPublisherNotAllowed
            }
        );
    }

    #[tokio::test]
    async fn missing_requester_certificate_is_refused() {
        let w = world(Some(("ghost", "gitlab-token"))).await;
        let reply = w
            .handler
            .handle(request("gitlab-token"), Some("ghost".into()))
            .await;
        assert_eq!(
            reply,
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::RequesterCertificateNotFound
            }
        );
    }

    #[tokio::test]
    async fn a_name_that_could_end_the_policy_resource_early_is_refused_before_any_lookup() {
        let w = world(None).await;
        seed_mtls_certificate(&w.ledger, "requester").await;
        let reply = w
            .handler
            .handle(
                request(r#"x"; permit(principal, action, resource); //"#),
                Some("requester".into()),
            )
            .await;
        assert_eq!(
            reply,
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::InvalidSecretName
            }
        );
    }
}
