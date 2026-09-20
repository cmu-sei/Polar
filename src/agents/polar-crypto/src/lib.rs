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
//! Scope so far: general-secret store, and the complete agent
//! escrow-keypair bootstrap flow -- retrieve-first, issuance-as-
//! fallback, escrowing the newly-issued result, and now kernel-
//! keyring custody, attempted transparently on first real use and
//! cached as an opaque handle afterward. RetrieveOwnEscrowKeypair and
//! EnsureOwnEscrowKeypair, this crate's own former, public messages
//! for that flow, are deliberately no longer exposed at all: the
//! whole point of a hard, single boundary for every key operation is
//! that no caller outside this crate should ever hold a raw
//! RsaPrivateKey, kernel-backed or not. DecryptOwnMessage is the
//! real, current way any other actor decrypts something addressed to
//! this identity -- ciphertext in, plaintext out, with no way to
//! observe which custody mode produced it. See credentials-agent-
//! architecture.md's own "Agent escrow bootstrap" section for the
//! full, intended sequence, and polar-kernel-keyring's own crate for
//! the real kernel-custody mechanics.
//!
//! Delivery: this actor registers its own five, real reply topics
//! directly with a real, live polar_router::RouterActor -- the same,
//! single, real RouterActor the consuming supervisor already spawned
//! for its own topics, sharing one real Cassini connection rather
//! than requiring a second. This replaced an earlier, real design
//! built entirely around a broadcast-style Arc<OutputPort<ClientEvent>>,
//! back when RouterActor didn't yet exist; that mechanism is
//! structurally incompatible with DeliveryMode::Router (a
//! ClientEvent::MessagePublished event never reaches any real
//! OutputPort in that mode at all -- it's routed by topic instead),
//! which every genuinely router-based consumer (kube-consumer, the
//! first real one) now uses.

use cassini_client::TcpClientMessage;
use cert_issuer_common::keypair::{KeyAlgorithm, KeypairError};
use credentials_agent_common::agent_escrow_issuance::{
    AgentEscrowIssuanceReply, AgentEscrowIssuanceRequest,
};
use credentials_agent_common::agent_escrow_key::{AgentEscrowKeyReply, AgentEscrowKeyRequest};
use credentials_agent_common::agent_escrow_keypair_bootstrap::{
    RetrieveAgentEscrowKeypairReply, RetrieveAgentEscrowKeypairRequest,
    agent_escrow_keypair_secret_name,
};
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use credentials_agent_common::escrow_secret::EscrowedSecret;
use credentials_agent_common::secret::{StoreRefusalReason, StoreSecretReply, StoreSecretRequest};
use credentials_agent_common::{
    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC, CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC,
    CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC, CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC, CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC, CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
    CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
};
use polar::request_reply::{Envelope, ReplyRouter};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};
use rsa::{RsaPrivateKey, RsaPublicKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey};
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
    #[error("decrypt failed: {0}")]
    DecryptFailed(String),
    #[error("no usable escrow public key exists for identity {0:?} -- that agent may never have completed automated escrow-identity issuance")]
    RecipientKeyNotFound(String),
}

/// This actor's own, cached escrow keypair -- either a real, opaque
/// kernel-keyring handle (the raw private key never entered this
/// process's own memory after the initial load; see
/// polar-kernel-keyring's own crate for the mechanics), or the raw
/// key itself, held in ordinary process memory, when kernel custody
/// genuinely wasn't available on this machine (confirmed via a real,
/// empirical attempt, not assumed). Deliberately not exposed outside
/// this crate at all -- see this crate's own, top-level doc comment
/// for why.
enum EscrowKeypairHandle {
    KernelBacked(polar_kernel_keyring::KeyringSerial),
    InMemory(RsaPrivateKey),
}

