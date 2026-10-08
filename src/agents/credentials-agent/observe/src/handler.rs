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
use credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC;
use credentials_agent_common::admin_action::{
    AdminActionRefusal, AdminActionReply, SignedAdminAction,
};
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
use polar::request_reply::{Envelope, ReplyError, ReplyRouter};
use ractor::ActorRef;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Hard ceiling on a single attempt's wait, per
/// cassini-request-reply-protocol.md: `min(token_remaining_lifetime / 3, 100s)`.
const MAX_TIMEOUT_SECS: i64 = 100;

/// The Cassini client the request handlers publish through.
///
/// A handle rather than a bare `ActorRef`: when the agent builder detects
/// that the TCP client died and spawns a fresh one, the supervisor calls
/// `replace`, and every handler picks up the new client on its next
/// request. A bare `ActorRef` captured at startup would keep publishing
/// into the dead client for the rest of the pod's life.
///
/// `send_message` has the same signature as `ActorRef::send_message`, so
/// a handler publishes exactly as it did before.
pub struct BrokerHandle {
    current: std::sync::RwLock<ActorRef<TcpClientMessage>>,
}

impl BrokerHandle {
    pub fn new(client: ActorRef<TcpClientMessage>) -> Self {
        Self {
            current: std::sync::RwLock::new(client),
        }
    }

    /// Points every later `send_message` at `client`.
    pub fn replace(&self, client: ActorRef<TcpClientMessage>) {
        // A poisoned lock only means another thread panicked while holding
        // it; the ActorRef inside is still valid, so keep going.
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = client;
    }

    pub fn send_message(
        &self,
        message: TcpClientMessage,
    ) -> Result<(), ractor::MessagingErr<TcpClientMessage>> {
        let client = self
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        client.send_message(message)
    }
}

impl From<ActorRef<TcpClientMessage>> for BrokerHandle {
    fn from(client: ActorRef<TcpClientMessage>) -> Self {
        Self::new(client)
    }
}

pub struct ObserverState {
    pub broker_client: BrokerHandle,
    pub issuance_reply_router: Arc<ReplyRouter<IssuanceReply>>,
    pub identity_reply_router: Arc<ReplyRouter<IdentityNormalizationReply>>,
    pub account_reply_router: Arc<ReplyRouter<AccountReply>>,
    pub admin_action_reply_router: Arc<ReplyRouter<AdminActionReply>>,
}

/// One request relayed to the consumer and the wait for its reply: the exchange every handler
/// above makes, written once. The failures it can end in are already the responses to send.
/// `what` names the exchange in the log; only identifiers and outcomes are logged, never what the
/// request or reply carries.
async fn relay<Req, Rep>(
    broker: &BrokerHandle,
    router: &Arc<ReplyRouter<Rep>>,
    topic: &str,
    what: &str,
    request: Req,
    timeout: Duration,
) -> Result<Rep, Response>
where
    Req: rkyv::Archive
        + for<'a> rkyv::Serialize<
            rkyv::api::high::HighSerializer<
                rkyv::util::AlignedVec,
                rkyv::ser::allocator::ArenaHandle<'a>,
                rkyv::rancor::Error,
            >,
        >,
{
    let correlation_id = uuid::Uuid::new_v4().to_string();
    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = match rkyv::to_bytes::<rkyv::rancor::Error>(&envelope) {
        Ok(bytes) => bytes.to_vec(),
        Err(e) => {
            error!(%correlation_id, "failed to serialize the {what} request: {e}");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response());
        }
    };

    info!(%correlation_id, %topic, "relaying a {what} request");
    let began = std::time::Instant::now();
    let outcome = router
        .register_with_timeout(correlation_id.clone(), timeout, || {
            broker
                .send_message(TcpClientMessage::publish(topic.to_string(), payload, None))
                .map_err(|e| e.to_string())
        })
        .await;

    match outcome {
        Ok(reply) => {
            info!(%correlation_id, elapsed_ms = began.elapsed().as_millis() as u64, "{what} reply received");
            Ok(reply)
        }
        Err(ReplyError::PublishFailed(e)) => {
            error!(%correlation_id, %topic, "failed to publish the {what} request: {e}");
            Err((StatusCode::SERVICE_UNAVAILABLE, "failed to reach the broker").into_response())
        }
        Err(ReplyError::ChannelClosed) => {
            warn!(%correlation_id, "the reply channel for the {what} request closed with no reply");
            Err((StatusCode::GATEWAY_TIMEOUT, "no reply received").into_response())
        }
        Err(ReplyError::Timeout) => {
            warn!(
                %correlation_id,
                %topic,
                waited_secs = timeout.as_secs(),
                "no reply to the {what} request: the consumer did not answer"
            );
            Err((StatusCode::GATEWAY_TIMEOUT, "timed out waiting for a reply").into_response())
        }
    }
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

    let timeout = compute_timeout(&request.credential);
    match relay(
        &state.broker_client,
        &state.issuance_reply_router,
        CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
        "issuance",
        request,
        timeout,
    )
    .await
    {
        Ok(reply) => reply_to_response(reply),
        Err(response) => response,
    }
}

