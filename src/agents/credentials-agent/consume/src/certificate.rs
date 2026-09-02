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

//! Unlike EscrowKeyHandler (a synchronous read of an already-loaded,
//! in-memory key), this handler genuinely needs to ask LedgerActor
//! for the current state -- CredentialsState::certificate_for lives
//! inside LedgerActor's own state, not something this handler can
//! hold a direct reference to. Mirrors issuance.rs's own
//! call_check_grant/call_check_renewal pattern: a local, Result-based
//! RPC helper, not ledger.rs's own panic-based check_grant/
//! check_renewal/check_certificate (those are test-only convenience
//! helpers, never used by real handler logic).

use crate::ledger::LedgerMessage;
use crate::state::CertificateRecord;
use credentials_agent_common::certificate::{CertificateReply, CertificateRequest};
use ractor::ActorRef;
use tracing::error;

pub struct CertificateHandler {
    pub ledger: ActorRef<LedgerMessage>,
}

impl CertificateHandler {
    pub async fn handle(&self, request: CertificateRequest) -> CertificateReply {
        match call_check_certificate(&self.ledger, &request.identity).await {
            Ok(Some(record)) => CertificateReply::Found {
                certificate_pem: record.certificate_pem,
            },
            Ok(None) => CertificateReply::NotFound,
            Err(e) => {
                error!("Failed to check certificate for {}: {e}", request.identity);
                CertificateReply::NotFound
            }
        }
    }
}

async fn call_check_certificate(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
) -> Result<Option<CertificateRecord>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{LedgerActor, LedgerActorArgs};
    use credentials_agent_common::events::{CredentialsEvent, GrantReason};

    async fn spawn_test_ledger() -> ActorRef<LedgerMessage> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        // Leak the tempdir so it isn't cleaned up while the actor's
        // still using it -- fine for a short-lived test process.
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
    async fn returns_not_found_when_no_certificate_exists() {
        let ledger = spawn_test_ledger().await;
        let handler = CertificateHandler {
            ledger: ledger.clone(),
        };

        let reply = handler
            .handle(CertificateRequest {
                identity: "nobody".to_string(),
            })
            .await;

        assert!(matches!(reply, CertificateReply::NotFound));
    }

    #[tokio::test]
    async fn returns_the_real_certificate_pem_once_issued() {
        let ledger = spawn_test_ledger().await;
        let expected_pem = "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n";

        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::Grant {
                    target_identity: "alice".to_string(),
                    permissions: vec!["read".to_string()],
                    granted_by: "admin".to_string(),
                    timestamp: 1000,
                    reason: GrantReason::UserOnboarding,
                },
                reply,
            },
            None,
        )
        .await;
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::IssuanceCompleted {
                    identity: "alice".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "serial-1".to_string(),
                    timestamp: 2000,
                    certificate_pem: expected_pem.to_string(),
                },
                reply,
            },
            None,
        )
        .await;

        let handler = CertificateHandler { ledger };

        let reply = handler
            .handle(CertificateRequest {
                identity: "alice".to_string(),
            })
            .await;

        match reply {
            CertificateReply::Found { certificate_pem } => {
                assert_eq!(certificate_pem, expected_pem);
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }
}