pub enum CryptoMessage {
    /// A general secret to escrow -- e.g. an access token an observer
    /// holds and wants the Credentials Agent to store on its behalf.
    /// Not the agent's own escrow private key -- see
    /// DecryptOwnMessage, below, for that flow.
    StoreSecret {
        name: String,
        plaintext: Vec<u8>,
        reply: RpcReplyPort<Result<(), CryptoError>>,
    },
    /// Decrypts `ciphertext` -- something a peer encrypted with real,
    /// standard OAEP to this identity's own escrow public key -- and
    /// returns the real plaintext. This is the one, sole way any
    /// other actor in this process ever decrypts something addressed
    /// to this identity: no message in this crate's own, public API
    /// ever hands back a raw RsaPrivateKey, kernel-backed handle, or
    /// anything else about how this identity's own key is actually
    /// held. The full retrieve-first/issuance-fallback/escrow
    /// sequence, and the empirical attempt at kernel-keyring custody,
    /// all happen internally, transparently, on first real use.
    DecryptOwnMessage {
        ciphertext: Vec<u8>,
        reply: RpcReplyPort<Result<Vec<u8>, CryptoError>>,
    },
    /// Encrypts `plaintext` to `recipient_identity`'s own, real
    /// escrow public key -- the one, sole way any other actor in
    /// this process ever encrypts something addressed to a different
    /// identity: this actor looks up (and caches) that identity's
    /// own public key itself, via a real AgentEscrowKeyRequest to the
    /// Credentials Agent on first use, without the caller ever
    /// needing to know or handle any key material directly. The
    /// returned bytes are EscrowedSecret's own, real, serialized wire
    /// format -- the same one credentials-agent-common's own
    /// StoreSecret flow already produces -- ready to publish as-is.
    EncryptForRecipient {
        recipient_identity: String,
        plaintext: Vec<u8>,
        reply: RpcReplyPort<Result<Vec<u8>, CryptoError>>,
    },
    /// Runs this identity's own escrow-keypair bootstrap sequence --
    /// retrieve-first, issuance-as-fallback, real, empirical kernel-
    /// custody attempt -- as a pure side effect, with no ciphertext
    /// and no reply payload beyond success/failure. This is the one,
    /// real reason it exists: polar-init's own, brief lifetime is the
    /// only point in this agent's entire lifecycle where the
    /// Kubernetes service-account token (required for real issuance)
    /// is ever readable at all -- the main, long-running agent
    /// process never has it mounted. Sending this from polar-init
    /// guarantees the main process's own, later CryptoActor instance
    /// can always succeed via the retrieve path alone, the first time
    /// DecryptOwnMessage or EncryptForRecipient is genuinely used,
    /// without ever needing issuance itself.
    EnsureBootstrapped {
        reply: RpcReplyPort<Result<(), CryptoError>>,
    },
    /// Internal: a raw, incoming message on one of this actor's own,
    /// registered reply topics, forwarded by RouterActor. Carries the
    /// topic alongside the payload -- unlike the earlier,
    /// broadcast-style design (a single ClientEvent variant, matched
    /// on internally), RouterActor's own registration is already
    /// per-topic, so each of the five, real Register calls in
    /// pre_start builds its own closure knowing exactly which topic
    /// it's for; this variant exists only because all five, real
    /// closures share the same underlying delivery shape.
    RoutedReply {
        topic: String,
        payload: Vec<u8>,
    },
}

pub struct CryptoActorArgs {
    /// The consuming agent's own, already-spawned TcpClientActor --
    /// shared, not a second connection. See
    /// credentials-agent-architecture.md's own "CryptoActor" section
    /// for why this must stay fire-and-forget in both directions.
    pub tcp_client: ActorRef<TcpClientMessage>,
    /// The consuming supervisor's own, already-spawned RouterActor --
    /// the same one its own, real topics are already registered
    /// with, not a second one. CryptoActor registers its own five,
    /// real reply topics with it directly in pre_start, rather than
    /// subscribing to a broadcast-style OutputPort<ClientEvent> (the
    /// earlier, real design, before RouterActor existed) -- see this
    /// crate's own, top-level doc comment for the full reasoning.
    pub router: ActorRef<polar_router::RouterMessage>,
}

pub struct CryptoActorState {
    tcp_client: ActorRef<TcpClientMessage>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    retrieve_agent_escrow_keypair_reply_router: Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
    agent_escrow_issuance_reply_router: Arc<ReplyRouter<AgentEscrowIssuanceReply>>,
    agent_escrow_key_reply_router: Arc<ReplyRouter<AgentEscrowKeyReply>>,
    /// The Credentials Agent's own current escrow public key, learned
    /// on first use and reused afterward -- avoids re-querying on
    /// every single store, per credentials-agent-architecture.md's
    /// own note on CryptoActor's local caching. Arc<Mutex<_>> rather
    /// than a plain field: the actual store work runs in a spawned
    /// task, not inline in handle() (see handle()'s own StoreSecret
    /// arm for why), so this needs to be mutable from outside the
    /// actor's own sequential mailbox loop.
    cached_credentials_agent_escrow_key: Arc<tokio::sync::Mutex<Option<String>>>,
    /// This identity's own escrow keypair, populated lazily on first
    /// real use of DecryptOwnMessage -- retrieve-first, issuance-
    /// fallback, and the empirical kernel-custody attempt all happen
    /// once, the first time this is genuinely needed, not at actor
    /// startup. Same Arc<Mutex<_>> reasoning as
    /// cached_credentials_agent_escrow_key, above.
    escrow_keypair_handle: Arc<tokio::sync::Mutex<Option<EscrowKeypairHandle>>>,
    /// Other identities' own, real escrow public keys, keyed by
    /// identity -- learned on first use of EncryptForRecipient for
    /// each specific recipient, and reused afterward, same caching
    /// reasoning as cached_credentials_agent_escrow_key above.
    /// Caches both the real, already-parsed RsaPublicKey (so
    /// EscrowedSecret::encrypt_to_parsed_key can skip a real,
    /// repeated PEM-parse cost on every single call) and the
    /// original PEM string alongside it (still required as real wire
    /// metadata -- see EscrowedSecret::encrypt_to_parsed_key's own
    /// doc comment).
    cached_recipient_public_keys:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, (RsaPublicKey, String)>>>,
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
        // Derived internally, not threaded in as a real, explicit
        // argument -- deliberately: this identity is already,
        // genuinely readable from this agent's own, already-mounted
        // mTLS client certificate (the same, real TLS_CLIENT_CERT
        // path TCPClientConfig::new() itself already reads), and
        // CryptoActor is meant to be the one, sole, real gateway for
        // every crypto-adjacent concern -- including this one --
        // rather than every, real caller needing its own, separate
        // way to learn or compute the same, real identity string.
        let own_identity = {
            let path = std::env::var("TLS_CLIENT_CERT").map_err(|e| {
                ActorProcessingErr::from(format!("TLS_CLIENT_CERT must be set: {e}"))
            })?;
            let certificate_pem = std::fs::read_to_string(&path).map_err(|e| {
                ActorProcessingErr::from(format!("failed to read {path}: {e}"))
            })?;
            cert_issuer_common::permission::extract_own_identity(&certificate_pem)
                .map_err(|e| ActorProcessingErr::from(e.to_string()))?
        };

