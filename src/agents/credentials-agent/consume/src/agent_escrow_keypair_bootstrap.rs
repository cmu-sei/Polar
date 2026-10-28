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

//! The agent escrow-keypair bootstrap retrieve flow. Answered to the
//! workload's custodian only, within its window, its certificate
//! naming the escrow key (docs/architecture/key-custody.md, "Part A's
//! bootstrap: the custodian"): never to the workload itself, which
//! holds no token and so never chose the ephemeral it would be sealed
//! to. Mirrors
//! retrieve_secret.rs's own shape and validation discipline closely
//! -- same escrow-private-key decrypt step -- but re-encrypts via the
//! ephemeral X25519 exchange (ephemeral_exchange.rs) instead of a
//! certificate-derived public key, and looks the stored secret up
//! under agent_escrow_keypair_bootstrap's own well-known,
//! identity-derived name rather than a caller-specified one.

use crate::escrow::EscrowKeypair;
use crate::ledger::LedgerMessage;
use credentials_agent_common::agent_escrow_keypair_bootstrap::{
    RetrieveAgentEscrowKeypairRefusalReason, RetrieveAgentEscrowKeypairReply,
    RetrieveAgentEscrowKeypairRequest, agent_escrow_keypair_secret_name,
};
use credentials_agent_common::ephemeral_exchange::EphemeralEncryptedPayload;
use ractor::ActorRef;
use std::sync::Arc;
use tracing::error;

pub struct AgentEscrowKeypairBootstrapHandler {
    pub escrow_keypair: Arc<EscrowKeypair>,
    pub ledger: ActorRef<LedgerMessage>,
}

