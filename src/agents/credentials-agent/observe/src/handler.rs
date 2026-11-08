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
use credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_ISSUANCE_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC;
use credentials_agent_common::account_claim::{
    AccountRefusal, AccountReply, AccountRequest, Claimant,
};
use credentials_agent_common::admin_action::{
    AdminActionRefusal, AdminActionReply, SignedAdminAction,
};
use credentials_agent_common::agent_escrow_key::{AgentEscrowKeyReply, AgentEscrowKeyRequest};
use credentials_agent_common::identity::{
    IdentityNormalizationRefusalReason, IdentityNormalizationReply, IdentityNormalizationRequest,
};
use credentials_agent_common::issuance::{IssuanceRefusalReason, IssuanceReply, IssuanceRequest};
use credentials_agent_common::observer::ObserverRejection;
use credentials_agent_common::workload_issuance::{
    SealedIssuanceRequest, WorkloadIssuanceReply, WorkloadIssuanceRequest,
};
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
    pub workload_issuance_reply_router: Arc<ReplyRouter<WorkloadIssuanceReply>>,
    pub agent_escrow_key_reply_router: Arc<ReplyRouter<AgentEscrowKeyReply>>,
}

/// One request relayed to the consumer and the wait for its reply: the exchange every handler
/// above makes, written once. The failures it can end in are already the responses to send.
/// `what` names the exchange in the log; only identifiers and outcomes are logged, never what the
/// request or reply carries.
// The error is the HTTP response the route returns, built once a request, so its size is not worth boxing it.
#[allow(clippy::result_large_err)]
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
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "failed to reach the broker",
            )
                .into_response())
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

/// How long a workload waits for the credentials agent to open a session: no token is involved and nothing is
/// signed, so it is quick or it is not coming.
const SESSION_TIMEOUT: Duration = Duration::from_secs(10);

/// `POST /workload/session`: a workload, before it has any certificate, asks for a session to seal its token to.
/// The request carries nothing; the credentials agent's consumer answers with the public half of a fresh key.
pub async fn workload_session(State(state): State<Arc<ObserverState>>) -> Response {
    match relay(
        &state.broker_client,
        &state.workload_issuance_reply_router,
        CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
        "workload session",
        WorkloadIssuanceRequest::OpenSession,
        SESSION_TIMEOUT,
    )
    .await
    {
        Ok(WorkloadIssuanceReply::Offer(offer)) => (StatusCode::OK, Json(offer)).into_response(),
        Ok(WorkloadIssuanceReply::Busy) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no session can be offered just now: too many are open, or the offer cannot be signed yet",
        )
            .into_response(),
        Ok(_) => {
            error!("the consumer answered a session request with something else");
            (StatusCode::BAD_GATEWAY, "unexpected reply").into_response()
        }
        Err(response) => response,
    }
}

/// `POST /workload/issue`: a workload asks for a certificate with its token sealed to a session. The observer
/// checks only what is public (the CSR is a CSR) and relays the rest as it arrived: it cannot open the seal and
/// never holds the token. The consumer decides everything, as for `/issue`.
pub async fn workload_issue(
    State(state): State<Arc<ObserverState>>,
    Json(request): Json<SealedIssuanceRequest>,
) -> Response {
    if let Err(e) = cert_issuer::csr::parse_csr(&request.csr_pem) {
        let rejection = ObserverRejection::MalformedCsr(e.to_string());
        return (StatusCode::BAD_REQUEST, Json(rejection)).into_response();
    }

    // The token is sealed, so its expiry cannot be read to size the wait: the ceiling applies.
    let timeout = Duration::from_secs(MAX_TIMEOUT_SECS as u64);
    match relay(
        &state.broker_client,
        &state.workload_issuance_reply_router,
        CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
        "workload issuance",
        WorkloadIssuanceRequest::Issue(request),
        timeout,
    )
    .await
    {
        Ok(WorkloadIssuanceReply::Issuance(reply)) => reply_to_response(reply),
        // The session is not there any more; the workload opens another.
        Ok(WorkloadIssuanceReply::UnknownSession) => (
            StatusCode::GONE,
            Json(serde_json::json!({ "reason": "unknown_session" })),
        )
            .into_response(),
        Ok(WorkloadIssuanceReply::Busy) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "too many sessions are open",
        )
            .into_response(),
        Ok(WorkloadIssuanceReply::Offer(_)) => {
            error!("the consumer answered a sealed request with a session offer");
            (StatusCode::BAD_GATEWAY, "unexpected reply").into_response()
        }
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

/// Longest identity the agent escrow-key route will relay.
const MAX_IDENTITY: usize = 253;

/// How long the route waits for the consumer: it reads the ledger and nothing else.
const AGENT_ESCROW_KEY_TIMEOUT: Duration = Duration::from_secs(30);

/// Why `identity` is not a name worth asking the consumer about, if it is not. The observer's own
/// well-formedness check and never a trust decision: letters, digits, dots, hyphens and underscores, as every
/// agent identity is written.
fn identity_problem(identity: &str) -> Option<&'static str> {
    if identity.is_empty() || identity.len() > MAX_IDENTITY {
        Some("the identity must be 1 to 253 characters")
    } else if !identity
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        Some("the identity may have only letters, digits, dots, hyphens and underscores")
    } else {
        None
    }
}