        // Registers each of this actor's own, real reply topics
        // directly with the shared RouterActor -- CryptoActor is a
        // requester here, not a responder, so it registers the reply
        // topics, not the request topics (easy to transpose by
        // accident, worth being explicit about). Each closure does
        // the same, minimal, real thing: forward the raw topic and
        // payload back to this actor as RoutedReply, letting handle()
        // itself do the real, per-topic deserialization/routing --
        // mirroring kube-consumer's own, established
        // register_kube_topics pattern exactly.
        for topic in [
            CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC,
            CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
            CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
            CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC,
        ] {
            let myself_for_deliver = myself.clone();
            if let Err(e) = args.router.send_message(polar_router::RouterMessage::Register {
                topic: topic.to_string(),
                deliver: Box::new(move |incoming: polar_router::IncomingMessage| {
                    if let Err(e) = myself_for_deliver.send_message(CryptoMessage::RoutedReply {
                        topic: incoming.topic,
                        payload: incoming.payload,
                    }) {
                        warn!("CryptoActor: failed to deliver routed reply to self: {e}");
                    }
                }),
            }) {
                error!("CryptoActor failed to register topic {topic} with RouterActor: {e}");
            }
        }

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
        if let Err(e) = args.tcp_client.send_message(TcpClientMessage::Subscribe {
            topic: CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC.to_string(),
            trace_ctx: None,
        }) {
            error!("CryptoActor failed to subscribe to agent escrow-key replies: {e}");
        }

