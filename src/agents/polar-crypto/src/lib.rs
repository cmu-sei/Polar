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

//! polar-crypto
//!
//! The process-local, single gateway to the Credentials Agent for
//! every other actor in an agent's own process -- see
//! credentials-agent-architecture.md's own "CryptoActor" section for
//! the full design. Mirrors polar-health's own, already-established
//! pattern: a dedicated, standalone crate any agent depends on and
//! `spawn_linked`s directly within its own supervisor, sharing that
//! agent's existing TcpClientActor rather than opening a second
//! Cassini connection.
//!
//! Scope so far: general-secret store, and the retrieve half of the
//! agent escrow-keypair bootstrap flow. What's still deliberately
//! deferred: the issuance fallback for when RetrieveOwnEscrowKeypair
//! comes back None (no keypair has ever been escrowed for this
//! identity yet), escrowing that issuance's own result afterward, and
//! kernel-keyring custody -- see credentials-agent-architecture.md's
//! own "Agent escrow bootstrap" section for the full, intended
//! sequence this crate is still working toward.

use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use cert_issuer_common::keypair::{KeyAlgorithm, KeypairError};
use credentials_agent_common::agent_escrow_issuance::{
    AgentEscrowIssuanceReply, AgentEscrowIssuanceRequest,
};
use credentials_agent_common::agent_escrow_keypair_bootstrap::{
    RetrieveAgentEscrowKeypairReply, RetrieveAgentEscrowKeypairRequest,
    agent_escrow_keypair_secret_name,
};
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use credentials_agent_common::escrow_secret::EscrowedSecret;
use credentials_agent_common::secret::{StoreRefusalReason, StoreSecretReply, StoreSecretRequest};
use credentials_agent_common::{
    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC, CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC,
    CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC, CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC, CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
    CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
};
use polar::request_reply::{Envelope, ReplyRouter};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, async_trait};
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, warn};
use x25519_dalek::{EphemeralSecret, PublicKey};

