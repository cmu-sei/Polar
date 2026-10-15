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

//! A workload asking the credentials agent for its pod-instance certificates before it has any certificate
//! (docs/architecture/cert-issuer-isolation.md, "Scope: polar-init through the observer").
//!
//! This sits beside `CryptoActor` and is not a message on it because the actor cannot exist yet: it is spawned
//! over a Cassini connection, and that connection needs the very client certificate being requested here. It is
//! one half of the crypto lifecycle, the half that runs first; the actor takes over once there is a certificate to
//! connect with, and is where the keys will be held by handle when that work lands.
//!
//! One call is one attempt at one certificate, on the existing flow of several separate requests per pod:
//!
//! 1. generate the keypair and CSR (the private key never leaves this process);
//! 2. ask the observer for an ephemeral session and seal the service-account token to it, so the token is not
//!    on the pod network in the clear;
//! 3. ask for the certificate with the sealed token, and give back what comes.
//!
//! Which failures are worth another attempt is decided here and said in the type ([`RequestError`]), so every
//! caller waits the same way: a flooded or unreachable observer, a session that expired, a signing failure are
//! retried; a refusal is not, and neither is a bug.

use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use credentials_agent_client::{
    ObserverClient, ObserverClientError, SealedOutcome, SessionOutcome,
};
use credentials_agent_common::ephemeral_exchange::EphemeralEncryptedPayload;
use credentials_agent_common::issuance::{IssuanceRefusalReason, IssuanceReply, WorkloadCertType};
use credentials_agent_common::workload_issuance::SealedIssuanceRequest;
use zeroize::Zeroizing;

/// What one certificate is to be.
#[derive(Debug, Clone)]
pub struct CertificateRequest {
    pub cert_type: WorkloadCertType,
    pub key_algorithm: KeyAlgorithm,
    /// The DNS names a server certificate carries besides the identity; none for a client certificate.
    pub names: Vec<String>,
}

/// A certificate that was issued, with the private key it was issued for.
pub struct IssuedCertificate {
    pub certificate_pem: String,
    pub ca_chain_pem: String,
    pub private_key_pem: Zeroizing<String>,
    /// The credentials agent's escrow public key, which the reply carries so the pod need not ask for it.
    pub escrow_public_key_pem: String,
}

impl std::fmt::Debug for IssuedCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedCertificate")
            .field("certificate_pem", &self.certificate_pem)
            .field("private_key_pem", &"<withheld>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// Nothing is wrong with the request. The observer or the credentials agent behind it was not able to answer
    /// just now: try again.
    #[error("not yet, worth trying again: {0}")]
    Retry(String),
    /// The credentials agent refused, and asking again will not change that.
    #[error("the credentials agent refused the request: {0:?}")]
    Refused(IssuanceRefusalReason),
    /// A bug or an impossible configuration on this side.
    #[error("the request could not be made: {0}")]
    Failed(String),
}

fn retry(why: impl std::fmt::Display) -> RequestError {
    RequestError::Retry(why.to_string())
}

fn transport(e: ObserverClientError) -> RequestError {
    match e {
        ObserverClientError::Unreachable(why) | ObserverClientError::Retryable(why) => {
            RequestError::Retry(why)
        }
    }
}

/// Whether a refusal is the credentials agent being unable for now and not saying no.
fn is_temporary(reason: &IssuanceRefusalReason) -> bool {
    matches!(
        reason,
        IssuanceRefusalReason::SigningFailed | IssuanceRefusalReason::TokenIntrospectionFailed
    )
}

