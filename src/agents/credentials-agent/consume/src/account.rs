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

//! The account claim, both phases, for every kind of claimant.
//!
//! Begin proves who the claimant is and tells them the account_id their CSR
//! must be built around. Complete takes that CSR, proves who they are again,
//! signs it through cert-issuer's /internal/issue, and records the result.
//! See `credentials_agent_common::account_claim` for why it is two phases.
//!
//! Bootstrap is the same flow with a Kubernetes token as the credential. Its
//! one difference is that the pending account doesn't exist until Begin
//! creates it, and bootstrap only counts as done once its certificate has
//! been issued, so a claim that fails part-way can simply be repeated.

use crate::ledger::LedgerMessage;
use crate::ledger_call::ask;
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::capability::Capability;
use cert_issuer_common::csr_san::csr_dns_sans;
use cert_issuer_common::{
    AccountSubject, CertType, InternalIssueRequest, SubjectDnDefaults, ValidateRequest,
};
use credentials_agent_common::account_claim::{
    AccountRefusal, AccountReply, AccountRequest, Claimant,
};
use credentials_agent_common::events::{CredentialsEvent, GrantReason};
use ractor::ActorRef;
use tracing::{error, info, warn};

/// The fixed identity of the one-time bootstrap account. The tool supplies
/// nothing: the account's whole job is to provision the first real admin.
pub const BOOTSTRAP_USERNAME: &str = "polar-bootstrap";
pub const BOOTSTRAP_EMAIL: &str = "bootstrap@polar.invalid";

/// The only Kubernetes identity allowed to bootstrap: the raw `sub` claim
/// of the `polar-admin-bootstrap` ServiceAccount's token.
pub const DEFAULT_BOOTSTRAP_IDENTITY: &str =
    "system:serviceaccount:polar-bootstrap:polar-admin-bootstrap";

pub struct AccountHandler {
    pub ledger: ActorRef<LedgerMessage>,
    /// Presents the agent's own client certificate: /internal/issue needs it.
    pub cert_issuer: HandshakeClient,
    pub bootstrap_identity: String,
}

/// Why a request stopped. Ledger failures are the agent's problem, not the
/// claimant's, and never reveal why to them.
enum Stop {
    Refuse(AccountRefusal),
    Ledger(String),
}

impl From<AccountRefusal> for Stop {
    fn from(reason: AccountRefusal) -> Self {
        Stop::Refuse(reason)
    }
}

impl From<String> for Stop {
    fn from(detail: String) -> Self {
        Stop::Ledger(detail)
    }
}

/// A provisioned account waiting to be claimed.
struct Pending {
    username: String,
    record: crate::state::ProvisionedAccountRecord,
}

impl AccountHandler {
    pub async fn handle(&self, request: AccountRequest) -> AccountReply {
        let outcome = match request {
            AccountRequest::Begin { claimant } => self
                .begin(&claimant)
                .await
                .map(|account_id| AccountReply::Offered { account_id }),
            AccountRequest::Complete { claimant, csr_pem } => {
                self.complete(&claimant, csr_pem).await
            }
        };
        match outcome {
            Ok(reply) => reply,
            Err(Stop::Refuse(reason)) => {
                warn!("account claim refused: {reason:?}");
                AccountReply::Refused { reason }
            }
            Err(Stop::Ledger(detail)) => {
                error!("account claim failed on the ledger: {detail}");
                AccountReply::Refused {
                    reason: AccountRefusal::Unavailable,
                }
            }
        }
    }

    async fn begin(&self, claimant: &Claimant) -> Result<String, Stop> {
        Ok(self.pending_for(claimant, true).await?.record.account_id)
    }