/// The standard, in-cluster path for a pod's own, Kubernetes-mounted
/// service-account token -- mirrors cert-issuer/server's own oidc.rs,
/// the existing, established pattern for this in this codebase.
/// Overridable via KUBERNETES_SA_TOKEN, matching that same precedent.
const KUBERNETES_SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// How long CryptoActor waits for a single correlated reply from the
/// Credentials Agent before giving up -- generous relative to what a
/// live broker round trip should ever actually take, matching the
/// same conservative-but-bounded spirit as this codebase's other
/// request/reply timeouts elsewhere.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("failed to reach the broker: {0}")]
    BrokerUnreachable(String),
    #[error("timed out waiting for a reply from the Credentials Agent")]
    Timeout,
    #[error("reply channel closed with no reply")]
    NoReply,
    #[error("failed to serialize request: {0}")]
    Serialization(String),
    #[error("encryption failed: {0}")]
    Encryption(#[from] credentials_agent_common::escrow_secret::EscrowError),
    #[error("store refused: {0:?}")]
    Refused(StoreRefusalReason),
    #[error("store request was valid but failed to persist: {0}")]
    StoreFailed(String),
    #[error("retrieve refused: {0:?}")]
    RetrieveRefused(
        credentials_agent_common::agent_escrow_keypair_bootstrap::RetrieveAgentEscrowKeypairRefusalReason,
    ),
    #[error("retrieve request was valid but failed: {0}")]
    RetrieveFailed(String),
    #[error("the ephemeral key exchange itself failed: {0}")]
    EphemeralExchange(#[from] credentials_agent_common::ephemeral_exchange::EphemeralExchangeError),
    #[error("the retrieved escrow keypair's own private-key PEM failed to parse: {0}")]
    InvalidRetrievedKey(String),
    #[error("failed to read this pod's own Kubernetes service-account token: {0}")]
    CredentialUnavailable(String),
    #[error("CSR generation failed: {0}")]
    CsrGeneration(#[from] KeypairError),
    #[error("agent escrow-identity issuance refused: {0:?}")]
    IssuanceRefused(
        credentials_agent_common::agent_escrow_issuance::AgentEscrowIssuanceRefusalReason,
    ),
    #[error("failed to encode the newly-issued private key as PKCS8 PEM: {0}")]
    KeyEncoding(String),
}

pub enum CryptoMessage {
    /// A general secret to escrow -- e.g. an access token an observer
    /// holds and wants the Credentials Agent to store on its behalf.
    /// Not the agent's own escrow private key -- see
    /// RetrieveOwnEscrowKeypair, below, for that flow.
    StoreSecret {
        name: String,
        plaintext: Vec<u8>,
        reply: RpcReplyPort<Result<(), CryptoError>>,
    },
    /// The retrieve half of the agent escrow-keypair bootstrap flow --
    /// see credentials-agent-architecture.md's own "Agent escrow
    /// bootstrap" section. `None` means no keypair has ever been
    /// escrowed for this identity yet (a genuine absence, not an
    /// error) -- the caller's own signal to fall back to issuance.
    /// That fallback itself, and escrowing its own result afterward,
    /// isn't built yet; this variant only covers the retrieve half.
    RetrieveOwnEscrowKeypair {
        reply: RpcReplyPort<Result<Option<RsaPrivateKey>, CryptoError>>,
    },
    /// The complete agent escrow-keypair bootstrap sequence --
    /// retrieve-first, issuance-as-fallback, per
    /// credentials-agent-architecture.md's own "Agent escrow
    /// bootstrap" section. Tries RetrieveOwnEscrowKeypair first;
    /// if that genuinely comes back None (no keypair has ever been
    /// escrowed for this identity), falls back to real issuance
    /// (reading this pod's own Kubernetes service-account token,
    /// generating a fresh RSA CSR, and publishing a real
    /// AgentEscrowIssuanceRequest), then escrows the newly-issued
    /// private key under the same, well-known name the retrieve
    /// path looks it up under -- so the next call, from any pod
    /// instance, genuinely finds it via the ordinary retrieve path
    /// instead of re-issuing. Always returns a real, usable
    /// RsaPrivateKey on success -- never Ok(None), unlike
    /// RetrieveOwnEscrowKeypair alone, since this variant's whole
    /// purpose is guaranteeing one exists by the time it returns.
    EnsureOwnEscrowKeypair {
        reply: RpcReplyPort<Result<RsaPrivateKey, CryptoError>>,
    },
    /// Internal: a ClientEvent forwarded from the shared
    /// TcpClientActor's own OutputPort, filtered to just the topics
    /// this actor cares about.
    ClientEvent(ClientEvent),
}

pub struct CryptoActorArgs {
    /// The consuming agent's own, already-spawned TcpClientActor --
    /// shared, not a second connection. See
    /// credentials-agent-architecture.md's own "CryptoActor" section
    /// for why this must stay fire-and-forget in both directions.
    pub tcp_client: ActorRef<TcpClientMessage>,
    /// The same OutputPort the agent's own TcpClientActor was
    /// constructed with -- CryptoActor subscribes to it too, so it
    /// can receive the replies its own requests are waiting on.
    pub events_output: Arc<OutputPort<ClientEvent>>,
    /// This agent's own, real workload identity -- required for
    /// EnsureOwnEscrowKeypair's own issuance-fallback path, since
    /// generate_csr needs a real, non-empty identity for the CSR's
    /// own SAN. The consuming supervisor already knows its own
    /// service-account identity; this is threaded in rather than
    /// derived here, since CryptoActor itself has no independent way
    /// to discover it.
    pub own_identity: String,
}

pub struct CryptoActorState {
    tcp_client: ActorRef<TcpClientMessage>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    retrieve_agent_escrow_keypair_reply_router: Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
    agent_escrow_issuance_reply_router: Arc<ReplyRouter<AgentEscrowIssuanceReply>>,
    /// The Credentials Agent's own current escrow public key, learned
    /// on first use and reused afterward -- avoids re-querying on
    /// every single store, per credentials-agent-architecture.md's
    /// own note on CryptoActor's local caching. Arc<Mutex<_>> rather
    /// than a plain field: the actual store work runs in a spawned
    /// task, not inline in handle() (see handle()'s own StoreSecret
    /// arm for why), so this needs to be mutable from outside the
    /// actor's own sequential mailbox loop.
    cached_credentials_agent_escrow_key: Arc<tokio::sync::Mutex<Option<String>>>,
    own_identity: String,
}

pub struct CryptoActor;

#[async_trait]
impl Actor for CryptoActor {
    type Msg = CryptoMessage;
    type State = CryptoActorState;
    type Arguments = CryptoActorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: CryptoActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        args.events_output.subscribe(myself.clone(), |event| {
            Some(CryptoMessage::ClientEvent(event))
        });

        // Subscribes to the *_REPLIES_TOPIC constants specifically --
        // CryptoActor is a requester here, not a responder, so it
        // needs the reply topics, not the request topics (easy to
        // transpose by accident, worth being explicit about).
        if let Err(e) = args.tcp_client.send_message(TcpClientMessage::Subscribe {
            topic: CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC.to_string(),
            trace_ctx: None,
        }) {
            error!("CryptoActor failed to subscribe to escrow-key replies: {e}");
        }
        if let Err(e) = args.tcp_client.send_message(TcpClientMessage::Subscribe {
            topic: CREDENTIALS_STORE_SECRET_REPLIES_TOPIC.to_string(),
            trace_ctx: None,
        }) {
            error!("CryptoActor failed to subscribe to store-secret replies: {e}");
        }
        if let Err(e) = args.tcp_client.send_message(TcpClientMessage::Subscribe {
            topic: CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC.to_string(),
            trace_ctx: None,
        }) {
            error!("CryptoActor failed to subscribe to agent escrow-keypair retrieve replies: {e}");
        }
        if let Err(e) = args.tcp_client.send_message(TcpClientMessage::Subscribe {
            topic: CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC.to_string(),
            trace_ctx: None,
        }) {
            error!("CryptoActor failed to subscribe to agent escrow-issuance replies: {e}");
        }

        Ok(CryptoActorState {
            tcp_client: args.tcp_client,
            escrow_key_reply_router: Arc::new(ReplyRouter::new()),
            store_secret_reply_router: Arc::new(ReplyRouter::new()),
            retrieve_agent_escrow_keypair_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_issuance_reply_router: Arc::new(ReplyRouter::new()),
            cached_credentials_agent_escrow_key: Arc::new(tokio::sync::Mutex::new(None)),
            own_identity: args.own_identity,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CryptoMessage::ClientEvent(ClientEvent::MessagePublished { topic, payload, .. })
                if topic == CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<credentials_agent_common::escrow_key::EscrowKeyReply>, rkyv::rancor::Error>(&payload)
                {
                    Ok(envelope) => state
                        .escrow_key_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!("CryptoActor failed to deserialize escrow-key reply: {e}"),
                }
            }
            CryptoMessage::ClientEvent(ClientEvent::MessagePublished { topic, payload, .. })
                if topic == CREDENTIALS_STORE_SECRET_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<StoreSecretReply>, rkyv::rancor::Error>(&payload)
                {
                    Ok(envelope) => state
                        .store_secret_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!("CryptoActor failed to deserialize store-secret reply: {e}"),
                }
            }
            CryptoMessage::ClientEvent(ClientEvent::MessagePublished { topic, payload, .. })
                if topic == CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<RetrieveAgentEscrowKeypairReply>, rkyv::rancor::Error>(&payload)
                {
                    Ok(envelope) => state
                        .retrieve_agent_escrow_keypair_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!(
                        "CryptoActor failed to deserialize agent escrow-keypair retrieve reply: {e}"
                    ),
                }
            }
            CryptoMessage::ClientEvent(ClientEvent::MessagePublished { topic, payload, .. })
                if topic == CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<AgentEscrowIssuanceReply>, rkyv::rancor::Error>(&payload)
                {
                    Ok(envelope) => state
                        .agent_escrow_issuance_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!("CryptoActor failed to deserialize agent escrow-issuance reply: {e}"),
                }
            }
            CryptoMessage::ClientEvent(_) => {
                // Some other topic's event, forwarded to every
                // subscriber of the shared OutputPort -- not ours,
                // ignored.
            }
            CryptoMessage::StoreSecret {
                name,
                plaintext,
                reply,
            } => {
                // Spawned, not awaited inline -- handle_store_secret
                // itself awaits replies from this same actor's own
                // mailbox (via the reply routers), which handle()
                // can't do without blocking the very mailbox those
                // replies need to be processed by. See this crate's
                // own README/commit history for the deadlock this
                // caused before being caught.
                let tcp_client = state.tcp_client.clone();
                let escrow_key_reply_router = state.escrow_key_reply_router.clone();
                let store_secret_reply_router = state.store_secret_reply_router.clone();
                let cached_key = state.cached_credentials_agent_escrow_key.clone();
                tokio::spawn(async move {
                    let result = handle_store_secret(
                        tcp_client,
                        escrow_key_reply_router,
                        store_secret_reply_router,
                        cached_key,
                        name,
                        plaintext,
                    )
                    .await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::RetrieveOwnEscrowKeypair { reply } => {
                // Same reasoning as StoreSecret's own arm above --
                // spawned, not awaited inline, since
                // handle_retrieve_own_escrow_keypair itself awaits a
                // reply from this same actor's own mailbox.
                let tcp_client = state.tcp_client.clone();
                let retrieve_agent_escrow_keypair_reply_router =
                    state.retrieve_agent_escrow_keypair_reply_router.clone();
                tokio::spawn(async move {
                    let result = handle_retrieve_own_escrow_keypair(
                        tcp_client,
                        retrieve_agent_escrow_keypair_reply_router,
                    )
                    .await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::EnsureOwnEscrowKeypair { reply } => {
                // Same reasoning as StoreSecret's own arm above --
                // spawned, not awaited inline.
                let tcp_client = state.tcp_client.clone();
                let retrieve_agent_escrow_keypair_reply_router =
                    state.retrieve_agent_escrow_keypair_reply_router.clone();
                let agent_escrow_issuance_reply_router =
                    state.agent_escrow_issuance_reply_router.clone();
                let escrow_key_reply_router = state.escrow_key_reply_router.clone();
                let store_secret_reply_router = state.store_secret_reply_router.clone();
                let cached_key = state.cached_credentials_agent_escrow_key.clone();
                let own_identity = state.own_identity.clone();
                tokio::spawn(async move {
                    let result = handle_ensure_own_escrow_keypair(
                        tcp_client,
                        retrieve_agent_escrow_keypair_reply_router,
                        agent_escrow_issuance_reply_router,
                        escrow_key_reply_router,
                        store_secret_reply_router,
                        cached_key,
                        own_identity,
                    )
                    .await;
                    let _ = reply.send(result);
                });
            }
        }
        Ok(())
    }
}

async fn handle_store_secret(
    tcp_client: ActorRef<TcpClientMessage>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    cached_key: Arc<tokio::sync::Mutex<Option<String>>>,
    name: String,
    plaintext: Vec<u8>,
) -> Result<(), CryptoError> {
    let escrow_key_pem = {
        let mut cached = cached_key.lock().await;
        match &*cached {
            Some(pem) => pem.clone(),
            None => {
                let pem = query_escrow_key(&tcp_client, &escrow_key_reply_router).await?;
                *cached = Some(pem.clone());
                pem
            }
        }
    };

    let secret = EscrowedSecret::encrypt(&plaintext, &escrow_key_pem)?;

    let reply = publish_store_secret_request(
        &tcp_client,
        &store_secret_reply_router,
        StoreSecretRequest { name, secret },
    )
    .await?;

    match reply {
        StoreSecretReply::Stored => Ok(()),
        StoreSecretReply::Refused { reason } => Err(CryptoError::Refused(reason)),
        StoreSecretReply::Failed { detail } => Err(CryptoError::StoreFailed(detail)),
    }
}

async fn query_escrow_key(
    tcp_client: &ActorRef<TcpClientMessage>,
    escrow_key_reply_router: &Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
) -> Result<String, CryptoError> {
    let correlation_id = uuid::Uuid::new_v4().to_string();

    // Registered before publishing -- a reply arriving in the gap
    // between publishing and registering would have nowhere to route.
    let rx = escrow_key_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: EscrowKeyRequest,
    };
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
        .map(|b| b.to_vec())
        .map_err(|e| {
            escrow_key_reply_router.cancel(&correlation_id);
            CryptoError::Serialization(e.to_string())
        })?;

    if let Err(e) = tcp_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        escrow_key_reply_router.cancel(&correlation_id);
        return Err(CryptoError::BrokerUnreachable(e.to_string()));
    }

    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(reply)) => Ok(reply.public_key_pem),
        Ok(Err(_)) => Err(CryptoError::NoReply),
        Err(_) => {
            escrow_key_reply_router.cancel(&correlation_id);
            Err(CryptoError::Timeout)
        }
    }
}