        Ok(CryptoActorState {
            tcp_client: args.tcp_client,
            escrow_key_reply_router: Arc::new(ReplyRouter::new()),
            store_secret_reply_router: Arc::new(ReplyRouter::new()),
            retrieve_agent_escrow_keypair_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_issuance_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
            cached_credentials_agent_escrow_key: Arc::new(tokio::sync::Mutex::new(None)),
            escrow_keypair_handle: Arc::new(tokio::sync::Mutex::new(None)),
            cached_recipient_public_keys: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            own_identity,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CryptoMessage::RoutedReply { topic, payload }
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
            CryptoMessage::RoutedReply { topic, payload }
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
            CryptoMessage::RoutedReply { topic, payload }
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
            CryptoMessage::RoutedReply { topic, payload }
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
            CryptoMessage::RoutedReply { topic, payload }
                if topic == CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<AgentEscrowKeyReply>, rkyv::rancor::Error>(&payload) {
                    Ok(envelope) => state
                        .agent_escrow_key_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!("CryptoActor failed to deserialize agent escrow-key reply: {e}"),
                }
            }
            CryptoMessage::RoutedReply { .. } => {
                // A topic this actor didn't itself register for --
                // RouterActor's own dispatch already guarantees this
                // never genuinely happens, but kept as an explicit,
                // harmless no-op arm rather than silently falling
                // through a wildcard elsewhere.
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
            CryptoMessage::DecryptOwnMessage { ciphertext, reply } => {
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
                let escrow_keypair_handle = state.escrow_keypair_handle.clone();
                let own_identity = state.own_identity.clone();
                tokio::spawn(async move {
                    let result = handle_decrypt_own_message(
                        tcp_client,
                        retrieve_agent_escrow_keypair_reply_router,
                        agent_escrow_issuance_reply_router,
                        escrow_key_reply_router,
                        store_secret_reply_router,
                        cached_key,
                        escrow_keypair_handle,
                        own_identity,
                        ciphertext,
                    )
                    .await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::EncryptForRecipient {
                recipient_identity,
                plaintext,
                reply,
            } => {
                // Same reasoning as StoreSecret's own arm above --
                // spawned, not awaited inline.
                let tcp_client = state.tcp_client.clone();
                let agent_escrow_key_reply_router = state.agent_escrow_key_reply_router.clone();
                let cached_recipient_public_keys = state.cached_recipient_public_keys.clone();
                tokio::spawn(async move {
                    let result = handle_encrypt_for_recipient(
                        tcp_client,
                        agent_escrow_key_reply_router,
                        cached_recipient_public_keys,
                        recipient_identity,
                        plaintext,
                    )
                    .await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::EnsureBootstrapped { reply } => {
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
                let escrow_keypair_handle = state.escrow_keypair_handle.clone();
                let own_identity = state.own_identity.clone();
                tokio::spawn(async move {
                    let result = handle_ensure_bootstrapped(
                        tcp_client,
                        retrieve_agent_escrow_keypair_reply_router,
                        agent_escrow_issuance_reply_router,
                        escrow_key_reply_router,
                        store_secret_reply_router,
                        cached_key,
                        escrow_keypair_handle,
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

/// The real, shared bootstrap logic behind both DecryptOwnMessage and
/// EnsureBootstrapped -- populates `escrow_keypair_handle` exactly
/// once (retrieve-first, issuance-as-fallback, then the real,
/// empirical kernel-custody attempt), if it isn't already populated.
/// Once set, the handle is never mutated again -- only ever read
/// afterward -- so a caller re-locking the same mutex separately,
/// after this returns, to read the now-stable value is genuinely
/// safe, not a real race.
async fn ensure_escrow_keypair_bootstrapped(
    tcp_client: ActorRef<TcpClientMessage>,
    retrieve_agent_escrow_keypair_reply_router: Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
    agent_escrow_issuance_reply_router: Arc<ReplyRouter<AgentEscrowIssuanceReply>>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    cached_key: Arc<tokio::sync::Mutex<Option<String>>>,
    escrow_keypair_handle: Arc<tokio::sync::Mutex<Option<EscrowKeypairHandle>>>,
    own_identity: String,
) -> Result<(), CryptoError> {
    let mut handle_guard = escrow_keypair_handle.lock().await;

    if handle_guard.is_none() {
        let private_key = handle_ensure_own_escrow_keypair(
            tcp_client,
            retrieve_agent_escrow_keypair_reply_router,
            agent_escrow_issuance_reply_router,
            escrow_key_reply_router,
            store_secret_reply_router,
            cached_key,
            own_identity.clone(),
        )
        .await?;

        let der_bytes = private_key
            .to_pkcs8_der()
            .map_err(|e| CryptoError::KeyEncoding(e.to_string()))?;

        // A real, empirical attempt -- try_load_asymmetric_key itself
        // makes the actual, real add_key/keyctl_pkey_query syscalls
        // (see polar-kernel-keyring's own crate for the mechanics),
        // rather than this crate assuming kernel support one way or
        // the other. A genuine None here is an ordinary, expected
        // fallback signal, not an error.
        let new_handle = match polar_kernel_keyring::try_load_asymmetric_key(
            &own_identity,
            der_bytes.as_bytes(),
        ) {
            Some(serial) => EscrowKeypairHandle::KernelBacked(serial),
            None => EscrowKeypairHandle::InMemory(private_key),
        };

        *handle_guard = Some(new_handle);
    }

    Ok(())
}

/// The real handler behind CryptoMessage::DecryptOwnMessage -- see
/// that variant's own doc comment for the complete, intended
/// behavior. On first genuine use, runs the full retrieve/issuance-
/// fallback sequence exactly once (handle_ensure_own_escrow_keypair,
/// unchanged), then makes one, real, empirical attempt at kernel-
/// keyring custody -- success caches an opaque KernelBacked handle;
/// a genuine failure (kernel unavailable on this machine) caches the
/// raw key itself, InMemory, as the real, correct fallback. Every
/// later call reuses whichever was cached, without repeating any of
/// this setup.
async fn handle_decrypt_own_message(
    tcp_client: ActorRef<TcpClientMessage>,
    retrieve_agent_escrow_keypair_reply_router: Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
    agent_escrow_issuance_reply_router: Arc<ReplyRouter<AgentEscrowIssuanceReply>>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    cached_key: Arc<tokio::sync::Mutex<Option<String>>>,
    escrow_keypair_handle: Arc<tokio::sync::Mutex<Option<EscrowKeypairHandle>>>,
    own_identity: String,
    ciphertext: Vec<u8>,
) -> Result<Vec<u8>, CryptoError> {
    ensure_escrow_keypair_bootstrapped(
        tcp_client,
        retrieve_agent_escrow_keypair_reply_router,
        agent_escrow_issuance_reply_router,
        escrow_key_reply_router,
        store_secret_reply_router,
        cached_key,
        escrow_keypair_handle.clone(),
        own_identity,
    )
    .await?;

    let handle_guard = escrow_keypair_handle.lock().await;
    match handle_guard.as_ref().expect("ensure_escrow_keypair_bootstrapped just populated this") {
        EscrowKeypairHandle::KernelBacked(serial) => {
            polar_kernel_keyring::decrypt(*serial, &ciphertext)
                .map_err(|e| CryptoError::DecryptFailed(e.to_string()))
        }
        EscrowKeypairHandle::InMemory(private_key) => private_key
            .decrypt(rsa::Oaep::<sha2_011::Sha256>::new(), &ciphertext)
            .map_err(|e| CryptoError::DecryptFailed(e.to_string())),
    }
}

/// The real handler behind CryptoMessage::EnsureBootstrapped -- see
/// that variant's own doc comment for the complete, intended
/// behavior. Genuinely just the setup half of DecryptOwnMessage,
/// without the final decrypt dispatch -- no ciphertext, only the
/// real, side-effecting bootstrap itself.
async fn handle_ensure_bootstrapped(
    tcp_client: ActorRef<TcpClientMessage>,
    retrieve_agent_escrow_keypair_reply_router: Arc<ReplyRouter<RetrieveAgentEscrowKeypairReply>>,
    agent_escrow_issuance_reply_router: Arc<ReplyRouter<AgentEscrowIssuanceReply>>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    cached_key: Arc<tokio::sync::Mutex<Option<String>>>,
    escrow_keypair_handle: Arc<tokio::sync::Mutex<Option<EscrowKeypairHandle>>>,
    own_identity: String,
) -> Result<(), CryptoError> {
    ensure_escrow_keypair_bootstrapped(
        tcp_client,
        retrieve_agent_escrow_keypair_reply_router,
        agent_escrow_issuance_reply_router,
        escrow_key_reply_router,
        store_secret_reply_router,
        cached_key,
        escrow_keypair_handle,
        own_identity,
    )
    .await
}

/// The real handler behind CryptoMessage::EncryptForRecipient -- see
/// that variant's own doc comment for the complete, intended
/// behavior. Looks up (and caches, per identity) the recipient's own,
/// real escrow public key via a real AgentEscrowKeyRequest on first
/// use for that specific identity, then encrypts via
/// EscrowedSecret::encrypt -- the same, real, standard OAEP-wrapped
/// hybrid cryptosystem StoreSecret already uses, returning its own,
/// real, serialized wire bytes directly.
async fn handle_encrypt_for_recipient(
    tcp_client: ActorRef<TcpClientMessage>,
    agent_escrow_key_reply_router: Arc<ReplyRouter<AgentEscrowKeyReply>>,
    cached_recipient_public_keys: Arc<tokio::sync::Mutex<std::collections::HashMap<String, (RsaPublicKey, String)>>>,
    recipient_identity: String,
    plaintext: Vec<u8>,
) -> Result<Vec<u8>, CryptoError> {
    let (public_key, public_key_pem) = {
        let mut cache = cached_recipient_public_keys.lock().await;
        match cache.get(&recipient_identity) {
            Some((key, pem)) => (key.clone(), pem.clone()),
            None => {
                let reply = publish_agent_escrow_key_request(
                    &tcp_client,
                    &agent_escrow_key_reply_router,
                    AgentEscrowKeyRequest {
                        identity: recipient_identity.clone(),
                    },
                )
                .await?;

                let pem = match reply {
                    AgentEscrowKeyReply::Found { public_key_pem } => public_key_pem,
                    AgentEscrowKeyReply::NotFound => {
                        return Err(CryptoError::RecipientKeyNotFound(recipient_identity));
                    }
                };

                // Parsed once, here, on genuine first use for this
                // identity -- every later call for the same identity
                // reuses the cached, already-parsed key directly,
                // never re-parsing the same PEM again.
                let key = RsaPublicKey::from_public_key_pem(&pem)
                    .map_err(|e| CryptoError::KeyEncoding(e.to_string()))?;

                cache.insert(recipient_identity.clone(), (key.clone(), pem.clone()));
                (key, pem)
            }
        }
    };

    let escrowed = EscrowedSecret::encrypt_to_parsed_key(&plaintext, &public_key, &public_key_pem)?;

    rkyv::to_bytes::<rkyv::rancor::Error>(&escrowed)
        .map(|b| b.to_vec())
        .map_err(|e| CryptoError::Serialization(e.to_string()))
}

async fn publish_agent_escrow_key_request(
    tcp_client: &ActorRef<TcpClientMessage>,
    agent_escrow_key_reply_router: &Arc<ReplyRouter<AgentEscrowKeyReply>>,
    request: AgentEscrowKeyRequest,
) -> Result<AgentEscrowKeyReply, CryptoError> {
    let correlation_id = uuid::Uuid::new_v4().to_string();

    let rx = agent_escrow_key_reply_router.register(correlation_id.clone());

    let envelope = Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    };
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
        .map(|b| b.to_vec())
        .map_err(|e| {
            agent_escrow_key_reply_router.cancel(&correlation_id);
            CryptoError::Serialization(e.to_string())
        })?;

    if let Err(e) = tcp_client.send_message(TcpClientMessage::publish(
        CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC.to_string(),
        payload,
        None,
    )) {
        agent_escrow_key_reply_router.cancel(&correlation_id);
        return Err(CryptoError::BrokerUnreachable(e.to_string()));
    }

    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(_)) => Err(CryptoError::NoReply),
        Err(_) => {
            agent_escrow_key_reply_router.cancel(&correlation_id);
            Err(CryptoError::Timeout)
        }
    }
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
                let mut rng = rand::rng();
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
    /// them exactly as the real Credentials Agent would, publishing
    /// its own real replies directly to `router` -- the same, real
    /// RouterActor CryptoActor itself registered its own reply
    /// topics with at pre_start. Mirrors the earlier, real,
    /// broadcast-style FakeCredentialsAgent exactly in every other
    /// respect; only the delivery mechanism changed.
    struct FakeCredentialsAgent {
        router: ActorRef<polar_router::RouterMessage>,
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
        /// If Some, AgentEscrowKeyRequests are answered with a real
        /// Found reply carrying this PEM -- a real, distinct
        /// recipient identity's own public key, standing in for
        /// EncryptForRecipient's own, real target. If None, answered
        /// with NotFound.
        recipient_public_key_pem: Option<String>,
        /// Real, shared call count for the agent-escrow-key branch
        /// specifically -- same reasoning as escrow_key_queries,
        /// above, but for EncryptForRecipient's own, separate cache.
        agent_escrow_key_queries: Arc<std::sync::atomic::AtomicUsize>,
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

            let route = |reply_topic: &str, bytes: Vec<u8>| {
                let _ = self.router.send_message(polar_router::RouterMessage::Route(
                    polar_router::IncomingMessage {
                        topic: reply_topic.to_string(),
                        payload: bytes,
                        published_by: None,
                        trace_ctx: None,
                    },
                ));
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
                    route(CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC, bytes);
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
                    route(CREDENTIALS_STORE_SECRET_REPLIES_TOPIC, bytes);
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
                    route(CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC, bytes);
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
                    route(CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC, bytes);
                }
            } else if topic == CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC {
                if let Ok(envelope) =
                    rkyv::from_bytes::<Envelope<AgentEscrowKeyRequest>, rkyv::rancor::Error>(&payload)
                {
                    self.agent_escrow_key_queries
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let reply_payload = match &self.recipient_public_key_pem {
                        Some(pem) => AgentEscrowKeyReply::Found {
                            public_key_pem: pem.clone(),
                        },
                        None => AgentEscrowKeyReply::NotFound,
                    };
                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: reply_payload,
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    route(CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC, bytes);
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
        recipient_public_key_pem: Option<String>,
    }

    /// pre_start now derives its own identity by reading a real,
    /// mounted TLS_CLIENT_CERT -- this builds a real, temporary,
    /// self-signed certificate carrying "test-agent" as its own
    /// identity SAN (mirroring extract_own_identity's own, real
    /// test fixtures directly), writes it to a real, temporary file,
    /// and points the env var at it, so every test in this module
    /// continues to genuinely exercise "test-agent" as this actor's
    /// own identity, exactly as before this migration.
    fn write_test_agent_certificate() -> tempfile::NamedTempFile {
        let mut rng = rand::rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let private_key_pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params =
            rcgen::CertificateParams::new(vec!["test-agent".to_string()]).unwrap();
        let certificate_pem = params.self_signed(&key_pair).unwrap().pem();

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), certificate_pem).unwrap();
        file
    }

    async fn spawn_crypto_actor_with_fake_credentials_agent_configured(
        config: FakeCredentialsAgentConfig,
    ) -> (
        (
            ActorRef<CryptoMessage>,
            Arc<std::sync::atomic::AtomicUsize>,
        ),
        Arc<std::sync::Mutex<Option<Vec<u8>>>>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let (private_key, public_key_pem) = test_credentials_agent_keypair();
        let escrow_key_queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_escrow_key_queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let last_stored_secret_plaintext = Arc::new(std::sync::Mutex::new(None));

        let (router, _) = Actor::spawn(None, polar_router::RouterActor, ())
            .await
            .unwrap();

        let (fake_agent, _) = Actor::spawn(
            None,
            FakeCredentialsAgent {
                router: router.clone(),
                escrow_private_key: private_key,
                escrow_public_key_pem: public_key_pem,
                escrow_key_queries: escrow_key_queries.clone(),
                stored_escrow_keypair_pem: config.stored_escrow_keypair_pem,
                last_stored_secret_plaintext: last_stored_secret_plaintext.clone(),
                issue_agent_escrow_certificates: config.issue_agent_escrow_certificates,
                recipient_public_key_pem: config.recipient_public_key_pem,
                agent_escrow_key_queries: agent_escrow_key_queries.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let own_cert_file = write_test_agent_certificate();
        unsafe {
            std::env::set_var("TLS_CLIENT_CERT", own_cert_file.path());
        }

        let (crypto_actor, _) = Actor::spawn(
            None,
            CryptoActor,
            CryptoActorArgs {
                tcp_client: fake_agent,
                router,
            },
        )
        .await
        .unwrap();

        unsafe {
            std::env::remove_var("TLS_CLIENT_CERT");
        }
        // own_cert_file must outlive pre_start's own, real read of it
        // above -- explicitly kept alive until here, then dropped
        // (and its own, temp file deleted) only now.
        drop(own_cert_file);

        (
            (crypto_actor, escrow_key_queries),
            last_stored_secret_plaintext,
            agent_escrow_key_queries,
        )
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
    async fn decrypt_own_message_falls_back_to_real_issuance_when_nothing_is_escrowed_yet() {
        let ((crypto_actor, _), last_stored_secret_plaintext, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: None,
                issue_agent_escrow_certificates: true,
                ..Default::default()
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

        // The real proof this is a genuine issuance, not a stub:
        // encrypt to the *newly-issued* key's own public half
        // (extracted from what was actually escrowed, since
        // DecryptOwnMessage never hands the key itself back to any
        // caller), then confirm CryptoActor genuinely decrypts it
        // correctly through the same, brand-new identity.
        let plaintext = b"a real secret, addressed to a freshly-issued escrow identity";
        let mut rng = rand::rng();

        // We can't encrypt until issuance has genuinely happened --
        // trigger it via a first DecryptOwnMessage call (whose own
        // ciphertext here is deliberately garbage; only the
        // bootstrap side effect, the real issuance and escrow, is
        // what this first call is genuinely for).
        let _ = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::DecryptOwnMessage {
                ciphertext: vec![0u8; 32],
                reply,
            }
        })
        .unwrap();

        let escrowed_pem = last_stored_secret_plaintext
            .lock()
            .unwrap()
            .clone()
            .expect("a real StoreSecretRequest must have genuinely been submitted");
        let issued_key = RsaPrivateKey::from_pkcs8_pem(std::str::from_utf8(&escrowed_pem).unwrap())
            .expect("the escrowed plaintext must itself be a genuine, valid PKCS8 RSA private key");

        let ciphertext = issued_key
            .to_public_key()
            .encrypt(&mut rng, rsa::Oaep::<sha2_011::Sha256>::new(), plaintext)
            .expect("real OAEP encryption to the newly-issued public half must succeed");

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::DecryptOwnMessage { ciphertext, reply }
        })
        .unwrap();

        match result {
            Ok(decrypted) => assert_eq!(decrypted, plaintext),
            other => panic!("expected Ok(_), got {other:?}"),
        }

        unsafe {
            std::env::remove_var("KUBERNETES_SA_TOKEN");
        }
    }

    #[tokio::test]
    async fn decrypt_own_message_genuinely_round_trips_a_real_message_when_a_key_is_already_escrowed() {
        let mut rng = rand::rng();
        let previously_escrowed_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let previously_escrowed_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &previously_escrowed_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();

        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: Some(previously_escrowed_key_pem.to_string()),
                // Deliberately false -- if DecryptOwnMessage
                // genuinely, correctly retrieves the already-escrowed
                // key, it should never even attempt issuance, let
                // alone need it to succeed.
                issue_agent_escrow_certificates: false,
                ..Default::default()
            })
            .await;

        let plaintext = b"a real secret, addressed to an already-escrowed identity";
        let ciphertext = previously_escrowed_key
            .to_public_key()
            .encrypt(&mut rng, rsa::Oaep::<sha2_011::Sha256>::new(), plaintext)
            .expect("real OAEP encryption to the existing public half must succeed");

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::DecryptOwnMessage { ciphertext, reply }
        })
        .unwrap();

        match result {
            Ok(decrypted) => assert_eq!(decrypted, plaintext),
            other => panic!("expected Ok(_), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn decrypt_own_message_fails_with_issuance_refused_when_nothing_is_escrowed_and_issuance_itself_is_refused()
     {
        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: None,
                // Deliberately false -- issuance genuinely gets
                // attempted (nothing is escrowed yet), and this is
                // what makes it fail, as an observable, real outcome
                // through DecryptOwnMessage's own, sole interface --
                // there's no other way left to distinguish "nothing
                // escrowed" from any other failure now that the
                // retrieve half is no longer separately exposed.
                issue_agent_escrow_certificates: false,
                ..Default::default()
            })
            .await;

        // read_kubernetes_sa_token() genuinely reads from disk before
        // the fake agent's own refusal is ever reached -- still needs
        // a real, valid token file, or this would fail with
        // CredentialUnavailable instead of the real thing this test
        // means to prove.
        let token_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(token_file.path(), "fake-k8s-service-account-token\n").unwrap();
        unsafe {
            std::env::set_var("KUBERNETES_SA_TOKEN", token_file.path());
        }

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::DecryptOwnMessage {
                ciphertext: vec![0u8; 32],
                reply,
            }
        })
        .unwrap();

        unsafe {
            std::env::remove_var("KUBERNETES_SA_TOKEN");
        }

        match result {
            Err(CryptoError::IssuanceRefused(_)) => {}
            other => panic!("expected Err(IssuanceRefused(_)), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ensure_bootstrapped_genuinely_issues_and_escrows_a_real_keypair_as_a_pure_side_effect() {
        let ((crypto_actor, _), last_stored_secret_plaintext, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: None,
                issue_agent_escrow_certificates: true,
                ..Default::default()
            })
            .await;

        let token_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(token_file.path(), "fake-k8s-service-account-token\n").unwrap();
        unsafe {
            std::env::set_var("KUBERNETES_SA_TOKEN", token_file.path());
        }

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::EnsureBootstrapped { reply }
        })
        .unwrap();

        unsafe {
            std::env::remove_var("KUBERNETES_SA_TOKEN");
        }

        assert!(result.is_ok(), "expected Ok(()), got {result:?}");

        // The real proof this is a genuine bootstrap, not a no-op:
        // a real StoreSecretRequest was actually submitted, escrowing
        // a real, newly-issued private key -- exactly the side effect
        // polar-init depends on.
        let escrowed_pem = last_stored_secret_plaintext
            .lock()
            .unwrap()
            .clone()
            .expect("a real StoreSecretRequest must have genuinely been submitted");
        RsaPrivateKey::from_pkcs8_pem(std::str::from_utf8(&escrowed_pem).unwrap())
            .expect("the escrowed plaintext must itself be a genuine, valid PKCS8 RSA private key");

        // A second call must genuinely reuse the already-bootstrapped
        // handle, not attempt issuance again -- proven by simply not
        // needing the token file (removed above) to still succeed.
        let second_result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::EnsureBootstrapped { reply }
        })
        .unwrap();
        assert!(second_result.is_ok(), "expected Ok(()), got {second_result:?}");
    }

    #[tokio::test]
    async fn encrypt_for_recipient_genuinely_round_trips_a_real_message_to_a_real_recipient_public_key() {
        let mut rng = rand::rng();
        let recipient_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let recipient_public_key_pem =
            rsa::pkcs8::EncodePublicKey::to_public_key_pem(&recipient_key.to_public_key(), rsa::pkcs8::LineEnding::LF)
                .unwrap();

        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                recipient_public_key_pem: Some(recipient_public_key_pem),
                ..Default::default()
            })
            .await;

