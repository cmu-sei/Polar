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

//! The consumer-side gate for signed admin actions.
//!
//! Authorship is proven by the signature, never by transport: the author
//! certificate is looked up by serial in the ledger, checked for revocation,
//! and used to verify the signature over the payload bytes. The capability the
//! action needs is then checked against the ledger record for that author
//! (the ledger is authoritative; the SAN copy in the cert is for outside
//! verifiers). The consumer mints the account UUID and stamps the timestamp.

use crate::ledger::LedgerMessage;
use credentials_agent_common::admin_action::{
    AdminAction, AdminActionRefusal, AdminActionReply, SignedAdminAction,
};
use credentials_agent_common::admin_signing;
use credentials_agent_common::events::CredentialsEvent;
use ractor::{ActorRef, RpcReplyPort};
use tracing::{error, info, warn};

pub struct AdminActionHandler {
    pub ledger: ActorRef<LedgerMessage>,
}

impl AdminActionHandler {
    pub async fn handle(&self, signed: SignedAdminAction) -> AdminActionReply {
        match self.handle_inner(signed).await {
            Ok(reply) => reply,
            Err(detail) => {
                error!("admin action failed: {detail}");
                AdminActionReply::Failed { detail }
            }
        }
    }

    async fn handle_inner(&self, signed: SignedAdminAction) -> Result<AdminActionReply, String> {
        // 1. Who claims to have signed this?
        let serial = signed.author_serial.clone();
        let author = self
            .ask(|reply| LedgerMessage::CheckAccountCertificateBySerial { serial, reply })
            .await?;
        let Some(author) = author else {
            return Ok(refuse(AdminActionRefusal::UnknownAuthor));
        };
        if author.revoked {
            return Ok(refuse(AdminActionRefusal::AuthorRevoked));
        }

        // 2. Did they actually sign these exact bytes?
        if let Err(e) = admin_signing::verify(&signed, &author.certificate_pem) {
            warn!(
                "admin action from serial {} failed verification: {e}",
                signed.author_serial
            );
            return Ok(refuse(AdminActionRefusal::BadSignature));
        }

        // 3. Only now is it safe to look inside.
        let body = match signed.decode_body() {
            Ok(body) => body,
            Err(_) => return Ok(refuse(AdminActionRefusal::MalformedPayload)),
        };

        // 4. Is the author allowed to do this? Ledger record decides.
        let required = body.action.required_capability();
        if !author.capabilities.contains(&required) {
            return Ok(refuse(AdminActionRefusal::MissingCapability { required }));
        }

        // 4b. The bootstrap certificate exists to create one real admin,
        // then it is spent. Checked here, in the single actor that also
        // records the action, so two concurrent requests can't both pass.
        if author.via_bootstrap
            && matches!(body.action, AdminAction::ProvisionAccount { .. })
            && author.provisionings_made >= 1
        {
            return Ok(refuse(AdminActionRefusal::BootstrapCertificateSpent));
        }

        // 5. Action-specific checks; provisioning mints the account UUID.
        let account_id = match &body.action {
            AdminAction::PolicyUpdate { .. } => {
                return Ok(refuse(AdminActionRefusal::NotYetSupported));
            }
            AdminAction::ProvisionAccount { username, .. } => {
                if is_reserved_username(username) {
                    return Ok(refuse(AdminActionRefusal::ReservedUsername));
                }
                match self.account_id_for_provisioning(username).await? {
                    Ok(id) => Some(id),
                    Err(refusal) => return Ok(refuse(refusal)),
                }
            }
            AdminAction::Grant { .. } | AdminAction::Revoke { .. } => None,
        };

        // 6. One atomic, replay-refusing append.
        let event = CredentialsEvent::AdminActionRecorded {
            payload_hash: signed.payload_hash(),
            signed,
            author_account_id: author.account_id.clone(),
            timestamp: polar::time::now_millis(),
            account_id,
        };
        match self
            .ask(|reply| LedgerMessage::RecordAdminAction { event, reply })
            .await?
        {
            Ok(true) => {
                info!("admin action recorded (author {})", author.account_id);
                Ok(AdminActionReply::Recorded)
            }
            Ok(false) => Ok(refuse(AdminActionRefusal::Replay)),
            Err(detail) => Ok(AdminActionReply::Failed { detail }),
        }
    }

    /// A live account or a pending provisioning blocks a new one. A revoked
    /// account is re-provisioned under its existing account_id, so identity
    /// survives the loss of a certificate.
    async fn account_id_for_provisioning(
        &self,
        username: &str,
    ) -> Result<Result<String, AdminActionRefusal>, String> {
        let name = username.to_string();
        let existing = self
            .ask(|reply| LedgerMessage::CheckAccountForUsername {
                username: name,
                reply,
            })
            .await?;
        if let Some(record) = existing {
            return Ok(if record.revoked {
                Ok(record.account_id)
            } else {
                Err(AdminActionRefusal::AccountAlreadyExists)
            });
        }

        let name = username.to_string();
        let pending = self
            .ask(|reply| LedgerMessage::CheckProvisionedAccount {
                username: name,
                reply,
            })
            .await?;
        if pending.is_some() {
            return Ok(Err(AdminActionRefusal::AccountAlreadyPending));
        }

        Ok(Ok(uuid::Uuid::new_v4().to_string()))
    }

    async fn ask<T: Send + 'static>(
        &self,
        build: impl FnOnce(RpcReplyPort<T>) -> LedgerMessage,
    ) -> Result<T, String> {
        crate::ledger_call::ask(&self.ledger, build).await
    }
}

fn refuse(reason: AdminActionRefusal) -> AdminActionReply {
    warn!("admin action refused: {reason:?}");
    AdminActionReply::Refused { reason }
}

/// Usernames an admin may never provision. The bootstrap account's name is
/// fixed by the claim flow; letting an admin pre-provision it would let
/// them pre-empt or shadow the bootstrap identity. Compared case- and
/// whitespace-insensitively so `Polar-Bootstrap ` can't slip past.
fn is_reserved_username(username: &str) -> bool {
    username
        .trim()
        .eq_ignore_ascii_case(crate::account::BOOTSTRAP_USERNAME)
}

#[cfg(test)]
mod reserved_tests {
    use super::*;

    #[test]
    fn the_bootstrap_username_is_reserved_in_any_spelling() {
        assert!(is_reserved_username("polar-bootstrap"));
        assert!(is_reserved_username("Polar-Bootstrap"));
        assert!(is_reserved_username("  polar-bootstrap "));
    }

    #[test]
    fn ordinary_usernames_are_not_reserved() {
        assert!(!is_reserved_username("alice"));
        assert!(!is_reserved_username("polar-bootstrap2"));
    }
}