async fn publish_store_secret_request(
    tcp_client: &ActorRef<TcpClientMessage>,
    store_secret_reply_router: &Arc<ReplyRouter<StoreSecretReply>>,
    request: StoreSecretRequest,
) -> Result<StoreSecretReply, CryptoError> {
    let correlation_id = uuid::Uuid::new_v4().to_string();

    let rx = store_secret_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
        .map(|b| b.to_vec())
        .map_err(|e| {
            store_secret_reply_router.cancel(&correlation_id);
            CryptoError::Serialization(e.to_string())
        })?;

    if let Err(e) = tcp_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        store_secret_reply_router.cancel(&correlation_id);
        return Err(CryptoError::BrokerUnreachable(e.to_string()));
    }

    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(_)) => Err(CryptoError::NoReply),
        Err(_) => {
            store_secret_reply_router.cancel(&correlation_id);
            Err(CryptoError::Timeout)
        }
    }
}

/// The retrieve half of the agent escrow-keypair bootstrap flow --
/// see credentials-agent-architecture.md's own "Agent escrow
/// bootstrap" section. A fresh, single-use X25519 ephemeral keypair
/// is generated for this one call: its public half travels in the
/// request, and its private half is consumed decrypting the reply
/// (matching EphemeralSecret::diffie_hellman's own single-use
/// semantics -- once used, it's gone, by construction, not just by
/// convention).
async fn handle_retrieve_own_escrow_keypair(
    tcp_client: ActorRef<TcpClientMessage>,
    retrieve_agent_escrow_keypair_reply_router: Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
) -> Result<Option<RsaPrivateKey>, CryptoError> {
    let my_ephemeral_secret = EphemeralSecret::random();
    let my_ephemeral_public_key = PublicKey::from(&my_ephemeral_secret).to_bytes();

    let reply = publish_retrieve_agent_escrow_keypair_request(
        &tcp_client,
        &retrieve_agent_escrow_keypair_reply_router,
        RetrieveAgentEscrowKeypairRequest {
            ephemeral_public_key: my_ephemeral_public_key,
        },
    )
    .await?;

    match reply {
        RetrieveAgentEscrowKeypairReply::Found { payload } => {
            let decrypted_pem = payload.decrypt(my_ephemeral_secret)?;
            let private_key = RsaPrivateKey::from_pkcs8_pem(
                std::str::from_utf8(&decrypted_pem)
                    .map_err(|e| CryptoError::InvalidRetrievedKey(e.to_string()))?,
            )
            .map_err(|e| CryptoError::InvalidRetrievedKey(e.to_string()))?;
            Ok(Some(private_key))
        }
        RetrieveAgentEscrowKeypairReply::NotFound => Ok(None),
        RetrieveAgentEscrowKeypairReply::Refused { reason } => Err(CryptoError::RetrieveRefused(reason)),
        RetrieveAgentEscrowKeypairReply::Failed { detail } => Err(CryptoError::RetrieveFailed(detail)),
    }
}

