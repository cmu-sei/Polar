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

//! Store half of the general secret-escrow mechanism. Validates that
//! the submitted EscrowedSecret is genuinely encrypted to this
//! agent's own current escrow key, and that the publish carrying it
//! has real, broker-verified attribution -- then appends
//! CredentialsEvent::SecretStored to the ledger. Retrieval is
//! in retrieve_secret.rs.

use crate::escrow::EscrowKeypair;
use crate::ledger::{LedgerMessage, SecretStoreError};
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::secret::{StoreRefusalReason, StoreSecretReply, StoreSecretRequest};
use ractor::ActorRef;
use std::sync::Arc;

pub struct StoreSecretHandler {
    pub escrow_keypair: Arc<EscrowKeypair>,
    pub ledger: ActorRef<LedgerMessage>,
}

impl StoreSecretHandler {
    pub async fn handle(
        &self,
        request: StoreSecretRequest,
        stored_by: Option<String>,
    ) -> StoreSecretReply {
        let Some(stored_by) = stored_by else {
            return StoreSecretReply::Refused {
                reason: StoreRefusalReason::AnonymousPublisherNotAllowed,
            };
        };

        if request.secret.encrypted_to_pem() != self.escrow_keypair.public_key_pem() {
            return StoreSecretReply::Refused {
                reason: StoreRefusalReason::WrongEscrowKey,
            };
        }

        let event = CredentialsEvent::SecretStored {
            name: request.name,
            secret: request.secret,
            stored_by,
            timestamp: polar::time::now_millis(),
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
                StoreSecretReply::Refused {
                    reason: StoreRefusalReason::NameTaken,
                }
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
    use crate::retrieve_secret::tests::spawn_test_ledger;
    use credentials_agent_common::escrow_secret::EscrowedSecret;

    struct World {
        handler: StoreSecretHandler,
        ledger: ActorRef<LedgerMessage>,
        keypair: Arc<EscrowKeypair>,
    }

    async fn world() -> World {
        let ledger = spawn_test_ledger().await;
        let keypair = crate::escrow::tests::test_keypair();
        World {
            handler: StoreSecretHandler {
                escrow_keypair: keypair.clone(),
                ledger: ledger.clone(),
            },
            ledger,
            keypair,
        }
    }

    fn request(w: &World, name: &str, plaintext: &[u8]) -> StoreSecretRequest {
        StoreSecretRequest {
            name: name.to_string(),
            secret: EscrowedSecret::encrypt(plaintext, w.keypair.public_key_pem()).unwrap(),
            labels: Default::default(),
        }
    }

    async fn held(w: &World, name: &str) -> Option<crate::state::StoredSecret> {
        ractor::call!(w.ledger, |reply| LedgerMessage::CheckSecret {
            name: name.to_string(),
            reply,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn the_identity_that_stored_a_secret_may_replace_it() {
        let w = world().await;
        let first = w
            .handler
            .handle(request(&w, "github-sei", b"one"), Some("observer".into()))
            .await;
        assert_eq!(first, StoreSecretReply::Stored);
        let before = held(&w, "github-sei").await.unwrap();
        let again = w
            .handler
            .handle(request(&w, "github-sei", b"two"), Some("observer".into()))
            .await;
        assert_eq!(again, StoreSecretReply::Stored);
        let after = held(&w, "github-sei").await.unwrap();
        assert_eq!(after.stored_by, "observer");
        assert_ne!(
            after.secret, before.secret,
            "the new value replaced the old"
        );
    }

    #[tokio::test]
    async fn another_identity_cannot_take_a_secret_over_and_the_owner_keeps_it() {
        let w = world().await;
        w.handler
            .handle(request(&w, "github-sei", b"mine"), Some("observer".into()))
            .await;
        let before = held(&w, "github-sei").await.unwrap();

        let taken = w
            .handler
            .handle(request(&w, "github-sei", b"yours"), Some("intruder".into()))
            .await;
        assert_eq!(
            taken,
            StoreSecretReply::Refused {
                reason: StoreRefusalReason::NameTaken
            }
        );
        let after = held(&w, "github-sei").await.unwrap();
        assert_eq!(after, before, "neither the value nor the owner changed");
    }

    #[tokio::test]
    async fn a_name_nobody_holds_is_free_to_any_identity_and_then_theirs() {
        let w = world().await;
        let a = w
            .handler
            .handle(request(&w, "a", b"x"), Some("alice".into()))
            .await;
        let b = w
            .handler
            .handle(request(&w, "b", b"x"), Some("bob".into()))
            .await;
        assert_eq!((a, b), (StoreSecretReply::Stored, StoreSecretReply::Stored));
        assert_eq!(held(&w, "a").await.unwrap().stored_by, "alice");
        assert_eq!(held(&w, "b").await.unwrap().stored_by, "bob");
    }

    #[tokio::test]
    async fn two_identities_racing_for_a_fresh_name_cannot_both_have_it() {
        for _ in 0..20 {
            let w = world().await;
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

    #[tokio::test]
    async fn an_anonymous_store_and_one_to_the_wrong_key_are_still_refused_first() {
        let w = world().await;
        let anonymous = w.handler.handle(request(&w, "s", b"x"), None).await;
        assert_eq!(
            anonymous,
            StoreSecretReply::Refused {
                reason: StoreRefusalReason::AnonymousPublisherNotAllowed
            }
        );
        let other_key = crate::escrow::EscrowKeypair::generate().unwrap();
        let wrong = StoreSecretRequest {
            name: "s".to_string(),
            secret: EscrowedSecret::encrypt(b"x", other_key.public_key_pem()).unwrap(),
            labels: Default::default(),
        };
        assert_eq!(
            w.handler.handle(wrong, Some("alice".into())).await,
            StoreSecretReply::Refused {
                reason: StoreRefusalReason::WrongEscrowKey
            }
        );
        assert!(held(&w, "s").await.is_none());
    }
}