/// One attempt at one certificate for `identity` (the DNS-form identity the CSR carries), asking the observer.
pub async fn request_certificate(
    observer: &ObserverClient,
    token: &str,
    identity: &str,
    request: &CertificateRequest,
) -> Result<IssuedCertificate, RequestError> {
    let csr = generate_csr(identity, &request.key_algorithm, &request.names)
        .map_err(|e| RequestError::Failed(format!("cannot generate the CSR: {e}")))?;
    let private_key_pem = Zeroizing::new(csr.private_key_pem);

    let offer = match observer.open_session().await.map_err(transport)? {
        SessionOutcome::Offer(offer) => offer,
        SessionOutcome::Busy => {
            return Err(retry("the credentials agent has too many sessions open"));
        }
    };
    let sealed_credential = EphemeralEncryptedPayload::encrypt(token.as_bytes(), &offer.public_key)
        .map_err(|e| RequestError::Failed(format!("cannot seal the token: {e}")))?;

    let sealed = SealedIssuanceRequest {
        session_id: offer.session_id,
        csr_pem: csr.csr_pem,
        cert_type: request.cert_type,
        names: request.names.clone(),
        sealed_credential,
    };
    match observer.issue_sealed(&sealed).await.map_err(transport)? {
        SealedOutcome::Issuance(IssuanceReply::Issued {
            certificate_pem,
            ca_chain_pem,
            escrow_public_key_pem,
        }) => Ok(IssuedCertificate {
            certificate_pem,
            ca_chain_pem,
            private_key_pem,
            escrow_public_key_pem,
        }),
        SealedOutcome::Issuance(IssuanceReply::Refused { reason }) if is_temporary(&reason) => {
            Err(retry(format!("{reason:?}")))
        }
        SealedOutcome::Issuance(IssuanceReply::Refused { reason }) => {
            Err(RequestError::Refused(reason))
        }
        // The session ran out between the offer and the request: another attempt opens another.
        SealedOutcome::SessionGone => Err(retry("the session expired before it was used")),
        SealedOutcome::Busy => Err(retry("the credentials agent has too many sessions open")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use credentials_agent_common::ephemeral_exchange::EphemeralSession;
    use credentials_agent_common::workload_issuance::SessionOffer;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJzeXN0ZW0ifQ.";

    /// What the credentials agent's consumer does with the two routes, with real sessions: it opens one on
    /// request, and opens the seal on the sealed request, once. What it learned is kept for the tests to read.
    #[derive(Default)]
    struct Consumer {
        sessions: Mutex<HashMap<String, EphemeralSession>>,
        tokens_it_opened: Mutex<Vec<String>>,
        requests_it_answered: Mutex<Vec<SealedIssuanceRequest>>,
        /// What to answer a sealed request that opens.
        answer: Mutex<Option<IssuanceReply>>,
        /// Answer the next session request as full.
        busy: Mutex<bool>,
        next_id: std::sync::atomic::AtomicUsize,
    }

    struct OpenSession(Arc<Consumer>);
    struct Issue(Arc<Consumer>);

    impl Respond for OpenSession {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            if *self.0.busy.lock().unwrap() {
                return ResponseTemplate::new(503).set_body_string("too many sessions are open");
            }
            let session = EphemeralSession::open();
            let offer = SessionOffer {
                session_id: format!(
                    "s{}",
                    self.0
                        .next_id
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                ),
                public_key: session.public_key(),
                valid_for_secs: 60,
            };
            self.0
                .sessions
                .lock()
                .unwrap()
                .insert(offer.session_id.clone(), session);
            ResponseTemplate::new(200).set_body_json(offer)
        }
    }

    impl Respond for Issue {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let sealed: SealedIssuanceRequest = serde_json::from_slice(&request.body).unwrap();
            let Some(session) = self.0.sessions.lock().unwrap().remove(&sealed.session_id) else {
                return ResponseTemplate::new(410)
                    .set_body_json(serde_json::json!({"reason": "unknown_session"}));
            };
            let token = session
                .unseal(&sealed.sealed_credential)
                .expect("the seal opens");
            self.0
                .tokens_it_opened
                .lock()
                .unwrap()
                .push(String::from_utf8(token.to_vec()).unwrap());
            self.0.requests_it_answered.lock().unwrap().push(sealed);
            let reply = self
                .0
                .answer
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(IssuanceReply::Issued {
                    certificate_pem: "CERT".to_string(),
                    ca_chain_pem: "CHAIN".to_string(),
                    escrow_public_key_pem: "ESCROW".to_string(),
                });
            let status = match &reply {
                IssuanceReply::Issued { .. } => 200,
                IssuanceReply::Refused { reason } if is_temporary(reason) => 503,
                IssuanceReply::Refused { .. } => 403,
            };
            ResponseTemplate::new(status).set_body_json(reply)
        }
    }

    async fn observer() -> (MockServer, Arc<Consumer>, ObserverClient) {
        let consumer = Arc::new(Consumer::default());
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(OpenSession(consumer.clone()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(Issue(consumer.clone()))
            .mount(&server)
            .await;
        let client = ObserverClient::new(server.uri());
        (server, consumer, client)
    }

    fn client_request() -> CertificateRequest {
        CertificateRequest {
            cert_type: WorkloadCertType::Client,
            key_algorithm: KeyAlgorithm::EcdsaP256,
            names: vec![],
        }
    }

    const IDENTITY: &str = "git-server.polar.serviceaccount.cluster.local";

    #[tokio::test]
    async fn a_certificate_comes_back_with_the_key_it_was_made_for_and_the_consumer_saw_the_real_token()
     {
        let (_server, consumer, client) = observer().await;
        let issued = request_certificate(&client, TOKEN, IDENTITY, &client_request())
            .await
            .unwrap();
        assert_eq!(issued.certificate_pem, "CERT");
        assert_eq!(issued.ca_chain_pem, "CHAIN");
        assert_eq!(issued.escrow_public_key_pem, "ESCROW");
        assert!(issued.private_key_pem.contains("PRIVATE KEY"));
        assert_eq!(
            *consumer.tokens_it_opened.lock().unwrap(),
            vec![TOKEN.to_string()]
        );
    }

    #[tokio::test]
    async fn the_token_is_never_on_the_wire_in_the_clear_and_the_private_key_never_leaves() {
        let (server, _consumer, client) = observer().await;
        let issued = request_certificate(&client, TOKEN, IDENTITY, &client_request())
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "one to open a session, one to ask");
        for request in &requests {
            let body = String::from_utf8_lossy(&request.body);
            assert!(!body.contains(TOKEN), "the token is in a request body");
            assert!(!body.contains("PRIVATE KEY"));
            assert!(
                !body.contains(issued.private_key_pem.lines().nth(1).unwrap_or("-")),
                "the private key is in a request body"
            );
        }
    }

    #[tokio::test]
    async fn a_server_certificate_asks_for_its_type_and_names_in_the_request_and_the_csr() {
        let (_server, consumer, client) = observer().await;
        let request = CertificateRequest {
            cert_type: WorkloadCertType::Server,
            key_algorithm: KeyAlgorithm::EcdsaP256,
            names: vec!["polar-git-server.polar.svc.cluster.local".to_string()],
        };
        request_certificate(&client, TOKEN, IDENTITY, &request)
            .await
            .unwrap();
        let answered = consumer.requests_it_answered.lock().unwrap();
        let sent = &answered[0];
        assert_eq!(sent.cert_type, WorkloadCertType::Server);
        assert_eq!(sent.names, request.names);
        assert!(sent.csr_pem.contains("CERTIFICATE REQUEST"));
    }

    #[tokio::test]
    async fn every_attempt_has_its_own_session_and_its_own_key() {
        let (_server, consumer, client) = observer().await;
        let first = request_certificate(&client, TOKEN, IDENTITY, &client_request())
            .await
            .unwrap();
        let second = request_certificate(&client, TOKEN, IDENTITY, &client_request())
            .await
            .unwrap();
        assert_ne!(*first.private_key_pem, *second.private_key_pem);
        let sent = consumer.requests_it_answered.lock().unwrap();
        assert_ne!(sent[0].session_id, sent[1].session_id);
    }

    #[tokio::test]
    async fn what_is_only_unavailable_is_retried_and_what_is_refused_is_not() {
        let (_server, consumer, client) = observer().await;

        *consumer.busy.lock().unwrap() = true;
        let err = request_certificate(&client, TOKEN, IDENTITY, &client_request())
            .await
            .unwrap_err();
        assert!(matches!(err, RequestError::Retry(_)), "{err:?}");
        *consumer.busy.lock().unwrap() = false;

        for temporary in [
            IssuanceRefusalReason::SigningFailed,
            IssuanceRefusalReason::TokenIntrospectionFailed,
        ] {
            *consumer.answer.lock().unwrap() = Some(IssuanceReply::Refused { reason: temporary });
            let err = request_certificate(&client, TOKEN, IDENTITY, &client_request())
                .await
                .unwrap_err();
            assert!(matches!(err, RequestError::Retry(_)), "{err:?}");
        }
        for final_answer in [
            IssuanceRefusalReason::TokenInvalid,
            IssuanceRefusalReason::AlreadyHasValidCertificate,
            IssuanceRefusalReason::RequestNotPermitted,
            IssuanceRefusalReason::NoGrantFound,
        ] {
            *consumer.answer.lock().unwrap() = Some(IssuanceReply::Refused {
                reason: final_answer.clone(),
            });
            let err = request_certificate(&client, TOKEN, IDENTITY, &client_request())
                .await
                .unwrap_err();
            match err {
                RequestError::Refused(reason) => assert_eq!(reason, final_answer),
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn an_observer_that_is_not_there_is_retried() {
        let client = ObserverClient::new("http://127.0.0.1:1".to_string());
        let err = request_certificate(&client, TOKEN, IDENTITY, &client_request())
            .await
            .unwrap_err();
        assert!(matches!(err, RequestError::Retry(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_session_that_expired_in_between_is_retried() {
        // An observer that offers a session and then says it is gone.
        let server = MockServer::start().await;
        let session = EphemeralSession::open();
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(SessionOffer {
                session_id: "gone".to_string(),
                public_key: session.public_key(),
                valid_for_secs: 60,
            }))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(
                ResponseTemplate::new(410)
                    .set_body_json(serde_json::json!({"reason": "unknown_session"})),
            )
            .mount(&server)
            .await;
        let client = ObserverClient::new(server.uri());
        let err = request_certificate(&client, TOKEN, IDENTITY, &client_request())
            .await
            .unwrap_err();
        assert!(matches!(err, RequestError::Retry(_)), "{err:?}");
    }

    #[test]
    fn an_issued_certificate_does_not_print_its_key() {
        let issued = IssuedCertificate {
            certificate_pem: "CERT".to_string(),
            ca_chain_pem: "CHAIN".to_string(),
            private_key_pem: Zeroizing::new("-----BEGIN PRIVATE KEY-----\nSECRET".to_string()),
            escrow_public_key_pem: "ESCROW".to_string(),
        };
        assert!(!format!("{issued:?}").contains("SECRET"));
    }
}
