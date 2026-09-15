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

use crate::escrow::EscrowKeypair;
use crate::ledger::LedgerMessage;
use credentials_agent_common::escrow_secret::EscrowedSecret;
use credentials_agent_common::retrieve_secret::{
    RetrieveRefusalReason, RetrieveSecretReply, RetrieveSecretRequest,
};
use ractor::ActorRef;
use std::sync::Arc;
use tracing::error;

pub struct RetrieveSecretHandler {
    pub escrow_keypair: Arc<EscrowKeypair>,
    pub ledger: ActorRef<LedgerMessage>,
}

impl RetrieveSecretHandler {
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

        // Step 1: find the target to re-encrypt to -- always the
        // requester's own, broker-verified identity's current mTLS
        // certificate, never anything the request itself specifies.
        let requester_certificate = match call_check_certificate(&self.ledger, &requested_by).await {
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
        let target_public_key_pem = match cert_issuer_common::public_key::extract_public_key_pem(
            &requester_certificate.certificate_pem,
        ) {
            Ok(pem) => pem,
            Err(e) => {
                error!(
                    "Requester certificate for {requested_by} failed public-key extraction: {e}"
                );
                return RetrieveSecretReply::Failed {
                    detail: e.to_string(),
                };
            }
        };

        // Step 2: find the stored secret itself.
        let stored_secret = match call_check_secret(&self.ledger, &request.name).await {
            Ok(Some(secret)) => secret,
            Ok(None) => return RetrieveSecretReply::NotFound,
            Err(e) => {
                error!("Failed to check stored secret {}: {e}", request.name);
                return RetrieveSecretReply::Failed { detail: e };
            }
        };

        // Step 3: the actual relay -- decrypt with this agent's own
        // escrow private key, immediately re-encrypt with the
        // requester's own public key. The decrypted plaintext is
        // Zeroizing (EscrowedSecret::decrypt's own return type) --
        // explicitly wiped the instant re-encryption completes below,
        // never left to Rust's ordinary Drop, per this document's own
        // hard requirement.
        let plaintext = match stored_secret.decrypt(self.escrow_keypair.private_key()) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to decrypt stored secret {}: {e}", request.name);
                return RetrieveSecretReply::Failed {
                    detail: e.to_string(),
                };
            }
        };

        let reencrypted = match EscrowedSecret::encrypt(&plaintext, &target_public_key_pem) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to re-encrypt secret {} for {requested_by}: {e}", request.name);
                return RetrieveSecretReply::Failed {
                    detail: e.to_string(),
                };
            }
        };
        // `plaintext` (Zeroizing<Vec<u8>>) goes out of scope here,
        // genuinely wiped rather than merely deallocated.

        RetrieveSecretReply::Found { secret: reencrypted }
    }
}

async fn call_check_certificate(
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

async fn call_check_secret(
    ledger: &ActorRef<LedgerMessage>,
    name: &str,
) -> Result<Option<EscrowedSecret>, String> {
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

    #[tokio::test]
    async fn retrieve_secret_genuinely_reencrypts_to_the_requesters_own_key() {
        let ledger = spawn_test_ledger().await;
        let escrow_keypair = crate::escrow::tests::test_keypair();

        // Seed a real certificate for the requester.
        let mut rng = rand::rng();
        let requester_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let requester_private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &requester_private_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let requester_key_pair = rcgen::KeyPair::from_pem(&requester_private_key_pem).unwrap();
        let requester_params = rcgen::CertificateParams::new(vec![
            "requester.polar.svc.cluster.local".to_string(),
        ])
        .unwrap();
        let requester_certificate_pem = requester_params.self_signed(&requester_key_pair).unwrap().pem();

        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::IssuanceCompleted {
                    identity: "requester".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "requester-serial-1".to_string(),
                    timestamp: 1000,
                    certificate_pem: requester_certificate_pem,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        // Seed a real, stored secret -- encrypted to the Credentials
        // Agent's own current escrow key, matching how
        // StoreSecretHandler actually validates on the way in.
        let original_plaintext = b"a real secret from a real agent";
        let stored_secret =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::SecretStored {
                    name: "gitlab-token".to_string(),
                    secret: stored_secret,
                    stored_by: "some-storing-agent".to_string(),
                    timestamp: 1000,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = RetrieveSecretHandler {
            escrow_keypair,
            ledger,
        };

        let reply = handler
            .handle(
                RetrieveSecretRequest {
                    name: "gitlab-token".to_string(),
                },
                Some("requester".to_string()),
            )
            .await;

        match reply {
            RetrieveSecretReply::Found { secret } => {
                // The real proof: this decrypts with the REQUESTER's
                // own private key, not the escrow key -- confirming
                // genuine re-encryption happened, not merely a pass-
                // through of the original ciphertext.
                let decrypted = secret.decrypt(&requester_private_key).unwrap();
                assert_eq!(&*decrypted, original_plaintext);
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn anonymous_publisher_is_refused() {
        let ledger = spawn_test_ledger().await;
        let handler = RetrieveSecretHandler {
            escrow_keypair: crate::escrow::tests::test_keypair(),
            ledger,
        };

        let reply = handler
            .handle(
                RetrieveSecretRequest {
                    name: "gitlab-token".to_string(),
                },
                None,
            )
            .await;

        assert!(matches!(
            reply,
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::AnonymousPublisherNotAllowed
            }
        ));
    }

    #[tokio::test]
    async fn missing_requester_certificate_is_refused() {
        let ledger = spawn_test_ledger().await;
        let handler = RetrieveSecretHandler {
            escrow_keypair: crate::escrow::tests::test_keypair(),
            ledger,
        };

        let reply = handler
            .handle(
                RetrieveSecretRequest {
                    name: "gitlab-token".to_string(),
                },
                Some("nobody-with-a-cert".to_string()),
            )
            .await;

        assert!(matches!(
            reply,
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::RequesterCertificateNotFound
            }
        ));
    }

    #[tokio::test]
    async fn nonexistent_secret_returns_not_found() {
        let ledger = spawn_test_ledger().await;

        // Requester still needs a real certificate -- this test is
        // specifically about the secret not existing, not about
        // requester validation, so that earlier check must pass
        // first.
        let mut rng = rand::rng();
        let requester_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let requester_private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &requester_private_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let requester_key_pair = rcgen::KeyPair::from_pem(&requester_private_key_pem).unwrap();
        let requester_params = rcgen::CertificateParams::new(vec![
            "requester.polar.svc.cluster.local".to_string(),
        ])
        .unwrap();
        let requester_certificate_pem = requester_params.self_signed(&requester_key_pair).unwrap().pem();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::IssuanceCompleted {
                    identity: "requester".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "requester-serial-1".to_string(),
                    timestamp: 1000,
                    certificate_pem: requester_certificate_pem,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = RetrieveSecretHandler {
            escrow_keypair: crate::escrow::tests::test_keypair(),
            ledger,
        };

        let reply = handler
            .handle(
                RetrieveSecretRequest {
                    name: "never-stored".to_string(),
                },
                Some("requester".to_string()),
            )
            .await;

        assert!(matches!(reply, RetrieveSecretReply::NotFound));
    }
}