        let plaintext = b"a real secret, addressed to a specific, real recipient identity";
        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::EncryptForRecipient {
                recipient_identity: "some-other-agent".to_string(),
                plaintext: plaintext.to_vec(),
                reply,
            }
        })
        .unwrap();

        let wire_bytes = match result {
            Ok(bytes) => bytes,
            other => panic!("expected Ok(_), got {other:?}"),
        };

        // The real proof: deserialize the actual, real wire bytes
        // EncryptForRecipient returned, and decrypt them with the
        // recipient's own, real private key -- not CryptoActor's
        // own, since this is addressed to someone else entirely.
        let escrowed: EscrowedSecret =
            rkyv::from_bytes::<EscrowedSecret, rkyv::rancor::Error>(&wire_bytes).unwrap();
        let decrypted = escrowed.decrypt(&recipient_key).unwrap();

        assert_eq!(&*decrypted, plaintext);
    }

    #[tokio::test]
    async fn encrypt_for_recipient_genuinely_caches_the_recipient_key_after_the_first_query() {
        let mut rng = rand::rng();
        let recipient_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let recipient_public_key_pem =
            rsa::pkcs8::EncodePublicKey::to_public_key_pem(&recipient_key.to_public_key(), rsa::pkcs8::LineEnding::LF)
                .unwrap();

        let ((crypto_actor, _), _, agent_escrow_key_queries) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                recipient_public_key_pem: Some(recipient_public_key_pem),
                ..Default::default()
            })
            .await;

        for _ in 0..3 {
            let result = ractor::call!(crypto_actor, |reply| {
                CryptoMessage::EncryptForRecipient {
                    recipient_identity: "some-other-agent".to_string(),
                    plaintext: b"repeated message".to_vec(),
                    reply,
                }
            })
            .unwrap();
            assert!(result.is_ok());
        }

        assert_eq!(
            agent_escrow_key_queries.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the recipient's own public key must genuinely be queried exactly once across three calls"
        );
    }

    #[tokio::test]
    async fn encrypt_for_recipient_fails_with_recipient_key_not_found_when_the_recipient_has_no_usable_key() {
        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                recipient_public_key_pem: None,
                ..Default::default()
            })
            .await;

        let result = ractor::call!(crypto_actor, |reply| {
            CryptoMessage::EncryptForRecipient {
                recipient_identity: "never-issued-agent".to_string(),
                plaintext: b"this should never be encrypted".to_vec(),
                reply,
            }
        })
        .unwrap();

        match result {
            Err(CryptoError::RecipientKeyNotFound(identity)) => {
                assert_eq!(identity, "never-issued-agent");
            }
            other => panic!("expected Err(RecipientKeyNotFound(_)), got {other:?}"),
        }
    }
}