async fn publish_retrieve_agent_escrow_keypair_request(
    tcp_client: &ActorRef<TcpClientMessage>,
    retrieve_agent_escrow_keypair_reply_router: &Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
    request: RetrieveAgentEscrowKeypairRequest,
) -> Result<RetrieveAgentEscrowKeypairReply, CryptoError> {
    let correlation_id = uuid::Uuid::new_v4().to_string();

    let rx = retrieve_agent_escrow_keypair_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
        .map(|b| b.to_vec())
        .map_err(|e| {
            retrieve_agent_escrow_keypair_reply_router.cancel(&correlation_id);
            CryptoError::Serialization(e.to_string())
        })?;

    if let Err(e) = tcp_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        retrieve_agent_escrow_keypair_reply_router.cancel(&correlation_id);
        return Err(CryptoError::BrokerUnreachable(e.to_string()));
    }

    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(_)) => Err(CryptoError::NoReply),
        Err(_) => {
            retrieve_agent_escrow_keypair_reply_router.cancel(&correlation_id);
            Err(CryptoError::Timeout)
        }
    }
}

/// Reads this pod's own, mounted Kubernetes service-account token --
/// mirrors cert-issuer/server's own oidc.rs (the established pattern
/// for this in this codebase): re-read on every call, never cached,
/// since Kubernetes rotates this token automatically and a cached
/// copy could genuinely be expired by the time it's used.
fn read_kubernetes_sa_token() -> Result<String, CryptoError> {
    let path = std::env::var("KUBERNETES_SA_TOKEN")
        .unwrap_or_else(|_| KUBERNETES_SA_TOKEN_PATH.to_string());
    std::fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .map_err(|e| CryptoError::CredentialUnavailable(format!("{path}: {e}")))
}

