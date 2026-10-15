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

//! A workload's request for its pod-instance certificates before it has any certificate, with its token sealed to
//! an ephemeral session (credentials-agent-common's `workload_issuance`). The session ends here: the observer
//! relays the sealed bytes and never holds the token.
//!
//! Nothing about issuance is decided here. Once the token is unsealed the request is the same request the
//! unsealed route makes, and `IssuanceHandler` applies every rule it has: the credential's kind, the renewal rule
//! per type, the grant, the server names, cert-issuer. This only opens sessions and opens the seal.

use crate::issuance::IssuanceHandler;
use crate::sessions::{MAX_OPEN_SESSIONS, SESSION_TTL, Sessions};
use credentials_agent_common::issuance::{IssuanceRefusalReason, IssuanceReply, IssuanceRequest};
use credentials_agent_common::workload_issuance::{WorkloadIssuanceReply, WorkloadIssuanceRequest};
use std::time::Instant;
use tracing::{debug, warn};

pub struct WorkloadIssuanceHandler {
    issuance: IssuanceHandler,
    sessions: Sessions,
}

impl WorkloadIssuanceHandler {
    pub fn new(issuance: IssuanceHandler) -> Self {
        Self::with_sessions(issuance, Sessions::new(SESSION_TTL, MAX_OPEN_SESSIONS))
    }

    pub fn with_sessions(issuance: IssuanceHandler, sessions: Sessions) -> Self {
        Self { issuance, sessions }
    }

    pub async fn handle(
        &mut self,
        request: WorkloadIssuanceRequest,
        now: Instant,
    ) -> WorkloadIssuanceReply {
        match request {
            WorkloadIssuanceRequest::OpenSession => match self.sessions.open(now) {
                Ok(offer) => {
                    debug!(open = self.sessions.len(), "a session was opened");
                    WorkloadIssuanceReply::Offer(offer)
                }
                Err(_) => {
                    warn!("a session was refused: too many are open");
                    WorkloadIssuanceReply::Busy
                }
            },
            WorkloadIssuanceRequest::Issue(sealed) => {
                // The session is used up here whatever comes of the seal: one try per session.
                let Some(session) = self.sessions.take(&sealed.session_id, now) else {
                    return WorkloadIssuanceReply::UnknownSession;
                };
                let credential = match session
                    .unseal(&sealed.sealed_credential)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                {
                    Some(credential) => credential,
                    None => {
                        // Not sealed to this session, or changed on the way, or not text. Said without saying
                        // which, and without the payload.
                        warn!("a sealed credential did not open");
                        return WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                            reason: IssuanceRefusalReason::TokenInvalid,
                        });
                    }
                };
                let reply = self
                    .issuance
                    .handle(IssuanceRequest {
                        csr_pem: sealed.csr_pem,
                        credential,
                        cert_type: sealed.cert_type,
                        names: sealed.names,
                    })
                    .await;
                WorkloadIssuanceReply::Issuance(reply)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issuance::tests::{
        CASSINI_NAME, K8S, handler_for, kubernetes_pod, make_token, response_with_serial,
        spawn_test_ledger,
    };
    use credentials_agent_common::ephemeral_exchange::EphemeralEncryptedPayload;
    use credentials_agent_common::issuance::WorkloadCertType;
    use credentials_agent_common::workload_issuance::{SealedIssuanceRequest, SessionOffer};
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn handler(server: &MockServer) -> WorkloadIssuanceHandler {
        WorkloadIssuanceHandler::new(handler_for(server, spawn_test_ledger().await).await)
    }

    async fn opened(handler: &mut WorkloadIssuanceHandler, now: Instant) -> SessionOffer {
        match handler
            .handle(WorkloadIssuanceRequest::OpenSession, now)
            .await
        {
            WorkloadIssuanceReply::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    /// What a workload does: seal the token to the offer and name the session.
    fn sealed(
        offer: &SessionOffer,
        token: &str,
        cert_type: WorkloadCertType,
        names: &[&str],
    ) -> SealedIssuanceRequest {
        SealedIssuanceRequest {
            session_id: offer.session_id.clone(),
            csr_pem: "fake-csr".to_string(),
            cert_type,
            names: names.iter().map(|s| s.to_string()).collect(),
            sealed_credential: EphemeralEncryptedPayload::encrypt(
                token.as_bytes(),
                &offer.public_key,
            )
            .unwrap(),
        }
    }

    fn issued(reply: &WorkloadIssuanceReply) -> bool {
        matches!(
            reply,
            WorkloadIssuanceReply::Issuance(IssuanceReply::Issued { .. })
        )
    }

    #[tokio::test]
    async fn a_token_sealed_to_an_opened_session_reaches_cert_issuer_as_that_token() {
        let server = MockServer::start().await;
        let token = make_token(K8S);
        // cert-issuer must be asked with the real token, unsealed.
        Mock::given(method("POST"))
            .and(path("/validate"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:git-server".to_string(),
                    instance_binding: Some("pod-1".to_string()),
                    jti: None,
                    normalized_identity: Some(
                        "git-server.polar.serviceaccount.cluster.local".to_string(),
                    ),
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("client-1")),
            )
            .mount(&server)
            .await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;

        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &token,
                    WorkloadCertType::Client,
                    &[],
                )),
                now,
            )
            .await;
        assert!(issued(&reply), "{reply:?}");
    }

