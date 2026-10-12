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

//! Decrypt-on-behalf-of half of the general secret mechanism -- the
//! real, new case credentials_agent_common::decrypt_on_behalf_of's
//! own module doc describes. Mirrors RetrieveSecretHandler's own
//! shape and re-encryption discipline exactly, with one, real,
//! additional step neither RetrieveSecretHandler nor StoreSecretHandler
//! needed: a genuine, live PolicyActor query, since this requester is
//! genuinely, potentially a stranger to whatever originally encrypted
//! the ciphertext -- unlike retrieve, where the requester is always
//! the one, real, escrowed owner.

use crate::escrow::EscrowKeypair;
use crate::ledger::LedgerMessage;
use crate::retrieve_secret::call_check_certificate;
use credentials_agent_common::decrypt_on_behalf_of::{
    DecryptOnBehalfOfRefusalReason, DecryptOnBehalfOfReply, DecryptOnBehalfOfRequest,
};
use credentials_agent_common::escrow_secret::EscrowedSecret;
use ractor::ActorRef;
use std::sync::Arc;
use tracing::error;

pub struct DecryptOnBehalfOfHandler {
    pub escrow_keypair: Arc<EscrowKeypair>,
    pub ledger: ActorRef<LedgerMessage>,
    pub policy_actor: ActorRef<polar_policy::PolicyMessage>,
}

impl DecryptOnBehalfOfHandler {
    pub async fn handle(
        &self,
        request: DecryptOnBehalfOfRequest,
        requested_by: Option<String>,
    ) -> DecryptOnBehalfOfReply {
        let Some(requested_by) = requested_by else {
            return DecryptOnBehalfOfReply::Refused {
                reason: DecryptOnBehalfOfRefusalReason::AnonymousPublisherNotAllowed,
            };
        };

        // Step 1: find the requester's own, current certificate --
        // needed both for re-encryption below AND as the real,
        // direct input PolicyActor uses to resolve the requester's
        // own, actual permissions internally.
        let requester_certificate = match call_check_certificate(&self.ledger, &requested_by).await
        {
            Ok(Some(record)) => record,
            Ok(None) => {
                return DecryptOnBehalfOfReply::Refused {
                    reason: DecryptOnBehalfOfRefusalReason::RequesterCertificateNotFound,
                };
            }
            Err(e) => {
                error!("Failed to check requester certificate for {requested_by}: {e}");
                return DecryptOnBehalfOfReply::Failed { detail: e };
            }
        };

        // Step 2: the real, genuine policy decision -- before ever
        // touching the escrow private key at all.
        let decision = match ractor::rpc::call(
            &self.policy_actor,
            |reply| polar_policy::PolicyMessage::CheckAuthorization {
                requester_identity: requested_by.clone(),
                requester_certificate_pem: Some(requester_certificate.certificate_pem.clone()),
                action: r#"Action::"decrypt""#.to_string(),
                resource: request.resource_id.clone(),
                reply,
            },
            None,
        )
        .await
        {
            Ok(ractor::rpc::CallResult::Success(Ok(decision))) => decision,
            Ok(ractor::rpc::CallResult::Success(Err(e))) => {
                error!("Policy evaluation failed for {requested_by}: {e}");
                return DecryptOnBehalfOfReply::Failed {
                    detail: e.to_string(),
                };
            }
            Ok(other) => {
                return DecryptOnBehalfOfReply::Failed {
                    detail: format!("unexpected CallResult: {other:?}"),
                };
            }
            Err(e) => {
                return DecryptOnBehalfOfReply::Failed {
                    detail: e.to_string(),
                };
            }
        };

        if decision != polar_policy::Decision::Allow {
            return DecryptOnBehalfOfReply::Denied {
                reason: format!("policy denied decrypt access to {}", request.resource_id),
            };
        }

        // Step 3: the real target to re-encrypt to.
        let target_public_key_pem = match cert_issuer_common::public_key::extract_public_key_pem(
            &requester_certificate.certificate_pem,
        ) {
            Ok(pem) => pem,
            Err(e) => {
                error!(
                    "Requester certificate for {requested_by} failed public-key extraction: {e}"
                );
                return DecryptOnBehalfOfReply::Failed {
                    detail: e.to_string(),
                };
            }
        };

        // Step 4: the actual relay -- decrypt with this agent's own
        // escrow private key, immediately re-encrypt with the
        // requester's own public key. Mirrors
        // RetrieveSecretHandler::handle's own, real Zeroizing
        // discipline exactly.
        let plaintext = match request
            .ciphertext
            .decrypt(self.escrow_keypair.private_key())
        {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to decrypt on behalf of {requested_by}: {e}");
                return DecryptOnBehalfOfReply::Failed {
                    detail: e.to_string(),
                };
            }
        };

        let reencrypted = match EscrowedSecret::encrypt(&plaintext, &target_public_key_pem) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to re-encrypt for {requested_by}: {e}");
                return DecryptOnBehalfOfReply::Failed {
                    detail: e.to_string(),
                };
            }
        };
        // `plaintext` (Zeroizing<Vec<u8>>) goes out of scope here,
        // genuinely wiped rather than merely deallocated.

        DecryptOnBehalfOfReply::Allowed {
            secret: reencrypted,
        }
    }
}

#[cfg(test)]
mod tests {
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

