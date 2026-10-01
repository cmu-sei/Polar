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

//! POST /issue -- the observer's synchronous-HTTP-to-asynchronous-
//! Cassini gateway for the issuance flow, per
//! cassini-request-reply-protocol.md. Accepts minimal input, does the
//! observer's own well-formedness checking (never a trust decision),
//! and forwards blindly -- the same shape for both real callers of
//! this endpoint (a human's tool with an IdP token, an admin's
//! bootstrap tool with a Kubernetes credential).

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use cassini_client::TcpClientMessage;
use credentials_agent_common::CREDENTIALS_ACCOUNT_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_ISSUANCE_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC;
use credentials_agent_common::account_claim::{
    AccountRefusal, AccountReply, AccountRequest, Claimant,
};
use credentials_agent_common::identity::{
    IdentityNormalizationReply, IdentityNormalizationRefusalReason, IdentityNormalizationRequest,
};
use credentials_agent_common::issuance::{IssuanceReply, IssuanceRefusalReason, IssuanceRequest};
use credentials_agent_common::observer::ObserverRejection;
use polar::request_reply::{Envelope, ReplyRouter};
use ractor::ActorRef;
use std::sync::Arc;
use std::time::Duration;
use tracing::{warn, error};

/// Hard ceiling on a single attempt's wait, per
/// cassini-request-reply-protocol.md: `min(token_remaining_lifetime / 3, 100s)`.
const MAX_TIMEOUT_SECS: i64 = 100;

pub struct ObserverState {
    pub broker_client: ActorRef<TcpClientMessage>,
    pub issuance_reply_router: Arc<ReplyRouter<IssuanceReply>>,
    pub identity_reply_router: Arc<ReplyRouter<IdentityNormalizationReply>>,
    pub account_reply_router: Arc<ReplyRouter<AccountReply>>,
}

pub async fn issue(
    State(state): State<Arc<ObserverState>>,
    Json(request): Json<IssuanceRequest>,
) -> Response {
    // Well-formedness checking -- the observer's own documented role,
    // never a trust decision. Reuses cert-issuer's own, already-tested
    // CSR parser rather than reimplementing it.
    if let Err(e) = cert_issuer::csr::parse_csr(&request.csr_pem) {
        let rejection = ObserverRejection::MalformedCsr(e.to_string());
        return (StatusCode::BAD_REQUEST, Json(rejection)).into_response();
    }

    let correlation_id = uuid::Uuid::new_v4().to_string();
    let timeout = compute_timeout(&request.credential);

    // Registered *before* publishing -- a reply arriving in the gap
    // between publishing and registering would have nowhere to route.
    let rx = state.issuance_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
        Ok(bytes) => bytes.to_vec(),
        Err(e) => {
            state.issuance_reply_router.cancel(&correlation_id);
            error!("Failed to serialize issuance request envelope: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };

    if let Err(e) = state.broker_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_ISSUANCE_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        state.issuance_reply_router.cancel(&correlation_id);
        error!("Failed to publish issuance request: {e}");
        return (StatusCode::SERVICE_UNAVAILABLE, "failed to reach the broker").into_response();
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(reply)) => reply_to_response(reply),
        Ok(Err(_)) => {
            warn!("Reply channel closed with no reply for correlation_id {correlation_id}");
            (StatusCode::GATEWAY_TIMEOUT, "no reply received").into_response()
        }
        Err(_) => {
            state.issuance_reply_router.cancel(&correlation_id);
            (StatusCode::GATEWAY_TIMEOUT, "timed out waiting for a reply").into_response()
        }
    }
}