    async fn complete(&self, claimant: &Claimant, csr_pem: String) -> Result<AccountReply, Stop> {
        let pending = self.pending_for(claimant, false).await?;
        let account_id = pending.record.account_id.clone();

        // The CSR must be built around exactly the account_id offered: one
        // SAN, no more. cert-issuer then checks it against the identity we
        // assert, byte for byte.
        let sans = csr_dns_sans(&csr_pem).map_err(|_| AccountRefusal::CsrInvalid)?;
        if sans.as_slice() != std::slice::from_ref(&account_id) {
            return Err(AccountRefusal::CsrInvalid.into());
        }

        let extra_sans: Vec<String> = pending
            .record
            .permissions
            .iter()
            .map(|p| p.to_san_string())
            .chain(
                pending
                    .record
                    .capabilities
                    .iter()
                    .map(|c| c.to_san_string()),
            )
            .collect();

        let issued = self
            .cert_issuer
            .issue_internal(InternalIssueRequest {
                csr_pem,
                identity: account_id.clone(),
                cert_type: CertType::Client,
                extra_sans,
                subject_dn_defaults: Some(SubjectDnDefaults {
                    given_name: pending.record.given_name.clone(),
                    surname: pending.record.surname.clone(),
                    title: pending.record.title.clone(),
                    telephone_number: pending.record.telephone_number.clone(),
                }),
                account_subject: Some(AccountSubject {
                    username: pending.username.clone(),
                    email: pending.record.email.clone(),
                }),
            })
            .await
            .map_err(|e| {
                error!("cert-issuer /internal/issue failed for account {account_id}: {e}");
                AccountRefusal::SigningFailed
            })?;

        // The certificate exists, but one the ledger doesn't know can't author
        // anything, so don't hand it over: refuse, leave the pending record,
        // and let the claimant try again.
        self.append(CredentialsEvent::AccountCertificateIssued {
            account_id: account_id.clone(),
            username: pending.username.clone(),
            serial: issued.serial_number.clone(),
            timestamp: polar::time::now_millis(),
            certificate_pem: issued.certificate_pem.clone(),
        })
        .await
        .inspect_err(|_e| {
            error!(
                "account {account_id}: certificate {} was issued but not recorded",
                issued.serial_number
            );
        })?;

        info!(
            "account {account_id} claimed; certificate {}",
            issued.serial_number
        );
        Ok(AccountReply::Issued {
            certificate_pem: issued.certificate_pem,
            ca_chain_pem: issued.ca_chain_pem,
        })
    }

    /// The pending account this claimant is entitled to. With `create`, the
    /// bootstrap path provisions its account when none is pending (Begin);
    /// without it, a missing account is simply a refusal (Complete).
    async fn pending_for(&self, claimant: &Claimant, create: bool) -> Result<Pending, Stop> {
        match claimant {
            Claimant::Native { username, email } => {
                let record = self.provisioned(username).await?;
                match record {
                    // A native claim can never reach the bootstrap record, even
                    // by matching its fixed username and email.
                    Some(record) if !record.via_bootstrap && emails_match(&record.email, email) => {
                        Ok(Pending {
                            username: username.clone(),
                            record,
                        })
                    }
                    _ => Err(AccountRefusal::NoMatchingProvisioning.into()),
                }
            }
            Claimant::Bootstrap { credential } => {
                self.check_bootstrap_token(credential).await?;

                if let Some(record) = self.provisioned(BOOTSTRAP_USERNAME).await? {
                    return if record.via_bootstrap {
                        Ok(Pending {
                            username: BOOTSTRAP_USERNAME.to_string(),
                            record,
                        })
                    } else {
                        Err(AccountRefusal::NoMatchingProvisioning.into())
                    };
                }
                if !create {
                    return Err(AccountRefusal::NoMatchingProvisioning.into());
                }
                if ask(&self.ledger, |reply| {
                    LedgerMessage::CheckBootstrapCompleted { reply }
                })
                .await?
                {
                    return Err(AccountRefusal::BootstrapAlreadyDone.into());
                }

                self.append(CredentialsEvent::AccountProvisioned {
                    account_id: uuid::Uuid::new_v4().to_string(),
                    username: BOOTSTRAP_USERNAME.to_string(),
                    email: BOOTSTRAP_EMAIL.to_string(),
                    permissions: vec![],
                    capabilities: vec![Capability::ProvisionAccount],
                    provisioned_by: BOOTSTRAP_USERNAME.to_string(),
                    timestamp: polar::time::now_millis(),
                    reason: GrantReason::UserOnboarding,
                    via_bootstrap: true,
                    given_name: None,
                    surname: None,
                    title: None,
                    telephone_number: None,
                })
                .await?;

                match self.provisioned(BOOTSTRAP_USERNAME).await? {
                    Some(record) => Ok(Pending {
                        username: BOOTSTRAP_USERNAME.to_string(),
                        record,
                    }),
                    None => Err(Stop::Ledger(
                        "bootstrap account was appended but is not in the ledger state".to_string(),
                    )),
                }
            }
        }
    }

