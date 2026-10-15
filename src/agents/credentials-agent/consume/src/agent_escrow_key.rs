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

//! The agent escrow-key query -- mirrors certificate.rs's own shape
//! exactly (a genuine ledger round trip, not a synchronous read of
//! an already-loaded key the way EscrowKeyHandler is), with one real
//! addition: the stored AgentEscrowCertificateRecord only carries a
//! certificate PEM, not a standalone public-key PEM, so this handler
//! extracts the key from the certificate itself via
//! cert_issuer_common::public_key::extract_public_key_pem.

use crate::ledger::LedgerMessage;
use credentials_agent_common::agent_escrow_key::{AgentEscrowKeyReply, AgentEscrowKeyRequest};
use ractor::ActorRef;
use tracing::error;

pub struct AgentEscrowKeyHandler {
    pub ledger: ActorRef<LedgerMessage>,
}

impl AgentEscrowKeyHandler {
    pub async fn handle(&self, request: AgentEscrowKeyRequest) -> AgentEscrowKeyReply {
        let record = match call_check_agent_escrow_key(&self.ledger, &request.identity).await {
            Ok(r) => r,
            Err(e) => {
                error!(
                    "Failed to check agent escrow certificate for {}: {e}",
                    request.identity
                );
                return AgentEscrowKeyReply::NotFound;
            }
        };

        let Some(record) = record else {
            return AgentEscrowKeyReply::NotFound;
        };

        match cert_issuer_common::public_key::extract_public_key_pem(&record.certificate_pem) {
            Ok(public_key_pem) => AgentEscrowKeyReply::Found {
                public_key_pem,
                certificate_pem: record.certificate_pem,
            },
            Err(e) => {
                // The certificate itself was successfully retrieved --
                // this means the certificate PEM this agent stored is
                // somehow malformed, a genuine data-integrity concern
                // worth loud operator attention, not a normal "just
                // doesn't exist yet" case.
                error!(
                    "Stored agent escrow certificate for {} failed public-key extraction \
                     -- possible data corruption: {e}",
                    request.identity
                );
                AgentEscrowKeyReply::NotFound
            }
        }
    }
}

pub(crate) async fn call_check_agent_escrow_key(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
) -> Result<Option<crate::state::AgentEscrowCertificateRecord>, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckAgentEscrowKey {
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

    fn real_self_signed_cert_pem() -> String {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let private_key_pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params =
            rcgen::CertificateParams::new(vec!["git-observer.polar.svc.cluster.local".to_string()])
                .unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        cert.pem()
    }

    #[tokio::test]
    async fn returns_not_found_when_no_agent_escrow_certificate_exists() {
        let handler = AgentEscrowKeyHandler {
            ledger: spawn_test_ledger().await,
        };

        let reply = handler
            .handle(AgentEscrowKeyRequest {
                identity: "nobody".to_string(),
            })
            .await;

        assert!(matches!(reply, AgentEscrowKeyReply::NotFound));
    }

    #[tokio::test]
    async fn returns_the_real_public_key_extracted_from_the_stored_certificate() {
        let ledger = spawn_test_ledger().await;
        let certificate_pem = real_self_signed_cert_pem();
        let expected_public_key_pem =
            cert_issuer_common::public_key::extract_public_key_pem(&certificate_pem).unwrap();

        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::AgentEscrowIssuanceCompleted {
                    identity: "git-observer".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "service-serial-1".to_string(),
                    timestamp: 2000,
                    certificate_pem: certificate_pem.clone(),
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowKeyHandler { ledger };

        let reply = handler
            .handle(AgentEscrowKeyRequest {
                identity: "git-observer".to_string(),
            })
            .await;

        match reply {
            AgentEscrowKeyReply::Found {
                public_key_pem,
                certificate_pem: replied_certificate,
            } => {
                assert_eq!(public_key_pem, expected_public_key_pem);
                assert_eq!(
                    replied_certificate.trim(),
                    certificate_pem.trim(),
                    "the certificate returned is the one stored"
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }
}