/// POST /agent-escrow-key -- an agent's escrow certificate, for someone outside the cluster who needs to
/// encrypt to that agent (`polar-sign`). Takes no authentication: the gateway is plain HTTP and cannot see a
/// client certificate, and the answer is a public certificate. The caller does the protecting, by checking that
/// the certificate chains to the CA it trusts and names the agent it asked for
/// (docs/architecture/signed-configuration.md, "The tool needs the recipient's public key").
pub async fn agent_escrow_key(
    State(state): State<Arc<ObserverState>>,
    Json(request): Json<AgentEscrowKeyRequest>,
) -> Response {
    if let Some(problem) = identity_problem(&request.identity) {
        return (StatusCode::BAD_REQUEST, problem).into_response();
    }
    match relay(
        &state.broker_client,
        &state.agent_escrow_key_reply_router,
        CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
        "agent escrow key",
        request,
        AGENT_ESCROW_KEY_TIMEOUT,
    )
    .await
    {
        Ok(reply) => agent_escrow_key_reply_to_response(reply),
        Err(response) => response,
    }
}

fn agent_escrow_key_reply_to_response(reply: AgentEscrowKeyReply) -> Response {
    let status = match &reply {
        AgentEscrowKeyReply::Found { .. } => StatusCode::OK,
        AgentEscrowKeyReply::NotFound => StatusCode::NOT_FOUND,
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

    let computed_secs = (remaining / 3).clamp(1, MAX_TIMEOUT_SECS);
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
            IssuanceRefusalReason::RequestNotPermitted => StatusCode::FORBIDDEN,
            IssuanceRefusalReason::TooSoon => StatusCode::TOO_MANY_REQUESTS,
            // A custodian's answers, which come over Cassini and never through the observer; mapped all the same.
            IssuanceRefusalReason::NotACustodian | IssuanceRefusalReason::CustodianWindowClosed => {
                StatusCode::FORBIDDEN
            }
        },
    };
    (status, Json(reply)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Json, State};

    // docs/architecture/deployments.md, "The renewal rule": too soon is an answer to wait out, said as one, and the
    // reply goes with it, since the client reads the reply whatever the status.
    #[tokio::test]
    async fn too_soon_is_too_many_requests_and_carries_the_reply() {
        let response = reply_to_response(IssuanceReply::Refused {
            reason: IssuanceRefusalReason::TooSoon,
        });
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(matches!(
            serde_json::from_slice::<IssuanceReply>(&body).unwrap(),
            IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TooSoon
            }
        ));
    }
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

    pub(super) fn well_formed_csr() -> String {
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = issue(
            State(state),
            Json(IssuanceRequest {
                csr_pem: "not a real CSR".to_string(),
                credential: "irrelevant".to_string(),
                cert_type: Default::default(),
                names: vec![],
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = issue(
            State(state),
            Json(IssuanceRequest {
                csr_pem: well_formed_csr(),
                credential: "any-credential".to_string(),
                cert_type: Default::default(),
                names: vec![],
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
        });

        let response = issue(
            State(state),
            Json(IssuanceRequest {
                csr_pem: well_formed_csr(),
                credential: "any-credential".to_string(),
                cert_type: Default::default(),
                names: vec![],
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
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
                    cert_type: Default::default(),
                    names: vec![],
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
                && let Ok(envelope) = rkyv::from_bytes::<
                    Envelope<IdentityNormalizationRequest>,
                    rkyv::rancor::Error,
                >(&payload)
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
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
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
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
            (
                AccountRefusal::NoMatchingProvisioning,
                StatusCode::FORBIDDEN,
            ),
            (
                AccountRefusal::NotTheBootstrapIdentity,
                StatusCode::FORBIDDEN,
            ),
            (AccountRefusal::BootstrapAlreadyDone, StatusCode::FORBIDDEN),
            (AccountRefusal::CsrInvalid, StatusCode::BAD_REQUEST),
            (
                AccountRefusal::SigningFailed,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
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
                AdminActionReply::Failed {
                    detail: "x".to_string(),
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                refused(AdminActionRefusal::UnknownAuthor),
                StatusCode::UNAUTHORIZED,
            ),
            (
                refused(AdminActionRefusal::BadSignature),
                StatusCode::UNAUTHORIZED,
            ),
            (
                refused(AdminActionRefusal::AuthorRevoked),
                StatusCode::FORBIDDEN,
            ),
            (
                refused(AdminActionRefusal::ReservedUsername),
                StatusCode::FORBIDDEN,
            ),
            (
                refused(AdminActionRefusal::BootstrapCertificateSpent),
                StatusCode::FORBIDDEN,
            ),
            (
                refused(AdminActionRefusal::MalformedPayload),
                StatusCode::BAD_REQUEST,
            ),
            (refused(AdminActionRefusal::Replay), StatusCode::CONFLICT),
            (
                refused(AdminActionRefusal::AccountAlreadyExists),
                StatusCode::CONFLICT,
            ),
            (
                refused(AdminActionRefusal::AccountAlreadyPending),
                StatusCode::CONFLICT,
            ),
            (
                refused(AdminActionRefusal::NotYetSupported),
                StatusCode::NOT_IMPLEMENTED,
            ),
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

#[cfg(test)]
mod workload_tests {
    use super::tests::well_formed_csr;
    use super::*;
    use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};

    // ---- a workload's own sealed request -------------------------------------------------------------------

    use credentials_agent_common::ephemeral_exchange::{
        EphemeralEncryptedPayload, EphemeralSession,
    };
    use credentials_agent_common::issuance::WorkloadCertType;
    use credentials_agent_common::workload_issuance::SessionOffer;

    /// A broker that records the workload requests it is asked to relay and answers each with a canned reply.
    struct WorkloadBroker {
        reply_router: Arc<ReplyRouter<WorkloadIssuanceReply>>,
        canned_reply: WorkloadIssuanceReply,
        relayed: Arc<std::sync::Mutex<Vec<WorkloadIssuanceRequest>>>,
    }

    #[async_trait]
    impl Actor for WorkloadBroker {
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
            if let TcpClientMessage::Publish { topic, payload, .. } = message {
                assert_eq!(topic, CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC);
                let envelope = rkyv::from_bytes::<
                    Envelope<WorkloadIssuanceRequest>,
                    rkyv::rancor::Error,
                >(&payload)
                .expect("the observer relays a workload request");
                self.relayed.lock().unwrap().push(envelope.payload);
                self.reply_router
                    .deliver(&envelope.correlation_id, self.canned_reply.clone());
            }
            Ok(())
        }
    }

    async fn workload_state(
        canned_reply: WorkloadIssuanceReply,
    ) -> (
        Arc<ObserverState>,
        Arc<std::sync::Mutex<Vec<WorkloadIssuanceRequest>>>,
    ) {
        let router = Arc::new(ReplyRouter::new());
        let relayed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker, _) = Actor::spawn(
            None,
            WorkloadBroker {
                reply_router: router.clone(),
                canned_reply,
                relayed: relayed.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let state = Arc::new(ObserverState {
            broker_client: broker.into(),
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
            workload_issuance_reply_router: router,
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
        });
        (state, relayed)
    }

    fn an_offer() -> SessionOffer {
        SessionOffer {
            session_id: "session-1".to_string(),
            public_key: EphemeralSession::open().public_key(),
            valid_for_secs: 60,
            // Not checked on the way: carried, whole.
            signature: credentials_agent_common::session_offer::OfferSignature {
                algorithm: "ed25519".to_string(),
                value: vec![1, 2, 3],
                certificate_chain: vec!["the signer's certificate".to_string()],
            },
        }
    }

    fn a_sealed_request(offer: &SessionOffer, csr_pem: String) -> SealedIssuanceRequest {
        SealedIssuanceRequest {
            session_id: offer.session_id.clone(),
            csr_pem,
            cert_type: WorkloadCertType::Client,
            names: vec![],
            sealed_credential: EphemeralEncryptedPayload::encrypt(
                b"a service-account token",
                &offer.public_key,
            )
            .unwrap(),
        }
    }

    async fn body_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn asking_for_a_session_relays_an_open_and_returns_the_offer() {
        let offer = an_offer();
        let (state, relayed) = workload_state(WorkloadIssuanceReply::Offer(offer.clone())).await;
        let response = workload_session(State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: SessionOffer = serde_json::from_str(&body_of(response).await).unwrap();
        assert_eq!(body, offer);
        assert!(matches!(
            relayed.lock().unwrap().as_slice(),
            [WorkloadIssuanceRequest::OpenSession]
        ));
    }

    #[tokio::test]
    async fn a_full_table_of_sessions_is_unavailable_and_not_an_error() {
        let (state, _) = workload_state(WorkloadIssuanceReply::Busy).await;
        assert_eq!(
            workload_session(State(state)).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn a_sealed_request_is_relayed_exactly_as_it_arrived_and_its_reply_is_the_usual_one() {
        let (state, relayed) =
            workload_state(WorkloadIssuanceReply::Issuance(IssuanceReply::Issued {
                certificate_pem: "cert".to_string(),
                ca_chain_pem: "chain".to_string(),
                escrow_public_key_pem: "key".to_string(),
            }))
            .await;
        let request = a_sealed_request(&an_offer(), well_formed_csr());
        let response = workload_issue(State(state), Json(request.clone())).await;
        assert_eq!(response.status(), StatusCode::OK);
        match relayed.lock().unwrap().as_slice() {
            [WorkloadIssuanceRequest::Issue(relayed)] => {
                assert_eq!(relayed.session_id, request.session_id);
                assert_eq!(relayed.csr_pem, request.csr_pem);
                assert_eq!(relayed.sealed_credential, request.sealed_credential);
            }
            other => panic!("expected one relayed Issue, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_refusal_is_mapped_as_it_is_for_the_clear_route() {
        use credentials_agent_common::issuance::IssuanceRefusalReason::*;
        for (reason, status) in [
            (TokenInvalid, StatusCode::UNAUTHORIZED),
            (AlreadyHasValidCertificate, StatusCode::FORBIDDEN),
            (RequestNotPermitted, StatusCode::FORBIDDEN),
            (SigningFailed, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let (state, _) =
                workload_state(WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                    reason,
                }))
                .await;
            let response = workload_issue(
                State(state),
                Json(a_sealed_request(&an_offer(), well_formed_csr())),
            )
            .await;
            assert_eq!(response.status(), status);
        }
    }

    #[tokio::test]
    async fn a_session_that_is_gone_is_gone() {
        let (state, _) = workload_state(WorkloadIssuanceReply::UnknownSession).await;
        let response = workload_issue(
            State(state),
            Json(a_sealed_request(&an_offer(), well_formed_csr())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::GONE);
        assert!(body_of(response).await.contains("unknown_session"));
    }

    #[tokio::test]
    async fn a_csr_that_is_not_one_is_turned_away_before_anything_is_relayed() {
        let (state, relayed) = workload_state(WorkloadIssuanceReply::UnknownSession).await;
        let response = workload_issue(
            State(state),
            Json(a_sealed_request(&an_offer(), "not a csr".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(relayed.lock().unwrap().is_empty());
    }

    struct FakeKeyBroker {
        reply_router: Arc<ReplyRouter<AgentEscrowKeyReply>>,
        canned_reply: AgentEscrowKeyReply,
        asked: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    #[async_trait]
    impl Actor for FakeKeyBroker {
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
            if let TcpClientMessage::Publish { topic, payload, .. } = message
                && let Ok(envelope) = rkyv::from_bytes::<
                    Envelope<AgentEscrowKeyRequest>,
                    rkyv::rancor::Error,
                >(&payload)
            {
                self.asked
                    .lock()
                    .unwrap()
                    .push((topic, envelope.payload.identity));
                self.reply_router
                    .deliver(&envelope.correlation_id, self.canned_reply.clone());
            }
            Ok(())
        }
    }

    type Asked = Arc<std::sync::Mutex<Vec<(String, String)>>>;

    async fn key_state(canned_reply: AgentEscrowKeyReply) -> (Arc<ObserverState>, Asked) {
        let router = Arc::new(ReplyRouter::new());
        let asked: Asked = Arc::default();
        let (broker, _) = Actor::spawn(
            None,
            FakeKeyBroker {
                reply_router: router.clone(),
                canned_reply,
                asked: asked.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let state = Arc::new(ObserverState {
            broker_client: broker.into(),
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: router,
        });
        (state, asked)
    }

    async fn json_of(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn an_agents_escrow_certificate_is_returned_with_200_and_asked_for_by_identity() {
        let (state, asked) = key_state(AgentEscrowKeyReply::Found {
            public_key_pem: "KEY".to_string(),
            certificate_pem: "CERT".to_string(),
        })
        .await;
        let response = agent_escrow_key(
            State(state),
            Json(AgentEscrowKeyRequest {
                identity: "git-repo-observer.polar.serviceaccount.cluster.local".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_of(response).await;
        assert_eq!(body["Found"]["certificate_pem"], "CERT");
        assert_eq!(body["Found"]["public_key_pem"], "KEY");
        assert_eq!(
            *asked.lock().unwrap(),
            vec![(
                CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC.to_string(),
                "git-repo-observer.polar.serviceaccount.cluster.local".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn an_agent_with_no_escrow_certificate_is_404() {
        let (state, _) = key_state(AgentEscrowKeyReply::NotFound).await;
        let response = agent_escrow_key(
            State(state),
            Json(AgentEscrowKeyRequest {
                identity: "nobody".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_identity_that_is_not_a_name_is_refused_before_the_consumer_is_asked() {
        let (state, asked) = key_state(AgentEscrowKeyReply::NotFound).await;
        let too_long = "a".repeat(MAX_IDENTITY + 1);
        for identity in [
            "",
            "has space",
            "slash/x",
            "pipe|x",
            "new\nline",
            "é",
            &too_long,
        ] {
            let response = agent_escrow_key(
                State(state.clone()),
                Json(AgentEscrowKeyRequest {
                    identity: identity.to_string(),
                }),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{identity:?}");
        }
        assert!(asked.lock().unwrap().is_empty());
        // and the longest allowed, and the usual forms, are relayed
        for identity in [
            "a".repeat(MAX_IDENTITY),
            "git-repo-observer".to_string(),
            "a_b.c-d".to_string(),
        ] {
            let response = agent_escrow_key(
                State(state.clone()),
                Json(AgentEscrowKeyRequest { identity }),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        assert_eq!(asked.lock().unwrap().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn no_reply_from_the_consumer_is_a_gateway_timeout() {
        let router = Arc::new(ReplyRouter::new());
        let (broker, _) = Actor::spawn(
            None,
            FakeKeyBroker {
                reply_router: Arc::new(ReplyRouter::new()), // delivers to a router nobody waits on
                canned_reply: AgentEscrowKeyReply::NotFound,
                asked: Arc::default(),
            },
            (),
        )
        .await
        .unwrap();
        let state = Arc::new(ObserverState {
            broker_client: broker.into(),
            issuance_reply_router: Arc::new(ReplyRouter::new()),
            identity_reply_router: Arc::new(ReplyRouter::new()),
            account_reply_router: Arc::new(ReplyRouter::new()),
            admin_action_reply_router: Arc::new(ReplyRouter::new()),
            workload_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: router,
        });
        let response = agent_escrow_key(
            State(state),
            Json(AgentEscrowKeyRequest {
                identity: "git-repo-observer".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    /// One raw HTTP exchange with the routes the pod serves, over a real socket.
    async fn http(state: Arc<ObserverState>, request: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = crate::supervisor::routes(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        server.abort();
        response
    }

    fn post(path: &str, body: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn the_pod_serves_the_agent_escrow_key_route_to_a_plain_unauthenticated_post() {
        let (state, _) = key_state(AgentEscrowKeyReply::Found {
            public_key_pem: "KEY".to_string(),
            certificate_pem: "CERT-PEM".to_string(),
        })
        .await;
        let response = http(
            state.clone(),
            &post("/agent-escrow-key", r#"{"identity":"git-repo-observer"}"#),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("CERT-PEM"), "{response}");
        // a route that is not served is not found, and the new one is not a GET
        let missing = http(state.clone(), &post("/agent-escrow-keys", "{}")).await;
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
        let get = http(
            state,
            "GET /agent-escrow-key HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(get.starts_with("HTTP/1.1 405"), "{get}");
    }
}