    async fn spawn_policy_actor_permitting(
        identity: &str,
        resource_id: &str,
    ) -> ActorRef<polar_policy::PolicyMessage> {
        let (permissions_actor, _) =
            ractor::Actor::spawn(None, polar_permissions::PermissionsActor, ())
                .await
                .unwrap();
        let (policy_actor, _) = ractor::Actor::spawn(
            None,
            polar_policy::PolicyActor,
            polar_policy::PolicyActorArgs { permissions_actor },
        )
        .await
        .unwrap();

        let policy = format!(
            r#"
            permit(
                principal == Identity::"{identity}",
                action == Action::"decrypt",
                resource == {resource_id}
            );
            "#
        );
        let result = ractor::call!(policy_actor, |reply| {
            polar_policy::PolicyMessage::UpdatePolicy {
                policy_source: policy,
                reply,
            }
        })
        .unwrap();
        result.unwrap();

        policy_actor
    }

    async fn seed_requester_certificate(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
    ) -> rsa::RsaPrivateKey {
        let mut rng = rand::rng();
        let requester_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let requester_private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &requester_private_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let requester_key_pair = rcgen::KeyPair::from_pem(&requester_private_key_pem).unwrap();
        let requester_params =
            rcgen::CertificateParams::new(vec!["requester.polar.svc.cluster.local".to_string()])
                .unwrap();
        let requester_certificate_pem = requester_params
            .self_signed(&requester_key_pair)
            .unwrap()
            .pem();

        let _ = ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::IssuanceCompleted {
                    identity: identity.to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: format!("{identity}-serial-1"),
                    timestamp: 1000,
                    certificate_pem: requester_certificate_pem,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        requester_private_key
    }

    #[tokio::test]
    async fn a_genuinely_permitted_requester_receives_the_decrypted_secret_reencrypted_to_their_own_key()
     {
        let ledger = spawn_test_ledger().await;
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let resource_id = r#"KubeSecret::"namespace/pod/ENV_VAR""#;

        let requester_private_key = seed_requester_certificate(&ledger, "requester").await;
        let policy_actor = spawn_policy_actor_permitting("requester", resource_id).await;

        let original_plaintext = b"a real, plaintext env var value";
        let ciphertext =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();

        let handler = DecryptOnBehalfOfHandler {
            escrow_keypair,
            ledger,
            policy_actor,
        };

        let reply = handler
            .handle(
                DecryptOnBehalfOfRequest {
                    ciphertext,
                    resource_id: resource_id.to_string(),
                },
                Some("requester".to_string()),
            )
            .await;

        match reply {
            DecryptOnBehalfOfReply::Allowed { secret } => {
                let decrypted = secret.decrypt(&requester_private_key).unwrap();
                assert_eq!(&*decrypted, original_plaintext);
            }
            other => panic!("expected Allowed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_genuinely_denied_requester_never_receives_the_secret() {
        let ledger = spawn_test_ledger().await;
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let resource_id = r#"KubeSecret::"namespace/pod/ENV_VAR""#;

        seed_requester_certificate(&ledger, "requester").await;
        // A real policy permitting a completely different principal
        // -- this requester must genuinely be denied.
        let policy_actor =
            spawn_policy_actor_permitting("someone-else-entirely", resource_id).await;

        let ciphertext =
            EscrowedSecret::encrypt(b"secret", escrow_keypair.public_key_pem()).unwrap();

        let handler = DecryptOnBehalfOfHandler {
            escrow_keypair,
            ledger,
            policy_actor,
        };

        let reply = handler
            .handle(
                DecryptOnBehalfOfRequest {
                    ciphertext,
                    resource_id: resource_id.to_string(),
                },
                Some("requester".to_string()),
            )
            .await;

        assert!(matches!(reply, DecryptOnBehalfOfReply::Denied { .. }));
    }

    #[tokio::test]
    async fn anonymous_publisher_is_refused() {
        let ledger = spawn_test_ledger().await;
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let resource_id = r#"KubeSecret::"namespace/pod/ENV_VAR""#;
        let policy_actor = spawn_policy_actor_permitting("requester", resource_id).await;

        let ciphertext =
            EscrowedSecret::encrypt(b"secret", escrow_keypair.public_key_pem()).unwrap();

        let handler = DecryptOnBehalfOfHandler {
            escrow_keypair,
            ledger,
            policy_actor,
        };

        let reply = handler
            .handle(
                DecryptOnBehalfOfRequest {
                    ciphertext,
                    resource_id: resource_id.to_string(),
                },
                None,
            )
            .await;

        assert!(matches!(
            reply,
            DecryptOnBehalfOfReply::Refused {
                reason: DecryptOnBehalfOfRefusalReason::AnonymousPublisherNotAllowed
            }
        ));
    }

    #[tokio::test]
    async fn missing_requester_certificate_is_refused() {
        let ledger = spawn_test_ledger().await;
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let resource_id = r#"KubeSecret::"namespace/pod/ENV_VAR""#;
        let policy_actor = spawn_policy_actor_permitting("nobody-with-a-cert", resource_id).await;

        let ciphertext =
            EscrowedSecret::encrypt(b"secret", escrow_keypair.public_key_pem()).unwrap();

        let handler = DecryptOnBehalfOfHandler {
            escrow_keypair,
            ledger,
            policy_actor,
        };

        let reply = handler
            .handle(
                DecryptOnBehalfOfRequest {
                    ciphertext,
                    resource_id: resource_id.to_string(),
                },
                Some("nobody-with-a-cert".to_string()),
            )
            .await;

        assert!(matches!(
            reply,
            DecryptOnBehalfOfReply::Refused {
                reason: DecryptOnBehalfOfRefusalReason::RequesterCertificateNotFound
            }
        ));
    }
}