    /// The token must validate with cert-issuer and be the bootstrap
    /// service account's, nobody else's.
    async fn check_bootstrap_token(&self, credential: &str) -> Result<(), Stop> {
        let validated = self
            .cert_issuer
            .validate(credential, ValidateRequest::default())
            .await
            .map_err(|e| {
                warn!("bootstrap token did not validate: {e}");
                AccountRefusal::TokenInvalid
            })?;
        if validated.workload_identity != self.bootstrap_identity {
            warn!(
                "valid token for '{}' presented for bootstrap",
                validated.workload_identity
            );
            return Err(AccountRefusal::NotTheBootstrapIdentity.into());
        }
        Ok(())
    }

    async fn provisioned(
        &self,
        username: &str,
    ) -> Result<Option<crate::state::ProvisionedAccountRecord>, String> {
        let username = username.to_string();
        ask(&self.ledger, |reply| {
            LedgerMessage::CheckProvisionedAccount { username, reply }
        })
        .await
    }

    async fn append(&self, event: CredentialsEvent) -> Result<(), Stop> {
        ask(&self.ledger, |reply| LedgerMessage::Append { event, reply })
            .await?
            .map_err(Stop::Ledger)
    }
}

/// Admin-authoritative, compared without regard to case or surrounding
/// whitespace: nobody should be locked out of their account by how they
/// capitalised their own email.
fn emails_match(provisioned: &str, claimed: &str) -> bool {
    provisioned.trim().eq_ignore_ascii_case(claimed.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
    use cert_issuer_common::permission::Permission;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ALICE_ID: &str = "acct-alice";

    async fn spawn_test_ledger() -> ActorRef<LedgerMessage> {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        // Leak the tempdir so it isn't cleaned up while the actor is still
        // using it -- fine for a short-lived test process.
        std::mem::forget(dir);
        let (ledger, _) = ractor::Actor::spawn(
            None,
            crate::ledger::LedgerActor,
            crate::ledger::LedgerActorArgs {
                log_path: log.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();
        ledger
    }

    fn fake_issue_response() -> cert_issuer_common::IssueResponse {
        cert_issuer_common::IssueResponse {
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                .to_string(),
            ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                .to_string(),
            serial_number: "01:23:45:67".to_string(),
            session_id: "00000000-0000-0000-0000-000000000001".to_string(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        }
    }

    async fn handler_for(server: &MockServer) -> AccountHandler {
        AccountHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new(server.uri()),
            bootstrap_identity: DEFAULT_BOOTSTRAP_IDENTITY.to_string(),
        }
    }

    /// cert-issuer's /validate says the presented token is `identity`'s.
    async fn mock_validate(server: &MockServer, identity: &str) {
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: identity.to_string(),
                    instance_binding: None,
                    jti: None,
                    normalized_identity: None,
                },
            ))
            .mount(server)
            .await;
    }

    async fn mock_internal_issue_ok(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(server)
            .await;
    }

    async fn provision(
        ledger: &ActorRef<LedgerMessage>,
        account_id: &str,
        username: &str,
        email: &str,
        via_bootstrap: bool,
    ) {
        let event = CredentialsEvent::AccountProvisioned {
            account_id: account_id.to_string(),
            username: username.to_string(),
            email: email.to_string(),
            permissions: vec![Permission::from_san_string("p:u2s").unwrap()],
            capabilities: vec![],
            provisioned_by: "admin".to_string(),
            timestamp: 1,
            reason: GrantReason::UserOnboarding,
            via_bootstrap,
            given_name: None,
            surname: None,
            title: None,
            telephone_number: None,
        };
        ask(ledger, |reply| LedgerMessage::Append { event, reply })
            .await
            .unwrap()
            .unwrap();
    }

    fn csr_for(account_id: &str) -> String {
        generate_csr(account_id, &KeyAlgorithm::Ed25519, &[])
            .unwrap()
            .csr_pem
    }

    fn alice() -> Claimant {
        Claimant::Native {
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
        }
    }

    fn bootstrap() -> Claimant {
        Claimant::Bootstrap {
            credential: "a-service-account-token".to_string(),
        }
    }

    fn offered(reply: AccountReply) -> String {
        match reply {
            AccountReply::Offered { account_id } => account_id,
            other => panic!("expected Offered, got {other:?}"),
        }
    }

    fn refused(reply: AccountReply) -> AccountRefusal {
        match reply {
            AccountReply::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    async fn bootstrap_completed(handler: &AccountHandler) -> bool {
        ask(&handler.ledger, |reply| {
            LedgerMessage::CheckBootstrapCompleted { reply }
        })
        .await
        .unwrap()
    }

    #[test]
    fn emails_match_ignoring_case_and_padding() {
        assert!(emails_match("Alice@Example.com", "  alice@example.com "));
    }

    #[test]
    fn different_emails_do_not_match() {
        assert!(!emails_match("alice@example.com", "alicia@example.com"));
        assert!(!emails_match("alice@example.com", ""));
    }

    #[tokio::test]
    async fn a_native_claim_runs_both_phases_and_can_only_succeed_once() {
        let server = MockServer::start().await;
        mock_internal_issue_ok(&server).await;
        let handler = handler_for(&server).await;
        provision(
            &handler.ledger,
            ALICE_ID,
            "alice",
            "alice@example.com",
            false,
        )
        .await;

        let account_id = offered(
            handler
                .handle(AccountRequest::Begin { claimant: alice() })
                .await,
        );
        assert_eq!(account_id, ALICE_ID);

        let reply = handler
            .handle(AccountRequest::Complete {
                claimant: alice(),
                csr_pem: csr_for(&account_id),
            })
            .await;
        assert!(
            matches!(reply, AccountReply::Issued { .. }),
            "got {reply:?}"
        );

        // The certificate is on the ledger, with what the account was
        // provisioned with, and no longer claimable.
        let record = ask(&handler.ledger, |reply| {
            LedgerMessage::CheckAccountCertificateBySerial {
                serial: "01:23:45:67".to_string(),
                reply,
            }
        })
        .await
        .unwrap()
        .expect("the issued certificate is recorded");
        assert_eq!(record.account_id, ALICE_ID);
        assert_eq!(record.username, "alice");
        assert_eq!(record.permissions.len(), 1);
        assert!(!record.via_bootstrap);

        let again = handler
            .handle(AccountRequest::Begin { claimant: alice() })
            .await;
        assert_eq!(refused(again), AccountRefusal::NoMatchingProvisioning);
    }

    #[tokio::test]
    async fn the_right_username_with_the_wrong_email_is_refused() {
        let server = MockServer::start().await;
        let handler = handler_for(&server).await;
        provision(
            &handler.ledger,
            ALICE_ID,
            "alice",
            "alice@example.com",
            false,
        )
        .await;

        let reply = handler
            .handle(AccountRequest::Begin {
                claimant: Claimant::Native {
                    username: "alice".to_string(),
                    email: "mallory@example.com".to_string(),
                },
            })
            .await;
        assert_eq!(refused(reply), AccountRefusal::NoMatchingProvisioning);
    }

    #[tokio::test]
    async fn a_native_claim_cannot_reach_the_bootstrap_account() {
        let server = MockServer::start().await;
        let handler = handler_for(&server).await;
        provision(
            &handler.ledger,
            "acct-boot",
            BOOTSTRAP_USERNAME,
            BOOTSTRAP_EMAIL,
            true,
        )
        .await;

        // Matching its fixed username and email isn't enough.
        let reply = handler
            .handle(AccountRequest::Begin {
                claimant: Claimant::Native {
                    username: BOOTSTRAP_USERNAME.to_string(),
                    email: BOOTSTRAP_EMAIL.to_string(),
                },
            })
            .await;
        assert_eq!(refused(reply), AccountRefusal::NoMatchingProvisioning);
    }

    #[tokio::test]
    async fn a_csr_built_around_the_wrong_account_id_is_refused_and_nothing_is_signed() {
        let server = MockServer::start().await;
        // Expecting zero signing requests: wiremock checks this when the
        // server drops at the end of the test.
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .expect(0)
            .mount(&server)
            .await;
        let handler = handler_for(&server).await;
        provision(
            &handler.ledger,
            ALICE_ID,
            "alice",
            "alice@example.com",
            false,
        )
        .await;

        let reply = handler
            .handle(AccountRequest::Complete {
                claimant: alice(),
                csr_pem: csr_for("someone-else"),
            })
            .await;
        assert_eq!(refused(reply), AccountRefusal::CsrInvalid);
    }

    #[tokio::test]
    async fn bootstrap_begin_creates_the_account_once_and_repeats_the_same_answer() {
        let server = MockServer::start().await;
        mock_validate(&server, DEFAULT_BOOTSTRAP_IDENTITY).await;
        let handler = handler_for(&server).await;

        let first = offered(
            handler
                .handle(AccountRequest::Begin {
                    claimant: bootstrap(),
                })
                .await,
        );
        let second = offered(
            handler
                .handle(AccountRequest::Begin {
                    claimant: bootstrap(),
                })
                .await,
        );
        assert_eq!(first, second);
        assert!(
            !bootstrap_completed(&handler).await,
            "provisioning alone must not complete bootstrap"
        );
    }

    #[tokio::test]
    async fn a_valid_token_for_some_other_identity_cannot_bootstrap() {
        let server = MockServer::start().await;
        mock_validate(&server, "system:serviceaccount:polar:some-other-service").await;
        let handler = handler_for(&server).await;

        let reply = handler
            .handle(AccountRequest::Begin {
                claimant: bootstrap(),
            })
            .await;
        assert_eq!(refused(reply), AccountRefusal::NotTheBootstrapIdentity);

        let pending = ask(&handler.ledger, |reply| {
            LedgerMessage::CheckProvisionedAccount {
                username: BOOTSTRAP_USERNAME.to_string(),
                reply,
            }
        })
        .await
        .unwrap();
        assert!(pending.is_none(), "a refused token must create nothing");
    }

    #[tokio::test]
    async fn bootstrap_completes_when_its_certificate_is_issued_and_not_before() {
        let server = MockServer::start().await;
        mock_validate(&server, DEFAULT_BOOTSTRAP_IDENTITY).await;
        mock_internal_issue_ok(&server).await;
        let handler = handler_for(&server).await;

        let account_id = offered(
            handler
                .handle(AccountRequest::Begin {
                    claimant: bootstrap(),
                })
                .await,
        );
        assert!(!bootstrap_completed(&handler).await);

        let reply = handler
            .handle(AccountRequest::Complete {
                claimant: bootstrap(),
                csr_pem: csr_for(&account_id),
            })
            .await;
        assert!(
            matches!(reply, AccountReply::Issued { .. }),
            "got {reply:?}"
        );
        assert!(bootstrap_completed(&handler).await);

        // The certificate is recorded as the bootstrap one, holding exactly
        // the one capability.
        let record = ask(&handler.ledger, |reply| {
            LedgerMessage::CheckAccountCertificateBySerial {
                serial: "01:23:45:67".to_string(),
                reply,
            }
        })
        .await
        .unwrap()
        .expect("the issued certificate is recorded");
        assert!(record.via_bootstrap);
        assert_eq!(
            record.capabilities,
            vec![cert_issuer_common::capability::Capability::ProvisionAccount]
        );

        let again = handler
            .handle(AccountRequest::Begin {
                claimant: bootstrap(),
            })
            .await;
        assert_eq!(refused(again), AccountRefusal::BootstrapAlreadyDone);
    }

    #[tokio::test]
    async fn a_failed_signing_leaves_the_claim_to_be_retried_with_the_same_account_id() {
        let server = MockServer::start().await;
        mock_validate(&server, DEFAULT_BOOTSTRAP_IDENTITY).await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let handler = handler_for(&server).await;

        let account_id = offered(
            handler
                .handle(AccountRequest::Begin {
                    claimant: bootstrap(),
                })
                .await,
        );
        let reply = handler
            .handle(AccountRequest::Complete {
                claimant: bootstrap(),
                csr_pem: csr_for(&account_id),
            })
            .await;
        assert_eq!(refused(reply), AccountRefusal::SigningFailed);
        assert!(!bootstrap_completed(&handler).await);

        let retry = offered(
            handler
                .handle(AccountRequest::Begin {
                    claimant: bootstrap(),
                })
                .await,
        );
        assert_eq!(retry, account_id);
    }
}