/// The complete agent escrow-keypair bootstrap sequence -- see
/// CryptoMessage::EnsureOwnEscrowKeypair's own doc comment for the
/// full, intended behavior.
async fn handle_ensure_own_escrow_keypair(
    tcp_client: ActorRef<TcpClientMessage>,
    retrieve_agent_escrow_keypair_reply_router: Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
    agent_escrow_issuance_reply_router: Arc<ReplyRouter<AgentEscrowIssuanceReply>>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    cached_key: Arc<tokio::sync::Mutex<Option<String>>>,
    own_identity: String,
) -> Result<RsaPrivateKey, CryptoError> {
    // Step 1: retrieve-first. A real Err here (broker unreachable,
    // timeout, a genuine refusal) propagates directly -- issuance is
    // only ever attempted when retrieve specifically, cleanly came
    // back "nothing escrowed yet," not as a fallback for every other
    // kind of failure too.
    if let Some(existing_key) = handle_retrieve_own_escrow_keypair(
        tcp_client.clone(),
        retrieve_agent_escrow_keypair_reply_router,
    )
    .await?
    {
        return Ok(existing_key);
    }

    // Step 2: issuance fallback. Read this pod's own, real Kubernetes
    // credential -- the same one cert-issuer's own oidc.rs already
    // knows how to validate on the receiving end.
    let credential = read_kubernetes_sa_token()?;

    // Step 3: generate a real CSR, RSA specifically -- this is the
    // agent's own encryption-capable escrow identity, not a
    // signing-only mTLS credential. See KeyAlgorithm::Rsa's own doc
    // comment (cert-issuer-common) for the full reasoning.
    let csr_output = cert_issuer_common::keypair::generate_csr(
        &own_identity,
        &KeyAlgorithm::Rsa,
        &[],
    )?;

    // Step 4: the actual issuance request/reply round trip.
    let issuance_reply = publish_agent_escrow_issuance_request(
        &tcp_client,
        &agent_escrow_issuance_reply_router,
        AgentEscrowIssuanceRequest {
            csr_pem: csr_output.csr_pem,
            credential,
        },
    )
    .await?;

    let AgentEscrowIssuanceReply::Issued { .. } = &issuance_reply else {
        let AgentEscrowIssuanceReply::Refused { reason } = issuance_reply else {
            unreachable!("AgentEscrowIssuanceReply has exactly two variants");
        };
        return Err(CryptoError::IssuanceRefused(reason));
    };

    // Step 5: escrow the newly-issued private key, under the exact,
    // well-known name the retrieve path (Step 1, above, and every
    // future call from any pod instance of this same identity) looks
    // it up under -- reusing handle_store_secret directly, the same
    // mechanism CryptoMessage::StoreSecret already uses, rather than
    // duplicating its own encrypt-and-publish logic here.
    handle_store_secret(
        tcp_client,
        escrow_key_reply_router,
        store_secret_reply_router,
        cached_key,
        agent_escrow_keypair_secret_name(&own_identity),
        csr_output.private_key_pem.clone().into_bytes(),
    )
    .await?;

    // Step 6: return the real, usable key -- parsed back from the
    // exact same PEM just generated and escrowed above, matching how
    // handle_retrieve_own_escrow_keypair already parses its own,
    // retrieved PEM.
    RsaPrivateKey::from_pkcs8_pem(&csr_output.private_key_pem)
        .map_err(|e| CryptoError::KeyEncoding(e.to_string()))
}