impl AgentEscrowKeypairBootstrapHandler {
    pub async fn handle(
        &self,
        request: RetrieveAgentEscrowKeypairRequest,
        requested_by: Option<String>,
    ) -> RetrieveAgentEscrowKeypairReply {
        let Some(requested_by) = requested_by else {
            return RetrieveAgentEscrowKeypairReply::Refused {
                reason: RetrieveAgentEscrowKeypairRefusalReason::AnonymousPublisherNotAllowed,
            };
        };

        // Who asks: the workload's custodian. Its ephemeral public key
        // (carried in the request itself) is the only target this flow
        // ever re-encrypts to, and the custody daemon opens it.
        let workload = match crate::custodian::custodian(
            &self.ledger,
            Some(&requested_by),
            cert_issuer_common::custody::CustodyPermission::Escrow,
        )
        .await
        {
            Ok(custodian) => custodian.workload,
            Err(crate::custodian::CustodianRefusal::WindowClosed) => {
                return RetrieveAgentEscrowKeypairReply::Refused {
                    reason: RetrieveAgentEscrowKeypairRefusalReason::CustodianWindowClosed,
                };
            }
            Err(refusal) => {
                tracing::warn!(%requested_by, ?refusal, "an escrow key asked for by someone other than its custodian: refused");
                return RetrieveAgentEscrowKeypairReply::Refused {
                    reason: RetrieveAgentEscrowKeypairRefusalReason::NotACustodian,
                };
            }
        };
        let secret_name = agent_escrow_keypair_secret_name(&workload);
        let stored_secret =
            match crate::retrieve_secret::call_check_secret(&self.ledger, &secret_name).await {
                Ok(Some(stored)) => stored.secret,
                Ok(None) => return RetrieveAgentEscrowKeypairReply::NotFound,
                Err(e) => {
                    error!("Failed to check stored agent escrow keypair for {requested_by}: {e}");
                    return RetrieveAgentEscrowKeypairReply::Failed { detail: e };
                }
            };

        // Same relay logic as RetrieveSecretHandler's own Step 3:
        // decrypt with this agent's own escrow private key. The
        // decrypted plaintext (Zeroizing<Vec<u8>>) is genuinely wiped
        // the instant the ephemeral re-encryption below completes,
        // never left to Rust's ordinary Drop.
        let plaintext = match stored_secret.decrypt(self.escrow_keypair.private_key()) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to decrypt stored agent escrow keypair for {requested_by}: {e}");
                return RetrieveAgentEscrowKeypairReply::Failed {
                    detail: e.to_string(),
                };
            }
        };

        let payload =
            match EphemeralEncryptedPayload::encrypt(&plaintext, &request.ephemeral_public_key) {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to re-encrypt agent escrow keypair for {requested_by}: {e}");
                    return RetrieveAgentEscrowKeypairReply::Failed {
                        detail: e.to_string(),
                    };
                }
            };
        // `plaintext` goes out of scope here, genuinely wiped.

        RetrieveAgentEscrowKeypairReply::Found { payload }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custodian::tests::{CUSTODIAN, X, ledger_with_custodian};
    use cert_issuer_common::custody::CustodyPermission;
    use credentials_agent_common::custodian::CUSTODIAN_WINDOW_SECS;
    use credentials_agent_common::escrow_secret::EscrowedSecret;
    use credentials_agent_common::events::CredentialsEvent;
    use x25519_dalek::{EphemeralSecret, PublicKey};

    #[tokio::test]
    async fn retrieve_genuinely_reencrypts_via_the_ephemeral_exchange() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        let escrow_keypair = crate::escrow::tests::test_keypair();

        // Seed a real, stored "escrow keypair" secret, encrypted to
        // the Credentials Agent's own current escrow key, under the
        // exact well-known name this flow looks up.
        let original_plaintext = b"a real, escrowed private keypair";
        let stored_secret =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::SecretStored {
                    name: agent_escrow_keypair_secret_name(X),
                    secret: stored_secret,
                    stored_by: CUSTODIAN.to_string(),
                    timestamp: 1000,
                    labels: Default::default(),
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair,
            ledger,
        };

        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let reply = handler
            .handle(
                RetrieveAgentEscrowKeypairRequest {
                    ephemeral_public_key: requester_public_key,
                },
                Some(CUSTODIAN.to_string()),
            )
            .await;

        match reply {
            RetrieveAgentEscrowKeypairReply::Found { payload } => {
                // The real proof: this decrypts with the REQUESTER's
                // own ephemeral private key, confirming genuine
                // re-encryption via the ephemeral exchange happened,
                // not merely a pass-through of the original
                // ciphertext.
                let decrypted = payload.decrypt(requester_secret).unwrap();
                assert_eq!(decrypted.as_slice(), original_plaintext);
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    /// The workload itself, a custodian whose certificate does not name the key, and one past its window, all with
    /// a key stored to be had.
    #[tokio::test]
    async fn no_one_but_the_workload_s_custodian_in_its_window_is_given_the_key() {
        for (requester, permissions, age, expected) in [
            (
                X,
                vec![CustodyPermission::Escrow],
                0,
                RetrieveAgentEscrowKeypairRefusalReason::NotACustodian,
            ),
            (
                CUSTODIAN,
                vec![CustodyPermission::Client],
                0,
                RetrieveAgentEscrowKeypairRefusalReason::NotACustodian,
            ),
            (
                CUSTODIAN,
                vec![CustodyPermission::Escrow],
                (CUSTODIAN_WINDOW_SECS + 1) * 1000,
                RetrieveAgentEscrowKeypairRefusalReason::CustodianWindowClosed,
            ),
        ] {
            let ledger = ledger_with_custodian(&permissions, age).await;
            let escrow_keypair = crate::escrow::tests::test_keypair();
            let stored =
                EscrowedSecret::encrypt(b"the key", escrow_keypair.public_key_pem()).unwrap();
            crate::issuance::call_append(
                &ledger,
                CredentialsEvent::SecretStored {
                    name: agent_escrow_keypair_secret_name(X),
                    secret: stored,
                    stored_by: CUSTODIAN.to_string(),
                    timestamp: 1000,
                    labels: Default::default(),
                },
            )
            .await
            .unwrap();
            let handler = AgentEscrowKeypairBootstrapHandler {
                escrow_keypair,
                ledger,
            };
            let reply = handler
                .handle(
                    RetrieveAgentEscrowKeypairRequest {
                        ephemeral_public_key: PublicKey::from(&EphemeralSecret::random())
                            .to_bytes(),
                    },
                    Some(requester.to_string()),
                )
                .await;
            match reply {
                RetrieveAgentEscrowKeypairReply::Refused { reason } => {
                    assert_eq!(reason, expected, "{requester} {permissions:?} {age}")
                }
                other => panic!("{requester} {permissions:?} {age}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn anonymous_publisher_is_refused() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        let handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair: crate::escrow::tests::test_keypair(),
            ledger,
        };

        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let reply = handler
            .handle(
                RetrieveAgentEscrowKeypairRequest {
                    ephemeral_public_key: requester_public_key,
                },
                None,
            )
            .await;

        assert!(matches!(
            reply,
            RetrieveAgentEscrowKeypairReply::Refused {
                reason: RetrieveAgentEscrowKeypairRefusalReason::AnonymousPublisherNotAllowed
            }
        ));
    }

    #[tokio::test]
    async fn no_stored_keypair_yet_returns_not_found() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        let handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair: crate::escrow::tests::test_keypair(),
            ledger,
        };

        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let reply = handler
            .handle(
                RetrieveAgentEscrowKeypairRequest {
                    ephemeral_public_key: requester_public_key,
                },
                Some(CUSTODIAN.to_string()),
            )
            .await;

        assert!(matches!(reply, RetrieveAgentEscrowKeypairReply::NotFound));
    }

    #[tokio::test]
    async fn two_different_identities_do_not_see_each_others_keypairs() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        let escrow_keypair = crate::escrow::tests::test_keypair();

        let stored_secret =
            EscrowedSecret::encrypt(b"alice's own keypair", escrow_keypair.public_key_pem())
                .unwrap();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::SecretStored {
                    name: agent_escrow_keypair_secret_name(
                        "alice.polar.serviceaccount.cluster.local",
                    ),
                    secret: stored_secret,
                    stored_by: "custody.alice.polar.serviceaccount.cluster.local".to_string(),
                    timestamp: 1000,
                    labels: Default::default(),
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair,
            ledger,
        };

        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let reply = handler
            .handle(
                RetrieveAgentEscrowKeypairRequest {
                    ephemeral_public_key: requester_public_key,
                },
                Some(CUSTODIAN.to_string()),
            )
            .await;

        assert!(matches!(reply, RetrieveAgentEscrowKeypairReply::NotFound));
    }
}