pub async fn identity(
    State(state): State<Arc<ObserverState>>,
    Json(request): Json<IdentityNormalizationRequest>,
) -> Response {
    let timeout = compute_timeout(&request.credential);
    match relay(
        &state.broker_client,
        &state.identity_reply_router,
        CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC,
        "identity normalization",
        request,
        timeout,
    )
    .await
    {
        Ok(reply) => identity_reply_to_response(reply),
        Err(response) => response,
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
    match relay(
        &state.broker_client,
        &state.account_reply_router,
        CREDENTIALS_ACCOUNT_REQUESTS_TOPIC,
        "account",
        request,
        timeout,
    )
    .await
    {
        Ok(reply) => account_reply_to_response(reply),
        Err(response) => response,
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

/// POST /admin-action -- a signed admin action, relayed blindly. The action is
/// self-authenticating: the consumer looks the author up by certificate
/// serial, verifies the signature and checks the capability against the
/// ledger. The observer makes no trust decision and needs to know nothing
/// about who is calling, which is why a laptop with no Cassini identity can
/// use it.
pub async fn admin_action(
    State(state): State<Arc<ObserverState>>,
    Json(signed): Json<SignedAdminAction>,
) -> Response {
    // No credential to read a lifetime from, so wait the full ceiling.
    let timeout = Duration::from_secs(MAX_TIMEOUT_SECS as u64);
    match relay(
        &state.broker_client,
        &state.admin_action_reply_router,
        CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC,
        "admin action",
        signed,
        timeout,
    )
    .await
    {
        Ok(reply) => admin_action_reply_to_response(reply),
        Err(response) => response,
    }
}

fn admin_action_reply_to_response(reply: AdminActionReply) -> Response {
    let status = match &reply {
        AdminActionReply::Recorded => StatusCode::OK,
        AdminActionReply::Failed { .. } => StatusCode::SERVICE_UNAVAILABLE,
        AdminActionReply::Refused { reason } => match reason {
            AdminActionRefusal::UnknownAuthor | AdminActionRefusal::BadSignature => {
                StatusCode::UNAUTHORIZED
            }
            AdminActionRefusal::AuthorRevoked
            | AdminActionRefusal::MissingCapability { .. }
            | AdminActionRefusal::ReservedUsername
            | AdminActionRefusal::BootstrapCertificateSpent => StatusCode::FORBIDDEN,
            AdminActionRefusal::MalformedPayload => StatusCode::BAD_REQUEST,
            AdminActionRefusal::Replay
            | AdminActionRefusal::AccountAlreadyExists
            | AdminActionRefusal::AccountAlreadyPending => StatusCode::CONFLICT,
            AdminActionRefusal::NotYetSupported => StatusCode::NOT_IMPLEMENTED,
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
            broker_client: broker.into(),
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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
            broker_client: broker.into(),
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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
            broker_client: broker.into(),
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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
            broker_client: broker.into(),
            issuance_reply_router: reply_router,
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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
            broker_client: broker.into(),
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router,
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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
            broker_client: broker.into(),
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router,
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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
            broker_client: broker.into(),
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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
            broker_client: broker.into(),
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router,
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
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

#[cfg(test)]
mod admin_action_status_tests {
    use super::*;

    #[test]
    fn each_admin_action_reply_maps_to_its_status() {
        let refused = |reason| AdminActionReply::Refused { reason };
        for (reply, expected) in [
            (AdminActionReply::Recorded, StatusCode::OK),
            (
                AdminActionReply::Failed { detail: "x".to_string() },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (refused(AdminActionRefusal::UnknownAuthor), StatusCode::UNAUTHORIZED),
            (refused(AdminActionRefusal::BadSignature), StatusCode::UNAUTHORIZED),
            (refused(AdminActionRefusal::AuthorRevoked), StatusCode::FORBIDDEN),
            (refused(AdminActionRefusal::ReservedUsername), StatusCode::FORBIDDEN),
            (
                refused(AdminActionRefusal::BootstrapCertificateSpent),
                StatusCode::FORBIDDEN,
            ),
            (refused(AdminActionRefusal::MalformedPayload), StatusCode::BAD_REQUEST),
            (refused(AdminActionRefusal::Replay), StatusCode::CONFLICT),
            (refused(AdminActionRefusal::AccountAlreadyExists), StatusCode::CONFLICT),
            (refused(AdminActionRefusal::AccountAlreadyPending), StatusCode::CONFLICT),
            (refused(AdminActionRefusal::NotYetSupported), StatusCode::NOT_IMPLEMENTED),
        ] {
            let label = format!("{reply:?}");
            assert_eq!(
                admin_action_reply_to_response(reply).status(),
                expected,
                "for {label}"
            );
        }
    }
}

#[cfg(test)]
mod broker_handle_tests {
    use super::*;
    use ractor::{Actor, ActorProcessingErr, async_trait};

    /// Records the topic of every publish it receives, so a test can see
    /// which of two clients a BrokerHandle actually sent to.
    struct RecordingClient {
        seen: tokio::sync::mpsc::UnboundedSender<String>,
    }

    #[async_trait]
    impl Actor for RecordingClient {
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
            if let TcpClientMessage::Publish { topic, .. } = message {
                let _ = self.seen.send(topic);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn broker_handle_sends_to_whichever_client_it_currently_holds() {
        let (tx_old, mut rx_old) = tokio::sync::mpsc::unbounded_channel();
        let (tx_new, mut rx_new) = tokio::sync::mpsc::unbounded_channel();
        let (old, _) = Actor::spawn(None, RecordingClient { seen: tx_old }, ())
            .await
            .unwrap();
        let (new, _) = Actor::spawn(None, RecordingClient { seen: tx_new }, ())
            .await
            .unwrap();

        let handle = BrokerHandle::new(old);
        assert!(
            handle
                .send_message(TcpClientMessage::publish(
                    "before".to_string(),
                    vec![],
                    None
                ))
                .is_ok()
        );

        // What the supervisor does when the agent builder respawns the
        // TCP client: every later publish must reach the fresh one.
        handle.replace(new);
        assert!(
            handle
                .send_message(TcpClientMessage::publish("after".to_string(), vec![], None))
                .is_ok()
        );

        assert_eq!(rx_old.recv().await.as_deref(), Some("before"));
        assert_eq!(rx_new.recv().await.as_deref(), Some("after"));
        assert!(
            rx_old.try_recv().is_err(),
            "the replaced client must not receive anything sent after the swap"
        );
        assert!(
            rx_new.try_recv().is_err(),
            "the new client must not receive anything sent before the swap"
        );
    }
}