async fn publish_agent_escrow_issuance_request(
    tcp_client: &ActorRef<TcpClientMessage>,
    agent_escrow_issuance_reply_router: &Arc<ReplyRouter<AgentEscrowIssuanceReply>>,
    request: AgentEscrowIssuanceRequest,
) -> Result<AgentEscrowIssuanceReply, CryptoError> {
    let correlation_id = uuid::Uuid::new_v4().to_string();

    let rx = agent_escrow_issuance_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
        .map(|b| b.to_vec())
        .map_err(|e| {
            agent_escrow_issuance_reply_router.cancel(&correlation_id);
            CryptoError::Serialization(e.to_string())
        })?;

    if let Err(e) = tcp_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        agent_escrow_issuance_reply_router.cancel(&correlation_id);
        return Err(CryptoError::BrokerUnreachable(e.to_string()));
    }

    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(_)) => Err(CryptoError::NoReply),
        Err(_) => {
            agent_escrow_issuance_reply_router.cancel(&correlation_id);
            Err(CryptoError::Timeout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use credentials_agent_common::escrow_key::EscrowKeyReply;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::{RsaPrivateKey, pkcs8::LineEnding};
    use std::sync::OnceLock;

    /// Cached across every test needing a real keypair to stand in
    /// for the Credentials Agent's own escrow key -- RSA generation
    /// is expensive, matching this codebase's established caching
    /// convention for exactly this reason (see e.g.
    /// credentials-agent's own escrow.rs test_keypair()).
    fn test_credentials_agent_keypair() -> (RsaPrivateKey, String) {
        static KEYPAIR: OnceLock<(RsaPrivateKey, String)> = OnceLock::new();
        KEYPAIR
            .get_or_init(|| {
                let mut rng = rsa::rand_core::OsRng;
                let private_key = RsaPrivateKey::new(&mut rng, 4096).unwrap();
                let public_key_pem = private_key
                    .to_public_key()
                    .to_public_key_pem(LineEnding::LF)
                    .unwrap();
                (private_key, public_key_pem)
            })
            .clone()
    }

    /// Intercepts CryptoActor's own outgoing publishes and answers
    /// them exactly as the real Credentials Agent would, over the
    /// same shared OutputPort<ClientEvent> CryptoActor itself
    /// subscribed to at pre_start -- not a direct reply_router.deliver()
    /// call, since CryptoActor's own reply routers are private,
    /// internal state, unlike credentials-agent-observer's own
    /// handler, which holds its router directly.
    struct FakeCredentialsAgent {
        events_output: Arc<OutputPort<ClientEvent>>,
        escrow_private_key: RsaPrivateKey,
        escrow_public_key_pem: String,
        /// Real, shared call count for the escrow-key branch
        /// specifically -- lets a test genuinely prove caching
        /// happened (queried exactly once across N stores), not just
        /// "repeated calls didn't crash."
        escrow_key_queries: Arc<std::sync::atomic::AtomicUsize>,
        /// If Some, RetrieveAgentEscrowKeypairRequests are answered
        /// with a real Found reply, genuinely re-encrypted via the
        /// ephemeral exchange to whatever ephemeral public key the
        /// request itself carries. If None, answered with NotFound --
        /// the "nothing escrowed yet" case.
        stored_escrow_keypair_pem: Option<String>,
        /// Captures the most recently decrypted plaintext from a
        /// genuine StoreSecretRequest -- lets a test assert on its
        /// own, specific expected content directly, now that this
        /// fake agent backs more than one kind of store (see the
        /// removed, hard-coded assertion's own doc comment above).
        /// Arc'd (not a bare Mutex) so the spawning test can hold its
        /// own, shared reference to read from after the actor's own
        /// handle() writes into it.
        last_stored_secret_plaintext: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
        /// If true, AgentEscrowIssuanceRequests are answered with a
        /// real Issued reply (a genuine, self-signed certificate
        /// this test helper builds itself, plus the same escrow
        /// public key PEM this fake agent already hands out for
        /// ordinary escrow-key queries). If false, answered with
        /// Refused -- lets a test exercise the "issuance itself was
        /// refused" path too.
        issue_agent_escrow_certificates: bool,
    }

    #[async_trait]
    impl Actor for FakeCredentialsAgent {
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
            let TcpClientMessage::Publish { topic, payload, .. } = message else {
                return Ok(());
            };

            if topic == CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC {
                if let Ok(envelope) =
                    rkyv::from_bytes::<Envelope<EscrowKeyRequest>, rkyv::rancor::Error>(&payload)
                {
                    self.escrow_key_queries
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: EscrowKeyReply {
                            public_key_pem: self.escrow_public_key_pem.clone(),
                        },
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    self.events_output.send(ClientEvent::MessagePublished {
                        topic: CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC.to_string(),
                        payload: bytes,
                        published_by: None,
                        trace_ctx: None,
                    });
                }
            } else if topic == CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC {
                if let Ok(envelope) =
                    rkyv::from_bytes::<Envelope<StoreSecretRequest>, rkyv::rancor::Error>(&payload)
                {
                    // A genuine, real check -- not just "did a
                    // message arrive," but "was the actual submitted
                    // ciphertext genuinely decryptable back to
                    // something," proving CryptoActor really
                    // encrypted to this key, not merely claimed to.
                    // Deliberately not asserting on a specific,
                    // literal plaintext here -- this same fake agent
                    // now backs multiple, different tests (ordinary
                    // secret storage, and the issuance-fallback
                    // flow's own, real private-key escrow step),
                    // each with genuinely different plaintext.
                    // Individual tests assert on their own, specific
                    // expected content directly, via the captured
                    // decrypted value below.
                    let decrypted = envelope
                        .payload
                        .secret
                        .decrypt(&self.escrow_private_key)
                        .expect("submitted secret must genuinely decrypt with the real key");
                    if let Ok(mut guard) = self.last_stored_secret_plaintext.lock() {
                        *guard = Some(decrypted.to_vec());
                    }

                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: StoreSecretReply::Stored,
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    self.events_output.send(ClientEvent::MessagePublished {
                        topic: CREDENTIALS_STORE_SECRET_REPLIES_TOPIC.to_string(),
                        payload: bytes,
                        published_by: None,
                        trace_ctx: None,
                    });
                }
            } else if topic == CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC {
                if let Ok(envelope) = rkyv::from_bytes::<
                    Envelope<RetrieveAgentEscrowKeypairRequest>,
                    rkyv::rancor::Error,
                >(&payload)
                {
                    let reply_payload = match &self.stored_escrow_keypair_pem {
                        Some(pem) => {
                            let encrypted = credentials_agent_common::ephemeral_exchange::EphemeralEncryptedPayload::encrypt(
                                pem.as_bytes(),
                                &envelope.payload.ephemeral_public_key,
                            )
                            .unwrap();
                            RetrieveAgentEscrowKeypairReply::Found { payload: encrypted }
                        }
                        None => RetrieveAgentEscrowKeypairReply::NotFound,
                    };
                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: reply_payload,
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    self.events_output.send(ClientEvent::MessagePublished {
                        topic: CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC.to_string(),
                        payload: bytes,
                        published_by: None,
                        trace_ctx: None,
                    });
                }
            } else if topic == CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC {
                if let Ok(envelope) = rkyv::from_bytes::<Envelope<AgentEscrowIssuanceRequest>, rkyv::rancor::Error>(&payload) {
                    let reply_payload = if self.issue_agent_escrow_certificates {
                        AgentEscrowIssuanceReply::Issued {
                            // A real, well-formed certificate isn't
                            // needed here -- CryptoActor's own
                            // issuance-fallback logic never actually
                            // parses this string, only the two other
                            // fields it goes on to use directly.
                            certificate_pem:
                                "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                                    .to_string(),
                            ca_chain_pem:
                                "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                                    .to_string(),
                            escrow_public_key_pem: self.escrow_public_key_pem.clone(),
                        }
                    } else {
                        AgentEscrowIssuanceReply::Refused {
                            reason:
                                credentials_agent_common::agent_escrow_issuance::AgentEscrowIssuanceRefusalReason::SigningFailed,
                        }
                    };
                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: reply_payload,
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    self.events_output.send(ClientEvent::MessagePublished {
                        topic: CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC.to_string(),
                        payload: bytes,
                        published_by: None,
                        trace_ctx: None,
                    });
                }
            }

            Ok(())
        }
    }

    async fn spawn_crypto_actor_with_fake_credentials_agent() -> (
        ActorRef<CryptoMessage>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig::default())
            .await
            .0
    }

    /// Configures the shared FakeCredentialsAgent's own, real behavior
    /// -- see each field's own doc comment (on FakeCredentialsAgent
    /// itself) for exactly what each one controls.
    #[derive(Default)]
    struct FakeCredentialsAgentConfig {
        stored_escrow_keypair_pem: Option<String>,
        issue_agent_escrow_certificates: bool,
    }

    async fn spawn_crypto_actor_with_fake_credentials_agent_and_stored_keypair(
        stored_escrow_keypair_pem: Option<String>,
    ) -> (
        ActorRef<CryptoMessage>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
            stored_escrow_keypair_pem,
            ..Default::default()
        })
        .await
        .0
    }

    async fn spawn_crypto_actor_with_fake_credentials_agent_configured(
        config: FakeCredentialsAgentConfig,
    ) -> (
        (
            ActorRef<CryptoMessage>,
            Arc<std::sync::atomic::AtomicUsize>,
        ),
        Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    ) {
        let (private_key, public_key_pem) = test_credentials_agent_keypair();
        let events_output = Arc::new(OutputPort::default());
        let escrow_key_queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let last_stored_secret_plaintext = Arc::new(std::sync::Mutex::new(None));

        let (fake_agent, _) = Actor::spawn(
            None,
            FakeCredentialsAgent {
                events_output: events_output.clone(),
                escrow_private_key: private_key,
                escrow_public_key_pem: public_key_pem,
                escrow_key_queries: escrow_key_queries.clone(),
                stored_escrow_keypair_pem: config.stored_escrow_keypair_pem,
                last_stored_secret_plaintext: last_stored_secret_plaintext.clone(),
                issue_agent_escrow_certificates: config.issue_agent_escrow_certificates,
            },
            (),
        )
        .await
        .unwrap();

        let (crypto_actor, _) = Actor::spawn(
            None,
            CryptoActor,
            CryptoActorArgs {
                tcp_client: fake_agent,
                events_output,
                own_identity: "test-agent".to_string(),
            },
        )
        .await
        .unwrap();

        ((crypto_actor, escrow_key_queries), last_stored_secret_plaintext)
    }

    #[tokio::test]
    async fn store_secret_round_trips_through_a_real_encrypt_and_a_real_fake_broker() {
        let (crypto_actor, _escrow_key_queries) =
            spawn_crypto_actor_with_fake_credentials_agent().await;

        let result = ractor::call!(
            crypto_actor,
            |reply| CryptoMessage::StoreSecret {
                name: "gitlab-token".to_string(),
                plaintext: b"a real secret from a real actor".to_vec(),
                reply,
            }
        )
        .unwrap();

        assert!(result.is_ok(), "expected Ok(()), got {result:?}");
    }

    #[tokio::test]
    async fn escrow_key_is_genuinely_cached_after_the_first_store() {
        let (crypto_actor, escrow_key_queries) =
            spawn_crypto_actor_with_fake_credentials_agent().await;

        for _ in 0..3 {
            let result = ractor::call!(
                crypto_actor,
                |reply| CryptoMessage::StoreSecret {
                    name: "gitlab-token".to_string(),
                    plaintext: b"a real secret from a real actor".to_vec(),
                    reply,
                }
            )
            .unwrap();
            assert!(result.is_ok(), "expected Ok(()), got {result:?}");
        }

        // The real proof: exactly one query across three stores, not
        // "it didn't crash on repeat calls."
        assert_eq!(
            escrow_key_queries.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "escrow key should be queried exactly once, then served from cache"
        );
    }

    #[tokio::test]
    async fn ensure_own_escrow_keypair_falls_back_to_real_issuance_when_nothing_is_escrowed_yet() {
        let ((crypto_actor, _), last_stored_secret_plaintext) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: None,
                issue_agent_escrow_certificates: true,
            })
            .await;

        // read_kubernetes_sa_token() genuinely reads from disk --
        // point it at a real, temp file standing in for the
        // in-cluster, mounted service-account token.
        let token_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(token_file.path(), "fake-k8s-service-account-token\n").unwrap();
        unsafe {
            std::env::set_var("KUBERNETES_SA_TOKEN", token_file.path());
        }

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::EnsureOwnEscrowKeypair { reply }
        })
        .unwrap();

        unsafe {
            std::env::remove_var("KUBERNETES_SA_TOKEN");
        }

        let issued_key = match result {
            Ok(key) => key,
            other => panic!("expected Ok(_), got {other:?}"),
        };

        // The real proof this is a genuine issuance, not a stub:
        // the key returned to the caller and the key actually
        // escrowed via a real StoreSecretRequest are the exact same
        // key -- both parse to the identical public key.
        let escrowed_pem = last_stored_secret_plaintext
            .lock()
            .unwrap()
            .clone()
            .expect("a real StoreSecretRequest must have genuinely been submitted");
        let escrowed_key = RsaPrivateKey::from_pkcs8_pem(
            std::str::from_utf8(&escrowed_pem).unwrap(),
        )
        .expect("the escrowed plaintext must itself be a genuine, valid PKCS8 RSA private key");

        assert_eq!(issued_key.to_public_key(), escrowed_key.to_public_key());
    }

    #[tokio::test]
    async fn ensure_own_escrow_keypair_returns_the_existing_key_without_issuing_when_one_is_already_escrowed()
     {
        let mut rng = rsa::rand_core::OsRng;
        let previously_escrowed_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let previously_escrowed_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &previously_escrowed_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();

        let ((crypto_actor, _), _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: Some(previously_escrowed_key_pem.to_string()),
                // Deliberately false -- if EnsureOwnEscrowKeypair
                // genuinely, correctly returns via the retrieve path
                // when a key already exists, it should never even
                // attempt issuance, let alone need it to succeed.
                issue_agent_escrow_certificates: false,
            })
            .await;

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::EnsureOwnEscrowKeypair { reply }
        })
        .unwrap();

        match result {
            Ok(key) => {
                assert_eq!(key.to_public_key(), previously_escrowed_key.to_public_key());
            }
            other => panic!("expected Ok(_), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retrieve_own_escrow_keypair_returns_none_when_nothing_has_ever_been_escrowed() {
        let (crypto_actor, _) = spawn_crypto_actor_with_fake_credentials_agent().await;

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::RetrieveOwnEscrowKeypair { reply }
        })
        .unwrap();

        match result {
            Ok(None) => {}
            other => panic!("expected Ok(None), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retrieve_own_escrow_keypair_genuinely_round_trips_a_real_key_through_the_real_ephemeral_exchange()
     {
        // A real, distinct RSA keypair standing in for what the
        // agent had previously escrowed -- its own PKCS8 PEM is what
        // the fake Credentials Agent hands back, genuinely
        // re-encrypted via the real ephemeral exchange for each call.
        let mut rng = rsa::rand_core::OsRng;
        let previously_escrowed_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let previously_escrowed_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &previously_escrowed_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();

        let (crypto_actor, _) = spawn_crypto_actor_with_fake_credentials_agent_and_stored_keypair(
            Some(previously_escrowed_key_pem.to_string()),
        )
        .await;

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::RetrieveOwnEscrowKeypair { reply }
        })
        .unwrap();

        match result {
            Ok(Some(retrieved_key)) => {
                // The real proof: the retrieved key's own modulus
                // genuinely matches the original -- a real key
                // round-tripped through the real ephemeral exchange
                // and real PKCS8 parsing, not merely "some value came
                // back."
                assert_eq!(
                    retrieved_key.to_public_key(),
                    previously_escrowed_key.to_public_key()
                );
            }
            other => panic!("expected Ok(Some(_)), got {other:?}"),
        }
    }
}