pub async fn identity(
    State(state): State<Arc<ObserverState>>,
    Json(request): Json<IdentityNormalizationRequest>,
) -> Response {
    let correlation_id = uuid::Uuid::new_v4().to_string();
    let timeout = compute_timeout(&request.credential);

    let rx = state.identity_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
        Ok(bytes) => bytes.to_vec(),
        Err(e) => {
            state.identity_reply_router.cancel(&correlation_id);
            error!("Failed to serialize identity-normalization request envelope: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };

    if let Err(e) = state.broker_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        state.identity_reply_router.cancel(&correlation_id);
        error!("Failed to publish identity-normalization request: {e}");
        return (StatusCode::SERVICE_UNAVAILABLE, "failed to reach the broker").into_response();
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(reply)) => identity_reply_to_response(reply),
        Ok(Err(_)) => {
            warn!("Reply channel closed with no reply for correlation_id {correlation_id}");
            (StatusCode::GATEWAY_TIMEOUT, "no reply received").into_response()
        }
        Err(_) => {
            state.identity_reply_router.cancel(&correlation_id);
            (StatusCode::GATEWAY_TIMEOUT, "timed out waiting for a reply").into_response()
        }
    }
}

fn identity_reply_to_response(reply: IdentityNormalizationReply) -> Response {
    let status = match &reply {
        IdentityNormalizationReply::Normalized { .. } => StatusCode::OK,
        IdentityNormalizationReply::Refused { reason } => match reason {
            IdentityNormalizationRefusalReason::TokenInvalid => StatusCode::UNAUTHORIZED,
        },
    };
    (status, Json(reply)).into_response()
}

/// POST /account -- both phases of the account claim, native and bootstrap
/// alike, relayed blindly. The observer checks well-formedness (a CSR has to
/// parse) and never a trust decision: who may claim what is the consumer's
/// call, from the ledger.
pub async fn account(
    State(state): State<Arc<ObserverState>>,
    Json(request): Json<AccountRequest>,
) -> Response {
    if let AccountRequest::Complete { csr_pem, .. } = &request
        && let Err(e) = cert_issuer::csr::parse_csr(csr_pem)
    {
        let rejection = ObserverRejection::MalformedCsr(e.to_string());
        return (StatusCode::BAD_REQUEST, Json(rejection)).into_response();
    }

    let claimant = match &request {
        AccountRequest::Begin { claimant } | AccountRequest::Complete { claimant, .. } => claimant,
    };
    let timeout = claimant_timeout(claimant);

    let correlation_id = uuid::Uuid::new_v4().to_string();
    let rx = state.account_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
        Ok(bytes) => bytes.to_vec(),
        Err(e) => {
            state.account_reply_router.cancel(&correlation_id);
            error!("Failed to serialize account request envelope: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };

    if let Err(e) = state.broker_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_ACCOUNT_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        state.account_reply_router.cancel(&correlation_id);
        error!("Failed to publish account request: {e}");
        return (StatusCode::SERVICE_UNAVAILABLE, "failed to reach the broker").into_response();
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(reply)) => account_reply_to_response(reply),
        Ok(Err(_)) => {
            warn!("Reply channel closed with no reply for correlation_id {correlation_id}");
            (StatusCode::GATEWAY_TIMEOUT, "no reply received").into_response()
        }
        Err(_) => {
            state.account_reply_router.cancel(&correlation_id);
            (StatusCode::GATEWAY_TIMEOUT, "timed out waiting for a reply").into_response()
        }
    }
}

/// A bootstrap claim carries a token, so it waits as long as an issuance
/// does. A native claim has no credential to read a lifetime from and gets
/// the full ceiling.
fn claimant_timeout(claimant: &Claimant) -> Duration {
    match claimant {
        Claimant::Bootstrap { credential } => compute_timeout(credential),
        Claimant::Native { .. } => Duration::from_secs(MAX_TIMEOUT_SECS as u64),
    }
}

fn account_reply_to_response(reply: AccountReply) -> Response {
    let status = match &reply {
        AccountReply::Offered { .. } | AccountReply::Issued { .. } => StatusCode::OK,
        AccountReply::Refused { reason } => match reason {
            AccountRefusal::TokenInvalid => StatusCode::UNAUTHORIZED,
            AccountRefusal::NoMatchingProvisioning
            | AccountRefusal::NotTheBootstrapIdentity
            | AccountRefusal::BootstrapAlreadyDone => StatusCode::FORBIDDEN,
            AccountRefusal::CsrInvalid => StatusCode::BAD_REQUEST,
            AccountRefusal::SigningFailed | AccountRefusal::Unavailable => {
                StatusCode::SERVICE_UNAVAILABLE
            }
        },
    };
    (status, Json(reply)).into_response()
}

