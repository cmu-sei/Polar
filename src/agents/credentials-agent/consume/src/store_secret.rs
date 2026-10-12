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
//! CredentialsEvent::SecretStored to the ledger. Retrieve is
//! deliberately not built yet -- see escrow_secret.rs's own doc
//! comment for why.

use crate::escrow::EscrowKeypair;
use crate::ledger::LedgerMessage;
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

        match call_append(&self.ledger, event).await {
            Ok(()) => StoreSecretReply::Stored,
            Err(e) => {
                tracing::error!("Failed to append SecretStored to the ledger: {e}");
                StoreSecretReply::Failed { detail: e }
            }
        }
    }
}

async fn call_append(
    ledger: &ActorRef<LedgerMessage>,
    event: CredentialsEvent,
) -> Result<(), String> {
    match ractor::rpc::call(ledger, |reply| LedgerMessage::Append { event, reply }, None).await {
        Ok(ractor::rpc::CallResult::Success(result)) => result,
        Ok(other) => Err(format!("unexpected CallResult: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}