    #[tokio::test]
    async fn the_type_and_names_a_sealed_request_makes_are_carried_through_to_cert_issuer() {
        let server = MockServer::start().await;
        kubernetes_pod(&server, "pod-1").await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "cert_type": "SERVER",
                "extra_sans": [CASSINI_NAME]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("server-1")),
            )
            .mount(&server)
            .await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token(K8S),
                    WorkloadCertType::Server,
                    &[CASSINI_NAME],
                )),
                now,
            )
            .await;
        assert!(issued(&reply), "{reply:?}");
    }

    #[tokio::test]
    async fn the_rules_of_issuance_still_apply_to_a_sealed_request() {
        // A person's credential asking for a server certificate is refused by IssuanceHandler, not here.
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token("https://keycloak.example/realms/polar"),
                    WorkloadCertType::Server,
                    &[CASSINI_NAME],
                )),
                now,
            )
            .await;
        assert!(matches!(
            reply,
            WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                reason: IssuanceRefusalReason::RequestNotPermitted
            })
        ));
    }

    #[tokio::test]
    async fn a_session_is_used_once_and_a_replayed_request_finds_it_gone() {
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let request = sealed(&offer, &make_token(K8S), WorkloadCertType::Client, &[]);
        handler
            .handle(WorkloadIssuanceRequest::Issue(request.clone()), now)
            .await;
        let again = handler
            .handle(WorkloadIssuanceRequest::Issue(request), now)
            .await;
        assert!(
            matches!(again, WorkloadIssuanceReply::UnknownSession),
            "{again:?}"
        );
    }

    #[tokio::test]
    async fn an_expired_or_unknown_session_is_unknown_and_nothing_is_asked_of_cert_issuer() {
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let late = now + SESSION_TTL;
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token(K8S),
                    WorkloadCertType::Client,
                    &[],
                )),
                late,
            )
            .await;
        assert!(matches!(reply, WorkloadIssuanceReply::UnknownSession));
        let stranger = SealedIssuanceRequest {
            session_id: "never-opened".to_string(),
            ..sealed(&offer, &make_token(K8S), WorkloadCertType::Client, &[])
        };
        let reply = handler
            .handle(WorkloadIssuanceRequest::Issue(stranger), now)
            .await;
        assert!(matches!(reply, WorkloadIssuanceReply::UnknownSession));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_credential_that_does_not_open_is_refused_the_session_is_spent_and_cert_issuer_is_not_asked()
     {
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let other = opened(&mut handler, now).await;

        // Sealed to another session's key.
        let mut wrong_key = sealed(&other, &make_token(K8S), WorkloadCertType::Client, &[]);
        wrong_key.session_id = offer.session_id.clone();
        let reply = handler
            .handle(WorkloadIssuanceRequest::Issue(wrong_key), now)
            .await;
        assert!(
            matches!(
                reply,
                WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid
                })
            ),
            "{reply:?}"
        );

        // Changed on the way.
        let mut tampered = sealed(&other, &make_token(K8S), WorkloadCertType::Client, &[]);
        let mut json = serde_json::to_value(&tampered.sealed_credential).unwrap();
        let first = json["ciphertext"][0].as_u64().unwrap();
        json["ciphertext"][0] = serde_json::json!(first ^ 1);
        tampered.sealed_credential = serde_json::from_value(json).unwrap();
        let reply = handler
            .handle(WorkloadIssuanceRequest::Issue(tampered), now)
            .await;
        assert!(matches!(
            reply,
            WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenInvalid
            })
        ));

        // Both sessions are spent: a second try on either finds nothing.
        for id in [&offer.session_id, &other.session_id] {
            let again = SealedIssuanceRequest {
                session_id: id.clone(),
                ..sealed(&offer, &make_token(K8S), WorkloadCertType::Client, &[])
            };
            let reply = handler
                .handle(WorkloadIssuanceRequest::Issue(again), now)
                .await;
            assert!(matches!(reply, WorkloadIssuanceReply::UnknownSession));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_full_table_says_busy_and_a_waiting_session_still_works() {
        let server = MockServer::start().await;
        kubernetes_pod(&server, "pod-1").await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("client-1")),
            )
            .mount(&server)
            .await;
        let issuance = handler_for(&server, spawn_test_ledger().await).await;
        let mut handler = WorkloadIssuanceHandler::with_sessions(
            issuance,
            Sessions::new(Duration::from_secs(60), 1),
        );
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let busy = handler
            .handle(WorkloadIssuanceRequest::OpenSession, now)
            .await;
        assert!(matches!(busy, WorkloadIssuanceReply::Busy));
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token(K8S),
                    WorkloadCertType::Client,
                    &[],
                )),
                now,
            )
            .await;
        assert!(issued(&reply), "{reply:?}");
    }
}