/// `min(token_remaining_lifetime / 3, 100s)`, computed by peeking the
/// unverified `exp` claim -- an operational read for timing purposes
/// only, the same category of operation as `jwt_utils::peek_claim`
/// itself; never a trust decision. Falls back to the full 100s ceiling
/// if `exp` can't be read at all (malformed credential, or a
/// Kubernetes token whose exp claim this function doesn't need to
/// understand precisely -- the consumer's own real validation is what
/// actually matters here, not this timing estimate).
fn compute_timeout(credential: &str) -> Duration {
    let default = Duration::from_secs(MAX_TIMEOUT_SECS as u64);

    let Some(exp) = jwt_utils::peek_numeric_claim(credential, "exp") else {
        return default;
    };

    let now = chrono::Utc::now().timestamp();
    let remaining = exp - now;
    if remaining <= 0 {
        // Already expired -- no point waiting meaningfully; the
        // consumer's real validation will reject it almost instantly.
        return Duration::from_secs(1);
    }

    let computed_secs = (remaining / 3).min(MAX_TIMEOUT_SECS).max(1);
    Duration::from_secs(computed_secs as u64)
}

fn reply_to_response(reply: IssuanceReply) -> Response {
    let status = match &reply {
        IssuanceReply::Issued { .. } => StatusCode::OK,
        IssuanceReply::Refused { reason } => match reason {
            IssuanceRefusalReason::TokenInvalid => StatusCode::UNAUTHORIZED,
            IssuanceRefusalReason::TokenIntrospectionFailed => StatusCode::SERVICE_UNAVAILABLE,
            IssuanceRefusalReason::NoGrantFound => StatusCode::FORBIDDEN,
            IssuanceRefusalReason::AlreadyHasValidCertificate => StatusCode::FORBIDDEN,
            IssuanceRefusalReason::SigningFailed => StatusCode::SERVICE_UNAVAILABLE,
        },
    };
    (status, Json(reply)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Json, State};
    use ractor::{Actor, ActorProcessingErr, async_trait};

    /// Simulates a broker connection for testing: on receiving a
    /// Publish, deserializes the payload as Envelope<IssuanceRequest>
    /// and immediately delivers a canned reply through the same
    /// ReplyRouter -- standing in for what a real round trip over
    /// Cassini would eventually do, without needing an actual broker.
    struct FakeBroker {
        reply_router: Arc<ReplyRouter<IssuanceReply>>,
        canned_reply: IssuanceReply,
    }

    #[async_trait]
    impl Actor for FakeBroker {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message
                && let Ok(envelope) =
                    rkyv::from_bytes::<Envelope<IssuanceRequest>, rkyv::rancor::Error>(&payload)
            {
                self.reply_router
                    .deliver(&envelope.correlation_id, self.canned_reply.clone());
            }
            Ok(())
        }
    }

    async fn spawn_fake_broker(
        reply_router: Arc<ReplyRouter<IssuanceReply>>,
        canned_reply: IssuanceReply,
    ) -> ActorRef<TcpClientMessage> {
        let (broker, _) = Actor::spawn(
            None,
            FakeBroker {
                reply_router,
                canned_reply,
            },
            (),
        )
        .await
        .unwrap();
        broker
    }

    fn well_formed_csr() -> String {
        let params =
            rcgen::CertificateParams::new(vec!["test.polar.svc.cluster.local".to_string()])
                .expect("valid params");
        let key_pair = rcgen::KeyPair::generate().expect("keypair generation");
        let csr = params
            .serialize_request(&key_pair)
            .expect("CSR serialization");
        csr.pem().expect("PEM encoding")
    }

    #[tokio::test]
    async fn malformed_csr_is_rejected_before_ever_publishing() {
        let reply_router = Arc::new(ReplyRouter::new());
        // A broker that panics if ever called -- proving the
        // well-formedness check genuinely short-circuits before any
        // publish is attempted, not just happens to work.
        let (broker, _) = Actor::spawn(
            None,
            {
                struct NeverCalled;
                #[async_trait]
                impl Actor for NeverCalled {
                    type Msg = TcpClientMessage;
                    type State = ();
                    type Arguments = ();
                    async fn pre_start(
                        &self,
                        _myself: ActorRef<Self::Msg>,
                        _args: (),
                    ) -> Result<Self::State, ActorProcessingErr> {
                        Ok(())
                    }
                    async fn handle(
                        &self,
                        _myself: ActorRef<Self::Msg>,
                        _message: Self::Msg,
                        _state: &mut Self::State,
                    ) -> Result<(), ActorProcessingErr> {
                        panic!("the broker must never be called for a malformed CSR");
                    }
                }
                NeverCalled
            },
            (),
        )
        .await
        .unwrap();

        let state = Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = issue(
            State(state),
            Json(IssuanceRequest {
                csr_pem: "not a real CSR".to_string(),
                credential: "irrelevant".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn successful_reply_returns_200_with_the_issued_body() {
        let reply_router = Arc::new(ReplyRouter::new());
        let canned_reply = IssuanceReply::Issued {
            certificate_pem: "fake-cert".to_string(),
            ca_chain_pem: "fake-chain".to_string(),
            escrow_public_key_pem: "fake-escrow-key".to_string(),
        };
        let broker = spawn_fake_broker(reply_router.clone(), canned_reply).await;

        let state = Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = issue(
            State(state),
            Json(IssuanceRequest {
                csr_pem: well_formed_csr(),
                credential: "any-credential".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn refused_reply_maps_to_the_correct_status_code() {
        let reply_router = Arc::new(ReplyRouter::new());
        let canned_reply = IssuanceReply::Refused {
            reason: IssuanceRefusalReason::NoGrantFound,
        };
        let broker = spawn_fake_broker(reply_router.clone(), canned_reply).await;

        let state = Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = issue(
            State(state),
            Json(IssuanceRequest {
                csr_pem: well_formed_csr(),
                credential: "any-credential".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn no_reply_at_all_eventually_times_out() {
        // A broker that receives the publish but never delivers
        // anything -- proving the timeout path itself, not just the
        // happy path.
        struct SilentBroker;
        #[async_trait]
        impl Actor for SilentBroker {
            type Msg = TcpClientMessage;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _myself: ActorRef<Self::Msg>,
                _args: (),
            ) -> Result<Self::State, ActorProcessingErr> {
                Ok(())
            }
            async fn handle(
                &self,
                _myself: ActorRef<Self::Msg>,
                _message: Self::Msg,
                _state: &mut Self::State,
            ) -> Result<(), ActorProcessingErr> {
                Ok(()) // received, but never delivers a reply
            }
        }

        let (broker, _) = Actor::spawn(None, SilentBroker, ()).await.unwrap();
        let reply_router = Arc::new(ReplyRouter::new());
        let state = Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
        });

        // A credential with no readable exp claim falls back to the
        // full 100s ceiling -- too slow for a unit test to actually
        // wait out, so this test only confirms the request completes
        // at all rather than hanging forever; the real timeout-timing
        // math is covered separately in compute_timeout's own tests.
        // Using tokio::time::pause/advance here instead:
        tokio::time::pause();
        let handle = tokio::spawn(async move {
            issue(
                State(state),
                Json(IssuanceRequest {
                    csr_pem: well_formed_csr(),
                    credential: "any-credential".to_string(),
                }),
            )
            .await
        });
        tokio::time::advance(Duration::from_secs(101)).await;
        let response = handle.await.unwrap();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[test]
    fn compute_timeout_uses_a_third_of_remaining_lifetime() {
        let exp = chrono::Utc::now().timestamp() + 30; // 30s remaining
        let token = format!(
            "{}.{}.",
            base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                r#"{"alg":"none"}"#
            ),
            base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                format!(r#"{{"exp":{exp}}}"#)
            ),
        );
        let timeout = compute_timeout(&token);
        assert_eq!(timeout, Duration::from_secs(10)); // 30 / 3
    }

    #[test]
    fn compute_timeout_falls_back_to_ceiling_when_exp_is_unreadable() {
        let timeout = compute_timeout("not-a-real-token");
        assert_eq!(timeout, Duration::from_secs(MAX_TIMEOUT_SECS as u64));
    }

    struct FakeIdentityBroker {
        reply_router: Arc<ReplyRouter<IdentityNormalizationReply>>,
        canned_reply: IdentityNormalizationReply,
    }

    #[async_trait]
    impl Actor for FakeIdentityBroker {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message
                && let Ok(envelope) =
                    rkyv::from_bytes::<Envelope<IdentityNormalizationRequest>, rkyv::rancor::Error>(&payload)
            {
                self.reply_router
                    .deliver(&envelope.correlation_id, self.canned_reply.clone());
            }
            Ok(())
        }
    }

    async fn spawn_fake_identity_broker(
        reply_router: Arc<ReplyRouter<IdentityNormalizationReply>>,
        canned_reply: IdentityNormalizationReply,
    ) -> ActorRef<TcpClientMessage> {
        let (broker, _) = Actor::spawn(
            None,
            FakeIdentityBroker {
                reply_router,
                canned_reply,
            },
            (),
        )
        .await
        .unwrap();
        broker
    }

    #[tokio::test]
    async fn identity_successful_reply_returns_200_with_the_normalized_identity() {
        let identity_reply_router = Arc::new(ReplyRouter::new());
        let canned_reply = IdentityNormalizationReply::Normalized {
            identity: "testuser.polar.keycloak-issuer".to_string(),
        };
        let broker = spawn_fake_identity_broker(identity_reply_router.clone(), canned_reply).await;

        let state = Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router,
            account_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = identity(
            State(state),
            Json(IdentityNormalizationRequest {
                credential: "any-token".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn identity_refused_reply_maps_to_401() {
        let identity_reply_router = Arc::new(ReplyRouter::new());
        let canned_reply = IdentityNormalizationReply::Refused {
            reason: IdentityNormalizationRefusalReason::TokenInvalid,
        };
        let broker = spawn_fake_identity_broker(identity_reply_router.clone(), canned_reply).await;

        let state = Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router,
            account_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = identity(
            State(state),
            Json(IdentityNormalizationRequest {
                credential: "bad-token".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn identity_no_reply_at_all_eventually_times_out() {
        struct SilentBroker;
        #[async_trait]
        impl Actor for SilentBroker {
            type Msg = TcpClientMessage;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _myself: ActorRef<Self::Msg>,
                _args: (),
            ) -> Result<Self::State, ActorProcessingErr> {
                Ok(())
            }
            async fn handle(
                &self,
                _myself: ActorRef<Self::Msg>,
                _message: Self::Msg,
                _state: &mut Self::State,
            ) -> Result<(), ActorProcessingErr> {
                Ok(())
            }
        }

        let (broker, _) = Actor::spawn(None, SilentBroker, ()).await.unwrap();
        let state = Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
        });

        tokio::time::pause();
        let handle = tokio::spawn(async move {
            identity(
                State(state),
                Json(IdentityNormalizationRequest {
                    credential: "any-token".to_string(),
                }),
            )
            .await
        });
        tokio::time::advance(Duration::from_secs(101)).await;
        let response = handle.await.unwrap();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;
    use ractor::{Actor, ActorProcessingErr, async_trait};

    /// Stands in for the broker: on a publish, decodes the account request
    /// and delivers a canned reply through the same router.
    struct FakeAccountBroker {
        reply_router: Arc<ReplyRouter<AccountReply>>,
        canned_reply: AccountReply,
    }

    #[async_trait]
    impl Actor for FakeAccountBroker {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message
                && let Ok(envelope) =
                    rkyv::from_bytes::<Envelope<AccountRequest>, rkyv::rancor::Error>(&payload)
            {
                self.reply_router
                    .deliver(&envelope.correlation_id, self.canned_reply.clone());
            }
            Ok(())
        }
    }

    /// Never answers, and fails the test if it is even asked.
    struct PanickingBroker;

    #[async_trait]
    impl Actor for PanickingBroker {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            _message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            panic!("the broker must never be called for a malformed CSR");
        }
    }

    /// Receives the publish and never replies.
    struct SilentBroker;

    #[async_trait]
    impl Actor for SilentBroker {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            _message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
    }

    fn state_with(
        broker: ActorRef<TcpClientMessage>,
        account_reply_router: Arc<ReplyRouter<AccountReply>>,
    ) -> Arc<ObserverState> {
        Arc::new(ObserverState {
            broker_client: broker,
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router,
        })
    }

    fn well_formed_csr() -> String {
        let params = rcgen::CertificateParams::new(vec!["acct-1".to_string()]).unwrap();
        let key_pair = rcgen::KeyPair::generate().unwrap();
        params.serialize_request(&key_pair).unwrap().pem().unwrap()
    }

    fn native() -> Claimant {
        Claimant::Native {
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
        }
    }

    async fn broker_replying(
        reply: AccountReply,
    ) -> (ActorRef<TcpClientMessage>, Arc<ReplyRouter<AccountReply>>) {
        let router = Arc::new(ReplyRouter::new());
        let (broker, _) = Actor::spawn(
            None,
            FakeAccountBroker {
                reply_router: router.clone(),
                canned_reply: reply,
            },
            (),
        )
        .await
        .unwrap();
        (broker, router)
    }

    #[tokio::test]
    async fn a_malformed_csr_is_rejected_before_anything_is_published() {
        let (broker, _) = Actor::spawn(None, PanickingBroker, ()).await.unwrap();
        let state = state_with(broker, Arc::new(ReplyRouter::new()));

        let response = account(
            State(state),
            Json(AccountRequest::Complete {
                claimant: native(),
                csr_pem: "not a real CSR".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn begin_needs_no_csr_and_an_offer_comes_back_as_200() {
        let (broker, router) = broker_replying(AccountReply::Offered {
            account_id: "acct-1".to_string(),
        })
        .await;
        let state = state_with(broker, router);

        let response = account(
            State(state),
            Json(AccountRequest::Begin { claimant: native() }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn complete_with_a_well_formed_csr_returns_the_issued_certificate() {
        let (broker, router) = broker_replying(AccountReply::Issued {
            certificate_pem: "fake-cert".to_string(),
            ca_chain_pem: "fake-chain".to_string(),
        })
        .await;
        let state = state_with(broker, router);

        let response = account(
            State(state),
            Json(AccountRequest::Complete {
                claimant: native(),
                csr_pem: well_formed_csr(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn each_refusal_maps_to_its_status() {
        for (reason, expected) in [
            (AccountRefusal::TokenInvalid, StatusCode::UNAUTHORIZED),
            (AccountRefusal::NoMatchingProvisioning, StatusCode::FORBIDDEN),
            (AccountRefusal::NotTheBootstrapIdentity, StatusCode::FORBIDDEN),
            (AccountRefusal::BootstrapAlreadyDone, StatusCode::FORBIDDEN),
            (AccountRefusal::CsrInvalid, StatusCode::BAD_REQUEST),
            (AccountRefusal::SigningFailed, StatusCode::SERVICE_UNAVAILABLE),
            (AccountRefusal::Unavailable, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let (broker, router) = broker_replying(AccountReply::Refused { reason }).await;
            let state = state_with(broker, router);

            let response = account(
                State(state),
                Json(AccountRequest::Begin { claimant: native() }),
            )
            .await;

            assert_eq!(response.status(), expected, "for {reason:?}");
        }
    }

    #[tokio::test]
    async fn no_reply_at_all_times_out() {
        let (broker, _) = Actor::spawn(None, SilentBroker, ()).await.unwrap();
        let state = state_with(broker, Arc::new(ReplyRouter::new()));

        tokio::time::pause();
        let handle = tokio::spawn(async move {
            account(
                State(state),
                Json(AccountRequest::Begin { claimant: native() }),
            )
            .await
        });
        tokio::time::advance(Duration::from_secs(101)).await;
        let response = handle.await.unwrap();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[test]
    fn a_native_claim_waits_the_full_ceiling() {
        assert_eq!(
            claimant_timeout(&native()),
            Duration::from_secs(MAX_TIMEOUT_SECS as u64)
        );
    }
}
