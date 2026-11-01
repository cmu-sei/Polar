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
//! Scope: general-secret store, encryption to another agent's escrow key (believed only for its
//! certificate, `recipient`), and opening what is sealed to this agent's own escrow key. That key is
//! the pod's custody daemon's, P-256, fetched by its custodian before the workload starts; the daemon
//! unwraps a secret's AES key for this actor and the key never enters this process
//! (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"). With no daemon there is no
//! escrow key here, and nothing sealed to it opens (`CryptoError::NoEscrowKey`). The actor also holds
//! the secrets that reach the agent, each with the binding that says where it may go (`registry`),
//! escrows them with the credentials agent, and performs named operations on them, so a caller gets
//! the result of an operation and never the secret (`RegisterSecret`, `LoadSecret`, `GitCredential`).
//!
//! Delivery: this actor registers its own reply topics
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

// Reading the process environment goes through polar-env (see clippy.toml).
#![deny(clippy::disallowed_methods)]

pub mod environment;
pub mod recipient;
pub mod registry;
pub mod workload_certificates;

use cassini_client::TcpClientMessage;
use credentials_agent_common::agent_escrow_key::{AgentEscrowKeyReply, AgentEscrowKeyRequest};
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use credentials_agent_common::escrow_secret::EscrowedSecret;
use credentials_agent_common::secret::{StoreRefusalReason, StoreSecretReply, StoreSecretRequest};
use credentials_agent_common::{
    CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC, CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC, CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC, CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
    CREDENTIALS_STORE_SECRET_REPLIES_TOPIC, CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
};
use polar::request_reply::{Envelope, ReplyRouter};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};
pub use zeroize::Zeroizing;

/// The credentials agent's request topics the actor publishes to, one for each request it makes. An agent
/// that encrypts uses them because of that need (docs/architecture/topics.md, "Implied topics").
///
/// Not the escrow keypair's retrieval or issuance: those are the custody daemon's custodian's, never the
/// workload's (docs/architecture/key-custody.md, step 9).
pub const REQUEST_TOPICS: [&str; 4] = [
    CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
    CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
];

/// Their replies, which the actor subscribes to. They are shared by every crypto actor: each takes its own
/// by correlation id.
pub const REPLY_TOPICS: [&str; 4] = [
    CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC,
    CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
    CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC,
    CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC,
];

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
    #[error("retrieve request was valid but failed: {0}")]
    RetrieveFailed(String),
    #[error("{0}")]
    KeyEncoding(String),
    #[error(
        "this pod has no custody daemon, so it holds no escrow key and nothing sealed to it opens here \
         (docs/architecture/key-custody.md)"
    )]
    NoEscrowKey,
    #[error("decrypt failed: {0}")]
    DecryptFailed(String),
    #[error("retrieving the secret was refused: {0:?}")]
    RetrieveSecretRefused(credentials_agent_common::retrieve_secret::RetrieveRefusalReason),
    #[error("policy does not let this agent have that secret: {0}")]
    RetrieveSecretDenied(String),
    #[error(
        "no usable escrow public key exists for identity {0:?} -- that agent may never have completed automated escrow-identity issuance"
    )]
    RecipientKeyNotFound(String),
    #[error("no secret is registered under {0:?}")]
    SecretNotRegistered(String),
    #[error(transparent)]
    Use(#[from] registry::UseRefusal),
    #[error(transparent)]
    Record(#[from] registry::RecordError),
}

/// What the actor read on disk at its start.
#[derive(Debug, Clone)]
struct KeyPaths {
    /// The pinned CA, which a recipient's escrow certificate must chain to; none if it could not be read, and then
    /// nothing is encrypted to a recipient.
    pinned_ca: Option<String>,
}

pub enum CryptoMessage {
    /// The Cassini client has registered (or re-registered after a
    /// reconnect). The client drops any Subscribe sent before
    /// registration completes, so this actor's reply-topic
    /// subscriptions are sent in response to this message, never at
    /// construction. Sent on every registration, so a reconnect
    /// restores them too. Subscribing again is harmless.
    ClientRegistered,
    /// A general secret to escrow -- e.g. an access token an observer
    /// holds and wants the Credentials Agent to store on its behalf.
    /// Not the agent's own escrow private key, which the custody
    /// daemon holds and this actor never sees.
    StoreSecret {
        name: String,
        plaintext: Vec<u8>,
        reply: RpcReplyPort<Result<(), CryptoError>>,
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
    /// Registers a secret that arrived in a signed document. `secret` is an `EscrowedSecret` (its `rkyv` bytes)
    /// encrypted to this identity's own escrow public key, and `context` is what it was bound to when it was made
    /// (`EscrowedSecret::encrypt_bound`): it opens only under that. It is decrypted here, with the key in its
    /// custody, and kept with its `binding`, the hosts it may be used for. Then the secret and its binding are
    /// escrowed together with the credentials agent under `<identity>/<name>`, in the background and retried
    /// until acknowledged, so a restart gets both back (`LoadSecret`). It is stored with one label, `kind`, equal
    /// to the binding's kind, which policy can speak of. The reply does not wait for that: it is
    /// `Ok` once the secret is registered, and refused, with nothing registered or escrowed, if the binding is
    /// not usable, the bytes are not an escrowed secret, or it does not open under `context`. A name registered
    /// again replaces the earlier secret and stops escrowing it. The plaintext never leaves this actor except
    /// through an operation on it, such as `GitCredential`.
    RegisterSecret {
        name: String,
        /// The runtime section the secret came from, which `ReplaceSection` speaks of.
        section: String,
        secret: Vec<u8>,
        context: Vec<u8>,
        binding: registry::SecretBinding,
        reply: RpcReplyPort<Result<(), CryptoError>>,
    },
    /// Asks the credentials agent for `<identity>/<name>` and registers what comes back, binding included, for an
    /// agent that has started again. `Ok(false)` is a name nothing is stored under; policy refusing is
    /// `Err(RetrieveSecretDenied)`. A record that is not a credential registers nothing.
    LoadSecret {
        name: String,
        /// The runtime section the secret belongs to, as for `RegisterSecret`.
        section: String,
        reply: RpcReplyPort<Result<bool, CryptoError>>,
    },
    /// A runtime section was replaced whole: drops every secret registered under `section` whose name is not in
    /// `keep`, stops escrowing it, and replies with the names dropped (sorted). Secrets of other sections, and
    /// those registered without this section, are untouched. The copy already escrowed with the credentials
    /// agent stays, since the ledger has no delete; `LoadSecret` is only asked for names a section lists.
    ReplaceSection {
        section: String,
        keep: Vec<String>,
        reply: RpcReplyPort<Vec<String>>,
    },
    /// The credential git is given to reach `host` with the registered secret `name`: a username and the secret,
    /// wiped when dropped. Refused if no secret is registered under the name, if the binding does not name the
    /// host, or if its kind or content cannot be given to git. The caller never has the secret otherwise.
    GitCredential {
        name: String,
        host: String,
        reply: RpcReplyPort<Result<registry::GitCredential, CryptoError>>,
    },
    /// Asks the credentials agent for a secret by name. It comes back encrypted to this identity's own
    /// escrow public key and is decrypted here, with the key in custody, so the plaintext is only ever
    /// in this process. `Ok(None)` means nothing is stored under the name. Whether this identity may
    /// have the secret is the credentials agent's policy to decide: a denial is
    /// `Err(RetrieveSecretDenied)`, and says nothing about whether the secret exists. The plaintext is
    /// wiped when dropped.
    RetrieveSecret {
        name: String,
        reply: RpcReplyPort<Result<Option<zeroize::Zeroizing<Vec<u8>>>, CryptoError>>,
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
    RoutedReply { topic: String, payload: Vec<u8> },
}

#[non_exhaustive]
pub struct CryptoActorArgs {
    /// The consuming agent's own, already-spawned TcpClientActor --
    /// shared, not a second connection. See
    /// credentials-agent-architecture.md's own "CryptoActor" section
    /// for why this must stay fire-and-forget in both directions.
    pub tcp_client: ActorRef<TcpClientMessage>,
    /// The consuming supervisor's own, already-spawned RouterActor --
    /// the same one its own, real topics are already registered
    /// with, not a second one. CryptoActor registers its own
    /// reply topics with it directly in pre_start, rather than
    /// subscribing to a broadcast-style OutputPort<ClientEvent> (the
    /// earlier, real design, before RouterActor existed) -- see this
    /// crate's own, top-level doc comment for the full reasoning.
    pub router: ActorRef<polar_router::RouterMessage>,
    /// The certificate, CA and custody-socket paths, already read and judged. The agent builder supplies
    /// them from its snapshot of the environment. `None` is for agents not yet on the
    /// builder: the actor then reads the process.
    pub settings: Option<environment::CryptoSettings>,
    /// How long to wait before trying to escrow a registered secret again, and the most that wait grows to.
    pub escrow_retry: (Duration, Duration),
}

impl CryptoActorArgs {
    /// Built with this and the `with_` methods, so a field added later touches no caller.
    pub fn new(
        tcp_client: ActorRef<TcpClientMessage>,
        router: ActorRef<polar_router::RouterMessage>,
    ) -> Self {
        Self {
            tcp_client,
            router,
            settings: None,
            escrow_retry: (Duration::from_secs(1), Duration::from_secs(30)),
        }
    }

    /// Escrow a registered secret again after `initial`, doubling up to `most`, until the credentials agent
    /// acknowledges it.
    pub fn with_escrow_retry(mut self, initial: Duration, most: Duration) -> Self {
        self.escrow_retry = (initial, most);
        self
    }

    /// Start on these settings instead of reading the process.
    pub fn with_settings(mut self, settings: environment::CryptoSettings) -> Self {
        self.settings = Some(settings);
        self
    }
}

pub struct CryptoActorState {
    tcp_client: ActorRef<TcpClientMessage>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    agent_escrow_key_reply_router: Arc<ReplyRouter<AgentEscrowKeyReply>>,
    retrieve_secret_reply_router:
        Arc<ReplyRouter<credentials_agent_common::retrieve_secret::RetrieveSecretReply>>,
    /// The Credentials Agent's own current escrow public key, learned
    /// on first use and reused afterward -- avoids re-querying on
    /// every single store, per credentials-agent-architecture.md's
    /// own note on CryptoActor's local caching. Arc<Mutex<_>> rather
    /// than a plain field: the actual store work runs in a spawned
    /// task, not inline in handle() (see handle()'s own StoreSecret
    /// arm for why), so this needs to be mutable from outside the
    /// actor's own sequential mailbox loop.
    cached_credentials_agent_escrow_key: Arc<tokio::sync::Mutex<Option<String>>>,
    /// The pod's custody daemon, which holds this identity's escrow key and unwraps for this actor; `None` when the
    /// pod has no daemon, and then there is no escrow key here at all.
    escrow_key: Option<Arc<polar_custody::client::CustodyClient>>,
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
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, String>>>,
    own_identity: String,
    /// What the actor read on disk at its start.
    paths: KeyPaths,
    /// The secrets that reached this agent, by name, each with the binding that says where it may go
    /// (docs/architecture/credentials-agent-architecture.md, "Named operations on registered secrets").
    registry: Registry,
    /// The task escrowing each registered secret, by name. A newer registration of the name replaces it, and
    /// every one is stopped when the actor is.
    escrow_tasks: EscrowTasks,
    escrow_retry: (Duration, Duration),
}

/// A secret and where it may be used. The secret is wiped when it is dropped.
struct RegisteredSecret {
    /// The runtime section it came from.
    section: String,
    binding: registry::SecretBinding,
    secret: zeroize::Zeroizing<Vec<u8>>,
}

type Registry = Arc<tokio::sync::Mutex<std::collections::HashMap<String, RegisteredSecret>>>;
type EscrowTasks =
    Arc<std::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>;

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
        let settings = match args.settings {
            Some(settings) => settings,
            None => environment::CryptoSettings::from_env(&polar_env::ProcessEnv)
                .map_err(ActorProcessingErr::from)?,
        };
        let own_identity = {
            let path = &settings.own_certificate_path;
            let certificate_pem = std::fs::read_to_string(path)
                .map_err(|e| ActorProcessingErr::from(format!("failed to read {path}: {e}")))?;
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
        for topic in REPLY_TOPICS {
            let myself_for_deliver = myself.clone();
            if let Err(e) = args
                .router
                .send_message(polar_router::RouterMessage::Register {
                    topic: topic.to_string(),
                    deliver: Box::new(move |incoming: polar_router::IncomingMessage| {
                        if let Err(e) =
                            myself_for_deliver.send_message(CryptoMessage::RoutedReply {
                                topic: incoming.topic,
                                payload: incoming.payload,
                            })
                        {
                            warn!("CryptoActor: failed to deliver routed reply to self: {e}");
                        }
                    }),
                })
            {
                error!("CryptoActor failed to register topic {topic} with RouterActor: {e}");
            }
        }

        // Subscriptions are not sent here: the Cassini client drops any
        // Subscribe sent before registration completes, and this actor
        // is spawned while the client is still registering. They are
        // sent on CryptoMessage::ClientRegistered instead.
        Ok(CryptoActorState {
            tcp_client: args.tcp_client,
            escrow_key_reply_router: Arc::new(ReplyRouter::new()),
            store_secret_reply_router: Arc::new(ReplyRouter::new()),
            agent_escrow_key_reply_router: Arc::new(ReplyRouter::new()),
            retrieve_secret_reply_router: Arc::new(ReplyRouter::new()),
            cached_credentials_agent_escrow_key: Arc::new(tokio::sync::Mutex::new(None)),
            escrow_key: settings
                .custody_socket
                .as_deref()
                .map(|socket| Arc::new(polar_custody::client::CustodyClient::new(socket))),
            cached_recipient_public_keys: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            own_identity,
            paths: KeyPaths {
                pinned_ca: match std::fs::read_to_string(&settings.ca_cert_path) {
                    Ok(pem) => Some(pem),
                    Err(e) => {
                        warn!(
                            path = %settings.ca_cert_path,
                            "the pinned CA cannot be read, so no recipient's key can be believed: {e}"
                        );
                        None
                    }
                },
            },
            registry: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            escrow_tasks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            escrow_retry: args.escrow_retry,
        })
    }

    /// The actor stops once, when the agent is going away: the Cassini client it talks to is
    /// a stable front that keeps its own connection up, so nothing replaces this actor. The secrets it
    /// holds are wiped, and their escrow stopped. A hard kill gets no cleanup; nothing runs.
    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        for (_, task) in state.escrow_tasks.lock().unwrap().drain() {
            task.abort();
        }
        state.registry.lock().await.clear();
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CryptoMessage::ClientRegistered => {
                // Subscribes to the *_REPLIES_TOPIC constants specifically --
                // CryptoActor is a requester here, not a responder, so it
                // needs the reply topics, not the request topics (easy to
                // transpose by accident, worth being explicit about).
                for topic in REPLY_TOPICS {
                    if let Err(e) = state.tcp_client.send_message(TcpClientMessage::Subscribe {
                        topic: topic.to_string(),
                        trace_ctx: None,
                    }) {
                        error!("CryptoActor failed to subscribe to {topic}: {e}");
                    }
                }
            }
            CryptoMessage::RoutedReply { topic, payload }
                if topic == CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<
                    Envelope<credentials_agent_common::retrieve_secret::RetrieveSecretReply>,
                    rkyv::rancor::Error,
                >(&payload)
                {
                    Ok(envelope) => state
                        .retrieve_secret_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!("CryptoActor failed to deserialize retrieve-secret reply: {e}"),
                }
            }
            CryptoMessage::RoutedReply { topic, payload }
                if topic == CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<
                    Envelope<credentials_agent_common::escrow_key::EscrowKeyReply>,
                    rkyv::rancor::Error,
                >(&payload)
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
                if topic == CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<AgentEscrowKeyReply>, rkyv::rancor::Error>(
                    &payload,
                ) {
                    Ok(envelope) => state
                        .agent_escrow_key_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => {
                        warn!("CryptoActor failed to deserialize agent escrow-key reply: {e}")
                    }
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
                        &plaintext,
                        Default::default(),
                    )
                    .await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::RegisterSecret {
                name,
                section,
                secret,
                context,
                binding,
                reply,
            } => {
                // Spawned, not awaited inline, for the same reason as the arms around it: the daemon is asked to
                // unwrap, and the secret is escrowed over the broker.
                let shared = state.shared();
                tokio::spawn(async move {
                    let result =
                        register_secret(shared, name, section, secret, context, binding).await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::LoadSecret {
                name,
                section,
                reply,
            } => {
                let shared = state.shared();
                tokio::spawn(async move {
                    let result = load_secret(shared, name, section).await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::ReplaceSection {
                section,
                keep,
                reply,
            } => {
                let mut dropped: Vec<String> = {
                    let mut registry = state.registry.lock().await;
                    let dropped: Vec<String> = registry
                        .iter()
                        .filter(|(name, held)| held.section == section && !keep.contains(name))
                        .map(|(name, _)| name.clone())
                        .collect();
                    for name in &dropped {
                        registry.remove(name);
                    }
                    dropped
                };
                dropped.sort();
                for name in &dropped {
                    if let Some(task) = state.escrow_tasks.lock().unwrap().remove(name) {
                        task.abort();
                    }
                    info!(secret = %name, %section, "secret withdrawn by its section");
                }
                let _ = reply.send(dropped);
            }
            CryptoMessage::GitCredential { name, host, reply } => {
                let result = {
                    let registry = state.registry.lock().await;
                    match registry.get(&name) {
                        None => Err(CryptoError::SecretNotRegistered(name)),
                        Some(held) => registry::git_credential(&held.binding, &held.secret, &host)
                            .map_err(CryptoError::Use),
                    }
                };
                let _ = reply.send(result);
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
                let pinned_ca = state.paths.pinned_ca.clone();
                tokio::spawn(async move {
                    let result = handle_encrypt_for_recipient(
                        tcp_client,
                        agent_escrow_key_reply_router,
                        cached_recipient_public_keys,
                        pinned_ca,
                        recipient_identity,
                        plaintext,
                    )
                    .await;
                    let _ = reply.send(result);
                });
            }
            CryptoMessage::RetrieveSecret { name, reply } => {
                // Spawned, not awaited inline, for the same reason as StoreSecret above.
                let tcp_client = state.tcp_client.clone();
                let retrieve_secret_reply_router = state.retrieve_secret_reply_router.clone();
                let escrow_key = state.escrow_key.clone();
                tokio::spawn(async move {
                    let result = handle_retrieve_secret(
                        tcp_client,
                        retrieve_secret_reply_router,
                        escrow_key,
                        name,
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
    plaintext: &[u8],
    labels: std::collections::BTreeMap<String, String>,
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

    let secret = EscrowedSecret::encrypt(plaintext, &escrow_key_pem)?;

    let reply = publish_store_secret_request(
        &tcp_client,
        &store_secret_reply_router,
        StoreSecretRequest {
            name,
            secret,
            labels,
        },
    )
    .await?;

    match reply {
        StoreSecretReply::Stored => Ok(()),
        StoreSecretReply::Refused { reason } => Err(CryptoError::Refused(reason)),
        StoreSecretReply::Failed { detail } => Err(CryptoError::StoreFailed(detail)),
    }
}

/// One request to the Credentials Agent and the wait for its reply: the exchange every call
/// above makes, written once. The reply is registered for before the request is published, so
/// one that arrives at once has somewhere to go, and the registration is removed on every way
/// out. `what` names the exchange in the log. Only identifiers and outcomes are logged, never
/// what a request or reply carries.
async fn request_reply<Req, Rep>(
    tcp_client: &ActorRef<TcpClientMessage>,
    reply_router: &Arc<ReplyRouter<Rep>>,
    topic: &str,
    what: &str,
    request: Req,
    timeout: Duration,
) -> Result<Rep, CryptoError>
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
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
        .map(|bytes| bytes.to_vec())
        .map_err(|e| CryptoError::Serialization(e.to_string()))?;

    info!(%correlation_id, %topic, "requesting: {what}");
    let began = std::time::Instant::now();
    let outcome = reply_router
        .register_with_timeout(correlation_id.clone(), timeout, || {
            tcp_client
                .send_message(TcpClientMessage::publish(topic.to_string(), payload, None))
                .map_err(|e| e.to_string())
        })
        .await;

    match outcome {
        Ok(reply) => {
            info!(%correlation_id, elapsed_ms = began.elapsed().as_millis() as u64, "reply received: {what}");
            Ok(reply)
        }
        Err(polar::request_reply::ReplyError::PublishFailed(e)) => {
            warn!(%correlation_id, %topic, "could not publish the request for {what}: {e}");
            Err(CryptoError::BrokerUnreachable(e))
        }
        Err(polar::request_reply::ReplyError::ChannelClosed) => {
            warn!(%correlation_id, "the reply channel for {what} closed with no reply");
            Err(CryptoError::NoReply)
        }
        Err(polar::request_reply::ReplyError::Timeout) => {
            warn!(
                %correlation_id,
                %topic,
                waited_secs = timeout.as_secs(),
                "no reply to {what}: nothing answered on the request topic, or its reply never \
                 reached this client"
            );
            Err(CryptoError::Timeout)
        }
    }
}

async fn query_escrow_key(
    tcp_client: &ActorRef<TcpClientMessage>,
    escrow_key_reply_router: &Arc<
        ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>,
    >,
) -> Result<String, CryptoError> {
    request_reply(
        tcp_client,
        escrow_key_reply_router,
        CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
        "escrow public key",
        EscrowKeyRequest,
        REPLY_TIMEOUT,
    )
    .await
    .map(|reply| reply.public_key_pem)
}

async fn publish_store_secret_request(
    tcp_client: &ActorRef<TcpClientMessage>,
    store_secret_reply_router: &Arc<ReplyRouter<StoreSecretReply>>,
    request: StoreSecretRequest,
) -> Result<StoreSecretReply, CryptoError> {
    request_reply(
        tcp_client,
        store_secret_reply_router,
        CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
        "store secret",
        request,
        REPLY_TIMEOUT,
    )
    .await
}

/// Opens `escrowed` under `context` with this identity's escrow key, the custody daemon's: the daemon unwraps the
/// secret's AES key and never sees the payload. With no daemon there is no key to open it with.
async fn open_escrowed(
    escrow_key: &Option<Arc<polar_custody::client::CustodyClient>>,
    escrowed: &EscrowedSecret,
    context: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, CryptoError> {
    use credentials_agent_common::escrow_secret::Suite;
    let daemon = escrow_key.clone().ok_or(CryptoError::NoEscrowKey)?;
    if escrowed.suite() != Suite::HpkeP256 {
        return Err(CryptoError::DecryptFailed(format!(
            "sealed under {:?}, and this agent's escrow key is P-256",
            escrowed.suite()
        )));
    }
    let wrapped = escrowed.wrapped_key().to_vec();
    let key = tokio::task::spawn_blocking(move || daemon.unwrap(&wrapped))
        .await
        .map_err(|e| CryptoError::DecryptFailed(e.to_string()))?
        .map_err(|e| CryptoError::DecryptFailed(format!("the custody daemon: {e}")))?;
    escrowed
        .decrypt_with_bound(Suite::HpkeP256, |_| Ok(key.to_vec()), context)
        .map_err(|e| CryptoError::DecryptFailed(e.to_string()))
}

/// What the tasks started for the registry need from the actor, cloned out of its state: they run outside the
/// mailbox, since they wait on the credentials agent.
#[derive(Clone)]
struct Shared {
    tcp_client: ActorRef<TcpClientMessage>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    retrieve_secret_reply_router:
        Arc<ReplyRouter<credentials_agent_common::retrieve_secret::RetrieveSecretReply>>,
    cached_key: Arc<tokio::sync::Mutex<Option<String>>>,
    escrow_key: Option<Arc<polar_custody::client::CustodyClient>>,
    own_identity: String,
    registry: Registry,
    escrow_tasks: EscrowTasks,
    escrow_retry: (Duration, Duration),
}

impl CryptoActorState {
    fn shared(&self) -> Shared {
        Shared {
            tcp_client: self.tcp_client.clone(),
            escrow_key_reply_router: self.escrow_key_reply_router.clone(),
            store_secret_reply_router: self.store_secret_reply_router.clone(),
            retrieve_secret_reply_router: self.retrieve_secret_reply_router.clone(),
            cached_key: self.cached_credentials_agent_escrow_key.clone(),
            escrow_key: self.escrow_key.clone(),
            own_identity: self.own_identity.clone(),
            registry: self.registry.clone(),
            escrow_tasks: self.escrow_tasks.clone(),
            escrow_retry: self.escrow_retry,
        }
    }
}

/// The name a secret is escrowed under: the identity that stores it is the prefix, so only that identity may
/// store under it (docs/architecture/credentials-policy.md, "Storing is governed by policy").
fn escrow_name(own_identity: &str, name: &str) -> String {
    format!("{own_identity}/{name}")
}

/// The real handler behind CryptoMessage::RegisterSecret: refuse an unusable binding first, then open the
/// secret, keep it with its binding, and start escrowing both.
async fn register_secret(
    shared: Shared,
    name: String,
    section: String,
    secret: Vec<u8>,
    context: Vec<u8>,
    binding: registry::SecretBinding,
) -> Result<(), CryptoError> {
    binding.check()?;
    let binding_kind = binding.kind;
    let plaintext = handle_decrypt_own_escrowed(&shared.escrow_key, secret, context).await?;
    // The form that is escrowed also proves the secret is text and the binding is whole.
    let record = registry::escrow_form(&binding, &plaintext)?;
    shared.registry.lock().await.insert(
        name.clone(),
        RegisteredSecret {
            section,
            binding,
            secret: plaintext,
        },
    );
    info!(secret = %name, "secret registered");
    // The one label a secret is stored with: what kind of credential it is, so policy can speak of it.
    let labels = std::collections::BTreeMap::from([(
        "kind".to_string(),
        binding_kind.as_label().to_string(),
    )]);
    start_escrow(&shared, name, record, labels);
    Ok(())
}

/// Escrows `record` under the identity-prefixed name in the background, until the credentials agent acknowledges
/// it, with the `kind` label policy can speak of. A failure is retried after a wait that doubles up to the most;
/// a refusal for the wrong escrow key forgets the cached key and retries; so does one because the agent's
/// certificate is not on record yet; any other refusal (policy, labels, the name) is deterministic, so it is
/// reported and not retried. A newer registration of the name replaces this task.
fn start_escrow(
    shared: &Shared,
    name: String,
    record: zeroize::Zeroizing<Vec<u8>>,
    labels: std::collections::BTreeMap<String, String>,
) {
    let task = shared.clone();
    let task_name = name.clone();
    let handle = tokio::spawn(async move {
        let escrow_name = escrow_name(&task.own_identity, &task_name);
        let (mut wait, most) = task.escrow_retry;
        loop {
            let result = handle_store_secret(
                task.tcp_client.clone(),
                task.escrow_key_reply_router.clone(),
                task.store_secret_reply_router.clone(),
                task.cached_key.clone(),
                escrow_name.clone(),
                &record,
                labels.clone(),
            )
            .await;
            match result {
                Ok(()) => {
                    info!(secret = %task_name, "secret escrowed with the credentials agent");
                    break;
                }
                Err(CryptoError::Refused(StoreRefusalReason::WrongEscrowKey)) => {
                    warn!(secret = %task_name, "the escrow key was not the current one; asking again");
                    *task.cached_key.lock().await = None;
                }
                Err(CryptoError::Refused(StoreRefusalReason::RequesterCertificateNotFound)) => {
                    // Its certificate is not on record yet, as happens just after it was issued: try again.
                    warn!(secret = %task_name, "the credentials agent has no certificate on record for this agent yet; will try again");
                }
                Err(CryptoError::Refused(reason)) => {
                    error!(secret = %task_name, ?reason, "the credentials agent refused to escrow the secret, and trying again cannot change that");
                    break;
                }
                Err(error) => {
                    warn!(secret = %task_name, %error, "escrowing the secret failed; will try again");
                }
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(most);
        }
    });
    if let Some(earlier) = shared.escrow_tasks.lock().unwrap().insert(name, handle) {
        earlier.abort();
    }
}

/// The real handler behind CryptoMessage::LoadSecret.
async fn load_secret(shared: Shared, name: String, section: String) -> Result<bool, CryptoError> {
    let found = handle_retrieve_secret(
        shared.tcp_client.clone(),
        shared.retrieve_secret_reply_router.clone(),
        shared.escrow_key.clone(),
        escrow_name(&shared.own_identity, &name),
    )
    .await?;
    let Some(record) = found else {
        return Ok(false);
    };
    let (binding, secret) = registry::from_escrow_form(&record)?;
    shared.registry.lock().await.insert(
        name,
        RegisteredSecret {
            section,
            binding,
            secret,
        },
    );
    Ok(true)
}

/// Opens an `EscrowedSecret` addressed to this identity, for `register_secret`: read the bytes as one, and open the
/// secret under `context` with this identity's escrow key, which never leaves the custody daemon. Not a message: no
/// caller has the plaintext of a registered secret.
async fn handle_decrypt_own_escrowed(
    escrow_key: &Option<Arc<polar_custody::client::CustodyClient>>,
    secret: Vec<u8>,
    context: Vec<u8>,
) -> Result<zeroize::Zeroizing<Vec<u8>>, CryptoError> {
    // Read before the key is touched: bytes that are not a secret need no daemon to be refused.
    let escrowed = rkyv::from_bytes::<EscrowedSecret, rkyv::rancor::Error>(&secret)
        .map_err(|e| CryptoError::Serialization(e.to_string()))?;
    open_escrowed(escrow_key, &escrowed, &context).await
}

/// The real handler behind CryptoMessage::RetrieveSecret: ask the credentials agent, and decrypt what
/// comes back with this identity's own escrow key, which never leaves the custody daemon. With no daemon nothing is
/// asked: what came back could not be opened.
async fn handle_retrieve_secret(
    tcp_client: ActorRef<TcpClientMessage>,
    retrieve_secret_reply_router: Arc<
        ReplyRouter<credentials_agent_common::retrieve_secret::RetrieveSecretReply>,
    >,
    escrow_key: Option<Arc<polar_custody::client::CustodyClient>>,
    name: String,
) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, CryptoError> {
    use credentials_agent_common::retrieve_secret::{RetrieveSecretReply, RetrieveSecretRequest};

    if escrow_key.is_none() {
        return Err(CryptoError::NoEscrowKey);
    }

    let reply = request_reply(
        &tcp_client,
        &retrieve_secret_reply_router,
        CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
        "retrieve secret",
        RetrieveSecretRequest { name },
        REPLY_TIMEOUT,
    )
    .await?;

    match reply {
        RetrieveSecretReply::NotFound => Ok(None),
        RetrieveSecretReply::Refused { reason } => Err(CryptoError::RetrieveSecretRefused(reason)),
        RetrieveSecretReply::Denied { reason } => Err(CryptoError::RetrieveSecretDenied(reason)),
        RetrieveSecretReply::Failed { detail } => Err(CryptoError::RetrieveFailed(detail)),
        RetrieveSecretReply::Found { secret } => {
            let plaintext = open_escrowed(&escrow_key, &secret, b"").await?;
            Ok(Some(plaintext))
        }
    }
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
    cached_recipient_public_keys: Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, String>>,
    >,
    pinned_ca: Option<String>,
    recipient_identity: String,
    plaintext: Vec<u8>,
) -> Result<Vec<u8>, CryptoError> {
    let cached = cached_recipient_public_keys
        .lock()
        .await
        .get(&recipient_identity)
        .cloned();
    let public_key_pem = match cached {
        Some(pem) => pem,
        None => {
            let reply = publish_agent_escrow_key_request(
                &tcp_client,
                &agent_escrow_key_reply_router,
                AgentEscrowKeyRequest {
                    identity: recipient_identity.clone(),
                },
            )
            .await?;
            let (public_key_pem, certificate_pem) = match reply {
                AgentEscrowKeyReply::Found {
                    public_key_pem,
                    certificate_pem,
                } => (public_key_pem, certificate_pem),
                AgentEscrowKeyReply::NotFound => {
                    return Err(CryptoError::RecipientKeyNotFound(recipient_identity));
                }
            };
            // Believed for its certificate, never for the channel it came over (docs/architecture/key-custody.md,
            // "Also in this work"): chained to the pinned CA, naming the recipient, an escrow certificate, valid,
            // holding this key.
            let pinned_ca = pinned_ca.as_deref().ok_or_else(|| {
                CryptoError::KeyEncoding(
                    "no pinned CA to believe a recipient's key under".to_string(),
                )
            })?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let believed = recipient::believed_key(
                &certificate_pem,
                &public_key_pem,
                &recipient_identity,
                pinned_ca,
                now,
            )
            .map_err(|why| {
                warn!(recipient = %recipient_identity, "a recipient's key that cannot be believed: {why}");
                CryptoError::KeyEncoding(format!("the recipient's key cannot be believed: {why}"))
            })?;
            cached_recipient_public_keys
                .lock()
                .await
                .insert(recipient_identity.clone(), believed.clone());
            believed
        }
    };

    // The suite is the key's: HPKE for a P-256 key (every agent's, held by its custody daemon), RSA-OAEP for an RSA one.
    let escrowed = EscrowedSecret::encrypt(&plaintext, &public_key_pem)?;

    rkyv::to_bytes::<rkyv::rancor::Error>(&escrowed)
        .map(|b| b.to_vec())
        .map_err(|e| CryptoError::Serialization(e.to_string()))
}

async fn publish_agent_escrow_key_request(
    tcp_client: &ActorRef<TcpClientMessage>,
    agent_escrow_key_reply_router: &Arc<ReplyRouter<AgentEscrowKeyReply>>,
    request: AgentEscrowKeyRequest,
) -> Result<AgentEscrowKeyReply, CryptoError> {
    request_reply(
        tcp_client,
        agent_escrow_key_reply_router,
        CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
        "agent escrow key",
        request,
        REPLY_TIMEOUT,
    )
    .await
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

    /// An actor that takes whatever it is sent and says nothing: a client whose request goes
    /// out and is never answered.
    struct Silent;

    #[async_trait]
    impl Actor for Silent {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<(), ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            _message: Self::Msg,
            _state: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_request_nobody_answers_times_out_and_leaves_nothing_registered() {
        let (client, _) = Actor::spawn(None, Silent, ()).await.unwrap();
        let router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>> =
            Arc::new(ReplyRouter::new());
        let outcome = request_reply(
            &client,
            &router,
            CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
            "test",
            EscrowKeyRequest,
            Duration::from_millis(100),
        )
        .await;
        assert!(matches!(outcome, Err(CryptoError::Timeout)), "{outcome:?}");
    }

    #[tokio::test]
    async fn a_request_to_a_client_that_is_gone_fails_at_once_as_unreachable() {
        let (client, _) = Actor::spawn(None, Silent, ()).await.unwrap();
        client.stop_and_wait(None, None).await.unwrap();
        let router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>> =
            Arc::new(ReplyRouter::new());
        let outcome = request_reply(
            &client,
            &router,
            CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
            "test",
            EscrowKeyRequest,
            Duration::from_secs(30),
        )
        .await;
        assert!(
            matches!(outcome, Err(CryptoError::BrokerUnreachable(_))),
            "{outcome:?}"
        );
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
        /// The escrow key the actor's custody daemon holds (its PKCS#8), which a retrievable secret is encrypted
        /// to, as the real agent does. If None, a retrieval finds nothing.
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
        /// If Some, AgentEscrowKeyRequests are answered with a real
        /// Found reply carrying this PEM -- a real, distinct
        /// recipient identity's own public key, standing in for
        /// EncryptForRecipient's own, real target. If None, answered
        /// with NotFound.
        recipient_public_key_pem: Option<String>,
        /// The recipient's key is handed out in a certificate another CA signed, as a forger would.
        forge_recipient_certificate: bool,
        /// Real, shared call count for the agent-escrow-key branch
        /// specifically -- same reasoning as escrow_key_queries,
        /// above, but for EncryptForRecipient's own, separate cache.
        agent_escrow_key_queries: Arc<std::sync::atomic::AtomicUsize>,
        /// Secrets the agent will hand back to RetrieveSecretRequests, by name, each encrypted to the
        /// escrow key the actor under test holds (`stored_escrow_keypair_pem`), as the real one does.
        retrievable_secrets: Vec<(String, Vec<u8>)>,
        /// If true every RetrieveSecretRequest is answered Denied, as policy would.
        deny_retrieval: bool,
        /// Every StoreSecretRequest, as (name, decrypted plaintext), in the order they arrived and whatever
        /// they were answered.
        stored: Option<StoreLog>,
        /// While true every store is answered Failed, as when the ledger cannot be appended to.
        failing_stores: Option<Arc<std::sync::atomic::AtomicBool>>,
        /// How the first stores are answered, in order; after that, Stored.
        store_script: std::sync::Mutex<std::collections::VecDeque<StoreScript>>,
    }

    /// What the fake credentials agent was asked to store, as (name, decrypted plaintext, labels), in arrival order.
    type StoreLog =
        Arc<std::sync::Mutex<Vec<(String, Vec<u8>, std::collections::BTreeMap<String, String>)>>>;

    /// A scripted answer to a store.
    #[derive(Clone, Copy)]
    enum StoreScript {
        Failed,
        Refused(StoreRefusalReason),
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
                        published_instance: None,
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
                    if let Some(stored) = &self.stored {
                        stored.lock().unwrap().push((
                            envelope.payload.name.clone(),
                            decrypted.to_vec(),
                            envelope.payload.labels.clone(),
                        ));
                    }
                    let failing = self
                        .failing_stores
                        .as_ref()
                        .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst));
                    let scripted = self.store_script.lock().unwrap().pop_front();
                    let answer = match (failing, scripted) {
                        (true, _) | (_, Some(StoreScript::Failed)) => StoreSecretReply::Failed {
                            detail: "scripted".to_string(),
                        },
                        (_, Some(StoreScript::Refused(reason))) => {
                            StoreSecretReply::Refused { reason }
                        }
                        (false, None) => StoreSecretReply::Stored,
                    };

                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: answer,
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    route(CREDENTIALS_STORE_SECRET_REPLIES_TOPIC, bytes);
                }
            } else if topic == CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC {
                if let Ok(envelope) = rkyv::from_bytes::<
                    Envelope<credentials_agent_common::retrieve_secret::RetrieveSecretRequest>,
                    rkyv::rancor::Error,
                >(&payload)
                {
                    use credentials_agent_common::retrieve_secret::RetrieveSecretReply;
                    let found = self
                        .retrievable_secrets
                        .iter()
                        .find(|(name, _)| *name == envelope.payload.name);
                    let answer = if self.deny_retrieval {
                        RetrieveSecretReply::Denied {
                            reason: "policy says no".to_string(),
                        }
                    } else if let (Some((_, plaintext)), Some(pem)) =
                        (found, &self.stored_escrow_keypair_pem)
                    {
                        // To the actor's own escrow key, as the real agent does: P-256, the custody daemon's.
                        let public = polar_custody::escrow::EscrowKey::from_pkcs8_pem(pem)
                            .unwrap()
                            .public_key_pem()
                            .unwrap();
                        RetrieveSecretReply::Found {
                            secret: EscrowedSecret::encrypt(plaintext, &public).unwrap(),
                        }
                    } else {
                        RetrieveSecretReply::NotFound
                    };
                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: answer,
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    route(CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC, bytes);
                }
            } else if topic == CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC
                && let Ok(envelope) = rkyv::from_bytes::<
                    Envelope<AgentEscrowKeyRequest>,
                    rkyv::rancor::Error,
                >(&payload)
            {
                self.agent_escrow_key_queries
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let reply_payload = match &self.recipient_public_key_pem {
                    // The key, in the escrow certificate cert-issuer would have signed for the recipient.
                    Some(pem) => AgentEscrowKeyReply::Found {
                        public_key_pem: pem.clone(),
                        certificate_pem: escrow_certificate(
                            &envelope.payload.identity,
                            pem,
                            if self.forge_recipient_certificate {
                                forger_ca()
                            } else {
                                test_ca()
                            },
                        ),
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

            Ok(())
        }
    }

    async fn spawn_crypto_actor_with_fake_credentials_agent()
    -> (ActorRef<CryptoMessage>, Arc<std::sync::atomic::AtomicUsize>) {
        spawn_crypto_actor_with_fake_credentials_agent_configured(
            FakeCredentialsAgentConfig::default(),
        )
        .await
        .0
    }

    /// Configures the shared FakeCredentialsAgent's own, real behavior
    /// -- see each field's own doc comment (on FakeCredentialsAgent
    /// itself) for exactly what each one controls.
    #[derive(Default)]
    struct FakeCredentialsAgentConfig {
        stored_escrow_keypair_pem: Option<String>,
        recipient_public_key_pem: Option<String>,
        forge_recipient_certificate: bool,
        /// The custody daemon's socket, when the actor's pod has one: without it the actor has no escrow key.
        custody_socket: Option<String>,
        /// The identity in the actor's certificate; unique per actor when not given.
        identity: Option<String>,
        /// Secrets the fake credentials agent will hand back, by name.
        retrievable_secrets: Vec<(String, Vec<u8>)>,
        /// Every retrieval is denied by policy.
        deny_retrieval: bool,
        stored: Option<StoreLog>,
        failing_stores: Option<Arc<std::sync::atomic::AtomicBool>>,
        store_script: Vec<StoreScript>,
    }

    /// pre_start now derives its own identity by reading a real,
    /// mounted TLS_CLIENT_CERT -- this builds a real, temporary,
    /// self-signed certificate carrying "test-agent" as its own
    /// identity SAN (mirroring extract_own_identity's own, real
    /// test fixtures directly), writes it to a real, temporary file,
    /// and hands the actor its path in its settings, so every test in this module
    /// continues to genuinely exercise "test-agent" as this actor's
    /// own identity, exactly as before this migration.
    /// The CA the test actor pins, and one that is not it.
    fn test_ca() -> &'static (rcgen::Issuer<'static, rcgen::KeyPair>, String) {
        static CA: std::sync::OnceLock<(rcgen::Issuer<'static, rcgen::KeyPair>, String)> =
            std::sync::OnceLock::new();
        CA.get_or_init(new_ca)
    }

    fn forger_ca() -> &'static (rcgen::Issuer<'static, rcgen::KeyPair>, String) {
        static CA: std::sync::OnceLock<(rcgen::Issuer<'static, rcgen::KeyPair>, String)> =
            std::sync::OnceLock::new();
        CA.get_or_init(new_ca)
    }

    fn new_ca() -> (rcgen::Issuer<'static, rcgen::KeyPair>, String) {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Polar Internal CA");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        (rcgen::Issuer::new(params, key), pem)
    }

    /// Where the test CA is written for the actor to pin.
    fn test_ca_file() -> std::path::PathBuf {
        static FILE: std::sync::OnceLock<tempfile::TempPath> = std::sync::OnceLock::new();
        FILE.get_or_init(|| {
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), &test_ca().1).unwrap();
            file.into_temp_path()
        })
        .to_path_buf()
    }

    /// An escrow certificate (`CertType::Service`) `ca` signs for `identity`, holding `public_key_pem`.
    fn escrow_certificate(
        identity: &str,
        public_key_pem: &str,
        ca: &(rcgen::Issuer<'static, rcgen::KeyPair>, String),
    ) -> String {
        let mut params = rcgen::CertificateParams::new(vec![identity.to_string()]).unwrap();
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::EmailProtection];
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::minutes(1);
        params.not_after = now + time::Duration::hours(1);
        let subject = rcgen::SubjectPublicKeyInfo::from_pem(public_key_pem).unwrap();
        params.signed_by(&subject, &ca.0).unwrap().pem()
    }

    fn write_test_agent_certificate(identity: &str) -> tempfile::NamedTempFile {
        let mut rng = rand::rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let private_key_pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params = rcgen::CertificateParams::new(vec![identity.to_string()]).unwrap();
        let certificate_pem = params.self_signed(&key_pair).unwrap().pem();

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), certificate_pem).unwrap();
        file
    }

    async fn spawn_crypto_actor_with_fake_credentials_agent_configured(
        config: FakeCredentialsAgentConfig,
    ) -> (
        (ActorRef<CryptoMessage>, Arc<std::sync::atomic::AtomicUsize>),
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
                recipient_public_key_pem: config.recipient_public_key_pem,
                forge_recipient_certificate: config.forge_recipient_certificate,
                agent_escrow_key_queries: agent_escrow_key_queries.clone(),
                retrievable_secrets: config.retrievable_secrets,
                deny_retrieval: config.deny_retrieval,
                stored: config.stored,
                failing_stores: config.failing_stores,
                store_script: std::sync::Mutex::new(config.store_script.into_iter().collect()),
            },
            (),
        )
        .await
        .unwrap();

        // Each actor gets an identity of its own, so the names its secrets are escrowed under are its own.
        let identity = config
            .identity
            .clone()
            .unwrap_or_else(|| format!("test-agent-{}", uuid::Uuid::new_v4()));
        let own_cert_file = write_test_agent_certificate(&identity);

        let (crypto_actor, _) = Actor::spawn(
            None,
            CryptoActor,
            CryptoActorArgs::new(fake_agent, router)
                .with_settings(environment::CryptoSettings {
                    own_certificate_path: own_cert_file.path().to_string_lossy().into_owned(),
                    custody_socket: config.custody_socket.clone(),
                    ca_cert_path: test_ca_file().to_string_lossy().into_owned(),
                })
                .with_escrow_retry(
                    std::time::Duration::from_millis(5),
                    std::time::Duration::from_millis(20),
                ),
        )
        .await
        .unwrap();

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

    /// Stops the actor and waits for it, so its `post_stop` has wiped what it held before the test ends.
    async fn stop_and_release_key(actor: &ActorRef<CryptoMessage>) {
        actor
            .stop_and_wait(None, Some(std::time::Duration::from_secs(5)))
            .await
            .unwrap();
    }

    /// A custody daemon on a socket in a directory of its own, as a pod's is, whose custodian has fetched the escrow
    /// key: P-256, as every agent's is (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"). The
    /// directory is returned to be kept for as long as the socket is used; the key's PKCS#8 is what the fake
    /// credentials agent encrypts a retrievable secret to, as the real one encrypts to the escrow certificate's key.
    fn custody_daemon_holding_an_escrow_key() -> CustodyDaemon {
        use polar_custody::custody::{Custody as Daemon, Roles};
        use polar_custody::protocol::{Reply, Request, Slot};
        use polar_custody::server::{Callers, bind, serve};
        use std::os::unix::fs::MetadataExt as _;
        let (issuer, ca_pem) = new_ca();
        let mut custody = Daemon::new(&ca_pem).unwrap();
        let installer = Roles {
            installer: true,
            workload: false,
        };
        let workload = "test-agent-daemon.polar.serviceaccount.cluster.local";
        let Reply::Csr(csr) = custody.handle(
            installer,
            Request::MakeCsr {
                slot: Slot::Custodian,
                identity: format!("custody.{workload}"),
                names: vec![],
            },
            0,
        ) else {
            panic!("a CSR")
        };
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        request
            .params
            .subject_alt_names
            .push(rcgen::SanType::URI("p:custody:escrow".try_into().unwrap()));
        let at = time::OffsetDateTime::now_utc();
        request.params.not_before = at - time::Duration::minutes(1);
        request.params.not_after = at + time::Duration::hours(1);
        let now = at.unix_timestamp();
        let certificate = request.signed_by(&issuer).unwrap().pem();
        assert_eq!(
            custody.handle(
                installer,
                Request::Install {
                    slot: Slot::Custodian,
                    chain_pem: vec![certificate]
                },
                now
            ),
            Reply::Installed
        );
        assert_eq!(
            custody.handle(
                installer,
                Request::Bootstrap {
                    identity: workload.to_string(),
                    client: false,
                    server: None,
                    escrow: true,
                },
                now,
            ),
            Reply::Installed
        );
        // A P-256 key, as the custodian would have fetched.
        let pkcs8 = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .unwrap()
            .serialize_pem();
        let public_pem = polar_custody::escrow::EscrowKey::from_pkcs8_pem(&pkcs8)
            .unwrap()
            .public_key_pem()
            .unwrap();
        custody.hold_retrieved_escrow(&pkcs8).unwrap();
        custody.set_ready();
        let dir = tempfile::TempDir::new().unwrap();
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        let socket = dir.path().join("custody.sock");
        tokio::spawn(serve(
            bind(&socket).unwrap(),
            Arc::new(std::sync::RwLock::new(custody)),
            Arc::new(Callers {
                installer_uid: uid,
                workload_uids: vec![uid],
            }),
        ));
        CustodyDaemon {
            _dir: dir,
            socket: socket.display().to_string(),
            pkcs8,
            public_pem,
        }
    }

    /// A custody daemon serving, for as long as this is kept.
    struct CustodyDaemon {
        _dir: tempfile::TempDir,
        socket: String,
        pkcs8: String,
        public_pem: String,
    }

    /// An actor whose escrow key its custody daemon holds, beside a credentials agent holding `secrets` for it.
    async fn crypto_actor_holding_an_escrow_key(
        secrets: &[(&str, &[u8])],
        deny_retrieval: bool,
    ) -> (ActorRef<CryptoMessage>, CustodyDaemon) {
        let daemon = custody_daemon_holding_an_escrow_key();
        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: Some(daemon.pkcs8.clone()),
                custody_socket: Some(daemon.socket.clone()),
                retrievable_secrets: secrets
                    .iter()
                    .map(|(n, v)| (n.to_string(), v.to_vec()))
                    .collect(),
                deny_retrieval,
                ..Default::default()
            })
            .await;
        (crypto_actor, daemon)
    }

    /// docs/architecture/key-custody.md, "Part A's bootstrap: the custodian": the escrow key is the custody daemon's,
    /// P-256; a secret retrieved is opened through it, the actor never holding the key, and a secret sealed under
    /// RSA-OAEP, to a key no agent holds now, is refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn with_a_custody_daemon_a_retrieved_secret_is_opened_through_it_and_an_rsa_sealed_one_is_refused()
     {
        let (crypto_actor, _daemon) =
            crypto_actor_holding_an_escrow_key(&[("github-sei", b"a token")], false).await;
        let opened = ractor::call!(crypto_actor, |reply| CryptoMessage::RetrieveSecret {
            name: "github-sei".to_string(),
            reply,
        })
        .unwrap();
        assert_eq!(
            opened.unwrap().as_deref().map(|v| v.as_slice()),
            Some(&b"a token"[..])
        );
        let rsa = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
        let rsa_public = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &rsa.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let sealed = rkyv::to_bytes::<rkyv::rancor::Error>(
            &EscrowedSecret::encrypt_bound(b"a token", &rsa_public, b"ctx").unwrap(),
        )
        .unwrap()
        .to_vec();
        let refused = ractor::call!(crypto_actor, |reply| CryptoMessage::RegisterSecret {
            name: "old".to_string(),
            section: "s".to_string(),
            secret: sealed,
            context: b"ctx".to_vec(),
            binding: token_binding("github.com"),
            reply,
        })
        .unwrap();
        assert!(
            matches!(&refused, Err(CryptoError::DecryptFailed(why)) if why.contains("P-256")),
            "{refused:?}"
        );
        crypto_actor.stop_and_wait(None, None).await.unwrap();
    }

    /// With no custody daemon there is no escrow key (docs/architecture/key-custody.md, step 9): nothing sealed to it
    /// opens, the actor says why at once, and it never asks the credentials agent for the key itself, which only the
    /// custodian may (a request it asked would wait out its reply timeout, unanswered).
    #[tokio::test]
    async fn without_a_custody_daemon_nothing_sealed_to_the_escrow_key_opens_and_the_key_is_never_asked_for()
     {
        let daemon = custody_daemon_holding_an_escrow_key();
        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: Some(daemon.pkcs8.clone()),
                // Nothing held: a retrieval asked would come back NotFound, not refused.
                ..Default::default()
            })
            .await;
        let at_once = std::time::Duration::from_secs(5);
        let retrieved = tokio::time::timeout(at_once, async {
            ractor::call!(crypto_actor, |reply| CryptoMessage::RetrieveSecret {
                name: "github-sei".to_string(),
                reply,
            })
        })
        .await
        .expect("refused at once")
        .unwrap();
        assert!(
            matches!(retrieved, Err(CryptoError::NoEscrowKey)),
            "{retrieved:?}"
        );
        let sealed = rkyv::to_bytes::<rkyv::rancor::Error>(
            &EscrowedSecret::encrypt_bound(b"a token", &daemon.public_pem, b"ctx").unwrap(),
        )
        .unwrap()
        .to_vec();
        let registered = tokio::time::timeout(at_once, async {
            ractor::call!(crypto_actor, |reply| CryptoMessage::RegisterSecret {
                name: "github-sei".to_string(),
                section: "s".to_string(),
                secret: sealed,
                context: b"ctx".to_vec(),
                binding: token_binding("github.com"),
                reply,
            })
        })
        .await
        .expect("refused at once")
        .unwrap();
        assert!(
            matches!(registered, Err(CryptoError::NoEscrowKey)),
            "{registered:?}"
        );
        let git = ractor::call!(crypto_actor, |reply| CryptoMessage::GitCredential {
            name: "github-sei".to_string(),
            host: "github.com".to_string(),
            reply,
        })
        .unwrap();
        assert!(
            matches!(git, Err(CryptoError::SecretNotRegistered(_))),
            "nothing registered"
        );
        stop_and_release_key(&crypto_actor).await;
    }

    #[tokio::test]
    async fn retrieve_secret_returns_what_the_credentials_agent_encrypted_to_the_escrow_key() {
        let (crypto, _daemon) =
            crypto_actor_holding_an_escrow_key(&[("github-sei", b"a token")], false).await;
        let result = ractor::call!(crypto, |reply| CryptoMessage::RetrieveSecret {
            name: "github-sei".to_string(),
            reply,
        })
        .unwrap();
        assert_eq!(
            result.unwrap().as_deref().map(|v| v.as_slice()),
            Some(&b"a token"[..])
        );
        stop_and_release_key(&crypto).await;
    }

    #[tokio::test]
    async fn retrieve_secret_for_a_name_never_stored_is_none() {
        let (crypto, _daemon) =
            crypto_actor_holding_an_escrow_key(&[("github-sei", b"a token")], false).await;
        let result = ractor::call!(crypto, |reply| CryptoMessage::RetrieveSecret {
            name: "other".to_string(),
            reply,
        })
        .unwrap();
        assert!(matches!(result, Ok(None)), "{result:?}");
        stop_and_release_key(&crypto).await;
    }

    #[tokio::test]
    async fn retrieve_secret_that_policy_denies_is_an_error_that_says_so() {
        let (crypto, _daemon) =
            crypto_actor_holding_an_escrow_key(&[("github-sei", b"a token")], true).await;
        let result = ractor::call!(crypto, |reply| CryptoMessage::RetrieveSecret {
            name: "github-sei".to_string(),
            reply,
        })
        .unwrap();
        assert!(
            matches!(result, Err(CryptoError::RetrieveSecretDenied(_))),
            "{result:?}"
        );
        stop_and_release_key(&crypto).await;
    }

    #[tokio::test]
    async fn store_secret_round_trips_through_a_real_encrypt_and_a_real_fake_broker() {
        let (crypto_actor, _escrow_key_queries) =
            spawn_crypto_actor_with_fake_credentials_agent().await;

        let result = ractor::call!(crypto_actor, |reply| CryptoMessage::StoreSecret {
            name: "gitlab-token".to_string(),
            plaintext: b"a real secret from a real actor".to_vec(),
            reply,
        })
        .unwrap();

        assert!(result.is_ok(), "expected Ok(()), got {result:?}");
        stop_and_release_key(&crypto_actor).await;
    }

    #[tokio::test]
    async fn escrow_key_is_genuinely_cached_after_the_first_store() {
        let (crypto_actor, escrow_key_queries) =
            spawn_crypto_actor_with_fake_credentials_agent().await;

        for _ in 0..3 {
            let result = ractor::call!(crypto_actor, |reply| CryptoMessage::StoreSecret {
                name: "gitlab-token".to_string(),
                plaintext: b"a real secret from a real actor".to_vec(),
                reply,
            })
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
        stop_and_release_key(&crypto_actor).await;
    }
    // ---- registered secrets, and what is done with them ---------------------------------------------------------

    use registry::{GitCredential, RecordError, SecretBinding, SecretKind, UseRefusal};

    /// An actor whose custody daemon holds an escrow key a test can encrypt to, beside a credentials agent that records
    /// every store.
    struct Rig {
        actor: ActorRef<CryptoMessage>,
        _daemon: CustodyDaemon,
        public_pem: String,
        identity: String,
        stored: StoreLog,
        failing: Arc<std::sync::atomic::AtomicBool>,
        key_queries: Arc<std::sync::atomic::AtomicUsize>,
    }

    /// `held` are secrets the credentials agent holds for this identity, by short name.
    async fn rig(
        script: Vec<StoreScript>,
        held: Vec<(&str, Vec<u8>)>,
        deny_retrieval: bool,
    ) -> Rig {
        let daemon = custody_daemon_holding_an_escrow_key();
        let public_pem = daemon.public_pem.clone();
        let identity = format!("test-agent-{}", uuid::Uuid::new_v4());
        let stored = Arc::new(std::sync::Mutex::new(Vec::new()));
        let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let retrievable_secrets = held
            .into_iter()
            .map(|(name, bytes)| (format!("{identity}/{name}"), bytes))
            .collect();
        let ((actor, key_queries), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                stored_escrow_keypair_pem: Some(daemon.pkcs8.clone()),
                custody_socket: Some(daemon.socket.clone()),
                identity: Some(identity.clone()),
                retrievable_secrets,
                deny_retrieval,
                stored: Some(stored.clone()),
                failing_stores: Some(failing.clone()),
                store_script: script,
                ..Default::default()
            })
            .await;
        Rig {
            actor,
            _daemon: daemon,
            public_pem,
            identity,
            stored,
            failing,
            key_queries,
        }
    }

    fn escrowed_bytes(rig: &Rig, plaintext: &[u8], context: &[u8]) -> Vec<u8> {
        let escrowed = EscrowedSecret::encrypt_bound(plaintext, &rig.public_pem, context).unwrap();
        rkyv::to_bytes::<rkyv::rancor::Error>(&escrowed)
            .unwrap()
            .to_vec()
    }

    fn token_binding(host: &str) -> SecretBinding {
        SecretBinding {
            host_pattern: host.to_string(),
            kind: SecretKind::Token,
            username: None,
        }
    }

    /// The section the tests register under unless a test says otherwise.
    const SECTION: &str = "credentials";

    async fn register_in(
        rig: &Rig,
        section: &str,
        name: &str,
        plaintext: &[u8],
        context: &[u8],
        binding: SecretBinding,
    ) -> Result<(), CryptoError> {
        let secret = escrowed_bytes(rig, plaintext, context);
        ractor::call!(rig.actor, |reply| CryptoMessage::RegisterSecret {
            name: name.to_string(),
            section: section.to_string(),
            secret,
            context: context.to_vec(),
            binding,
            reply,
        })
        .unwrap()
    }

    async fn replace_section(rig: &Rig, section: &str, keep: &[&str]) -> Vec<String> {
        ractor::call!(rig.actor, |reply| CryptoMessage::ReplaceSection {
            section: section.to_string(),
            keep: keep.iter().map(|k| k.to_string()).collect(),
            reply,
        })
        .unwrap()
    }

    async fn register(
        rig: &Rig,
        name: &str,
        plaintext: &[u8],
        context: &[u8],
        binding: SecretBinding,
    ) -> Result<(), CryptoError> {
        let secret = escrowed_bytes(rig, plaintext, context);
        ractor::call!(rig.actor, |reply| CryptoMessage::RegisterSecret {
            name: name.to_string(),
            section: SECTION.to_string(),
            secret,
            context: context.to_vec(),
            binding,
            reply,
        })
        .unwrap()
    }

    async fn git_credential(
        rig: &Rig,
        name: &str,
        host: &str,
    ) -> Result<GitCredential, CryptoError> {
        ractor::call!(rig.actor, |reply| CryptoMessage::GitCredential {
            name: name.to_string(),
            host: host.to_string(),
            reply,
        })
        .unwrap()
    }

    async fn load(rig: &Rig, name: &str) -> Result<bool, CryptoError> {
        ractor::call!(rig.actor, |reply| CryptoMessage::LoadSecret {
            name: name.to_string(),
            section: SECTION.to_string(),
            reply,
        })
        .unwrap()
    }

    async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..600 {
            if condition() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    fn stored_len(rig: &Rig) -> usize {
        rig.stored.lock().unwrap().len()
    }

    fn secret_of(record: &[u8]) -> String {
        String::from_utf8(registry::from_escrow_form(record).unwrap().1.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn a_registered_secret_is_given_to_git_for_a_bound_host_and_never_for_another() {
        let r = rig(vec![], vec![], false).await;
        register(
            &r,
            "github",
            b"ghp-token",
            b"ctx-1",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        let got = git_credential(&r, "github", "github.com").await.unwrap();
        assert_eq!(got.username, "oauth2");
        assert_eq!(got.secret.as_slice(), b"ghp-token");
        for host in ["evil.example.org", "github.com.evil.org", "github.com:443"] {
            let refused = git_credential(&r, "github", host).await;
            assert!(
                matches!(refused, Err(CryptoError::Use(UseRefusal::HostNotBound))),
                "{host}: {refused:?}"
            );
        }
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn a_secret_nothing_registered_is_refused_by_name() {
        let r = rig(vec![], vec![], false).await;
        let refused = git_credential(&r, "github", "github.com").await;
        assert!(
            matches!(refused, Err(CryptoError::SecretNotRegistered(_))),
            "{refused:?}"
        );
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn a_secret_made_for_another_context_is_refused_and_nothing_is_registered_or_escrowed() {
        let r = rig(vec![], vec![], false).await;
        let secret = escrowed_bytes(
            &r,
            b"ghp-token",
            b"git-repo-observer|credentials|1|github|secret",
        );
        let refused = ractor::call!(r.actor, |reply| CryptoMessage::RegisterSecret {
            name: "github".to_string(),
            section: SECTION.to_string(),
            secret: secret.clone(),
            context: b"git-repo-observer|credentials|1|other|secret".to_vec(),
            binding: token_binding("github.com"),
            reply,
        })
        .unwrap();
        assert!(
            matches!(refused, Err(CryptoError::DecryptFailed(_))),
            "{refused:?}"
        );
        assert!(matches!(
            git_credential(&r, "github", "github.com").await,
            Err(CryptoError::SecretNotRegistered(_))
        ));
        settle().await;
        assert_eq!(stored_len(&r), 0);
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn bytes_that_are_not_an_escrowed_secret_are_a_serialization_error_and_register_nothing()
    {
        let r = rig(vec![], vec![], false).await;
        let refused = ractor::call!(r.actor, |reply| CryptoMessage::RegisterSecret {
            name: "github".to_string(),
            section: SECTION.to_string(),
            secret: b"not an escrowed secret".to_vec(),
            context: b"ctx".to_vec(),
            binding: token_binding("github.com"),
            reply,
        })
        .unwrap();
        assert!(
            matches!(refused, Err(CryptoError::Serialization(_))),
            "{refused:?}"
        );
        assert!(git_credential(&r, "github", "github.com").await.is_err());
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn a_binding_that_is_not_usable_is_refused_and_registers_and_escrows_nothing() {
        let r = rig(vec![], vec![], false).await;
        let refused = register(&r, "github", b"ghp-token", b"ctx", token_binding("*")).await;
        assert!(
            matches!(
                refused,
                Err(CryptoError::Record(RecordError::BadBinding(_)))
            ),
            "{refused:?}"
        );
        assert!(git_credential(&r, "github", "github.com").await.is_err());
        settle().await;
        assert_eq!(stored_len(&r), 0);
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn a_user_and_password_without_a_username_is_held_but_not_given_to_git() {
        let r = rig(vec![], vec![], false).await;
        let binding = SecretBinding {
            host_pattern: "git.example.org".to_string(),
            kind: SecretKind::UserPassword,
            username: None,
        };
        register(&r, "internal", b"hunter2", b"ctx", binding)
            .await
            .unwrap();
        let refused = git_credential(&r, "internal", "git.example.org").await;
        assert!(
            matches!(refused, Err(CryptoError::Use(UseRefusal::NoUsername))),
            "{refused:?}"
        );
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn registering_escrows_the_secret_with_its_binding_under_the_identity_prefixed_name() {
        let r = rig(vec![], vec![], false).await;
        let binding = SecretBinding {
            host_pattern: "github.com".to_string(),
            kind: SecretKind::Token,
            username: Some("daveman1010221".to_string()),
        };
        register(&r, "github", b"ghp-token", b"ctx", binding.clone())
            .await
            .unwrap();
        wait_until("the escrow", || stored_len(&r) >= 1).await;
        let (name, record, _labels) = r.stored.lock().unwrap()[0].clone();
        assert_eq!(name, format!("{}/github", r.identity));
        let (escrowed_binding, secret) = registry::from_escrow_form(&record).unwrap();
        assert_eq!(escrowed_binding, binding);
        assert_eq!(secret.as_slice(), b"ghp-token");
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn registering_does_not_wait_for_the_escrow_and_the_secret_is_usable_meanwhile() {
        let r = rig(vec![], vec![], false).await;
        r.failing.store(true, std::sync::atomic::Ordering::SeqCst);
        register(
            &r,
            "github",
            b"ghp-token",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        assert!(git_credential(&r, "github", "github.com").await.is_ok());
        wait_until("a retry", || stored_len(&r) >= 2).await;
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn an_escrow_that_fails_is_retried_until_it_is_acknowledged_and_then_stops() {
        let r = rig(
            vec![StoreScript::Failed, StoreScript::Failed],
            vec![],
            false,
        )
        .await;
        register(
            &r,
            "github",
            b"ghp-token",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        wait_until("three attempts", || stored_len(&r) >= 3).await;
        settle().await;
        assert_eq!(
            stored_len(&r),
            3,
            "two failures, then the acknowledgement, then nothing more"
        );
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn an_escrow_refused_for_a_reason_retrying_cannot_fix_is_not_retried() {
        for reason in [
            StoreRefusalReason::NameTaken,
            StoreRefusalReason::AnonymousPublisherNotAllowed,
        ] {
            let r = rig(vec![StoreScript::Refused(reason)], vec![], false).await;
            register(
                &r,
                "github",
                b"ghp-token",
                b"ctx",
                token_binding("github.com"),
            )
            .await
            .unwrap();
            wait_until("the attempt", || stored_len(&r) >= 1).await;
            settle().await;
            assert_eq!(stored_len(&r), 1, "{reason:?}");
            // It is still held, and usable, in this agent.
            assert!(git_credential(&r, "github", "github.com").await.is_ok());
            stop_and_release_key(&r.actor).await;
        }
    }

    #[tokio::test]
    async fn an_escrow_refused_for_the_wrong_escrow_key_is_retried_with_the_current_key() {
        let r = rig(
            vec![StoreScript::Refused(StoreRefusalReason::WrongEscrowKey)],
            vec![],
            false,
        )
        .await;
        register(
            &r,
            "github",
            b"ghp-token",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        wait_until("a second attempt", || stored_len(&r) >= 2).await;
        settle().await;
        assert_eq!(stored_len(&r), 2);
        assert!(
            r.key_queries.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "the cached key was kept after the credentials agent said it was wrong"
        );
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn registering_a_name_again_replaces_it_and_stops_escrowing_the_old_value() {
        let r = rig(vec![], vec![], false).await;
        r.failing.store(true, std::sync::atomic::Ordering::SeqCst);
        register(
            &r,
            "github",
            b"old-token",
            b"ctx-1",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        wait_until("an attempt for the old value", || stored_len(&r) >= 1).await;
        register(
            &r,
            "github",
            b"new-token",
            b"ctx-2",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        r.failing.store(false, std::sync::atomic::Ordering::SeqCst);
        wait_until("the new value escrowed", || {
            r.stored
                .lock()
                .unwrap()
                .last()
                .is_some_and(|(_, record, _)| secret_of(record) == "new-token")
        })
        .await;
        settle().await;
        let entries = r.stored.lock().unwrap().clone();
        let first_new = entries
            .iter()
            .position(|(_, record, _)| secret_of(record) == "new-token")
            .unwrap();
        assert!(
            entries[first_new..]
                .iter()
                .all(|(_, record, _)| secret_of(record) == "new-token"),
            "the old value was escrowed after it was replaced"
        );
        let got = git_credential(&r, "github", "github.com").await.unwrap();
        assert_eq!(got.secret.as_slice(), b"new-token");
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn a_stopped_actor_stops_trying_to_escrow() {
        let r = rig(vec![], vec![], false).await;
        r.failing.store(true, std::sync::atomic::Ordering::SeqCst);
        register(
            &r,
            "github",
            b"ghp-token",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        wait_until("an attempt", || stored_len(&r) >= 1).await;
        stop_and_release_key(&r.actor).await;
        let at_stop = stored_len(&r);
        settle().await;
        assert_eq!(
            stored_len(&r),
            at_stop,
            "it kept trying after it was stopped"
        );
    }

    #[tokio::test]
    async fn loading_registers_what_the_credentials_agent_holds_under_the_prefixed_name_and_does_not_escrow_it_again()
     {
        let binding = SecretBinding {
            host_pattern: "*.example.org".to_string(),
            kind: SecretKind::Token,
            username: None,
        };
        let record = registry::escrow_form(&binding, b"ghp-token").unwrap();
        let r = rig(vec![], vec![("github", record.to_vec())], false).await;
        assert!(load(&r, "github").await.unwrap());
        let got = git_credential(&r, "github", "git.example.org")
            .await
            .unwrap();
        assert_eq!(got.secret.as_slice(), b"ghp-token");
        assert!(git_credential(&r, "github", "github.com").await.is_err());
        settle().await;
        assert_eq!(stored_len(&r), 0);
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn loading_a_name_nothing_holds_is_false_and_registers_nothing() {
        let r = rig(vec![], vec![], false).await;
        assert!(!load(&r, "github").await.unwrap());
        assert!(git_credential(&r, "github", "github.com").await.is_err());
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn loading_a_secret_policy_denies_is_an_error_that_says_so() {
        let binding = token_binding("github.com");
        let record = registry::escrow_form(&binding, b"ghp-token").unwrap();
        let r = rig(vec![], vec![("github", record.to_vec())], true).await;
        let refused = load(&r, "github").await;
        assert!(
            matches!(refused, Err(CryptoError::RetrieveSecretDenied(_))),
            "{refused:?}"
        );
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn loading_something_that_is_not_a_credential_registers_nothing() {
        let r = rig(vec![], vec![("github", b"just some bytes".to_vec())], false).await;
        let refused = load(&r, "github").await;
        assert!(
            matches!(refused, Err(CryptoError::Record(RecordError::Malformed))),
            "{refused:?}"
        );
        assert!(git_credential(&r, "github", "github.com").await.is_err());
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn the_escrow_carries_a_kind_label_equal_to_the_bindings_kind() {
        for (kind, expected) in [
            (SecretKind::Token, "token"),
            (SecretKind::UserPassword, "user-password"),
        ] {
            let r = rig(vec![], vec![], false).await;
            let binding = SecretBinding {
                host_pattern: "github.com".to_string(),
                kind,
                username: Some("alice".to_string()),
            };
            register(&r, "github", b"ghp-token", b"ctx", binding)
                .await
                .unwrap();
            wait_until("the escrow", || stored_len(&r) >= 1).await;
            let (_, _, labels) = r.stored.lock().unwrap()[0].clone();
            assert_eq!(labels.len(), 1, "{labels:?}");
            assert_eq!(labels.get("kind").map(String::as_str), Some(expected));
            stop_and_release_key(&r.actor).await;
        }
    }

    #[tokio::test]
    async fn an_escrow_refused_for_a_policy_or_a_label_reason_is_not_retried() {
        for reason in [
            StoreRefusalReason::PolicyDenied,
            StoreRefusalReason::InvalidLabels,
            StoreRefusalReason::LabelsChanged,
            StoreRefusalReason::InvalidSecretName,
        ] {
            let r = rig(vec![StoreScript::Refused(reason)], vec![], false).await;
            register(
                &r,
                "github",
                b"ghp-token",
                b"ctx",
                token_binding("github.com"),
            )
            .await
            .unwrap();
            wait_until("the attempt", || stored_len(&r) >= 1).await;
            settle().await;
            assert_eq!(stored_len(&r), 1, "{reason:?}");
            stop_and_release_key(&r.actor).await;
        }
    }

    #[tokio::test]
    async fn an_escrow_refused_because_the_certificate_is_not_on_record_yet_is_retried() {
        let r = rig(
            vec![StoreScript::Refused(
                StoreRefusalReason::RequesterCertificateNotFound,
            )],
            vec![],
            false,
        )
        .await;
        register(
            &r,
            "github",
            b"ghp-token",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        wait_until("a second attempt", || stored_len(&r) >= 2).await;
        settle().await;
        assert_eq!(stored_len(&r), 2);
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn replacing_a_section_drops_what_it_no_longer_lists_and_keeps_what_it_does() {
        let r = rig(vec![], vec![], false).await;
        for name in ["kept", "dropped-b", "dropped-a"] {
            register(&r, name, b"tok", b"ctx", token_binding("github.com"))
                .await
                .unwrap();
        }
        let dropped = replace_section(&r, SECTION, &["kept"]).await;
        assert_eq!(
            dropped,
            vec!["dropped-a".to_string(), "dropped-b".to_string()]
        );
        assert!(git_credential(&r, "kept", "github.com").await.is_ok());
        for name in ["dropped-a", "dropped-b"] {
            assert!(
                matches!(
                    git_credential(&r, name, "github.com").await,
                    Err(CryptoError::SecretNotRegistered(_))
                ),
                "{name}"
            );
        }
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn replacing_a_section_leaves_the_secrets_of_other_sections_alone() {
        let r = rig(vec![], vec![], false).await;
        register_in(
            &r,
            "elsewhere",
            "other",
            b"tok",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        register(&r, "mine", b"tok", b"ctx", token_binding("github.com"))
            .await
            .unwrap();
        let dropped = replace_section(&r, SECTION, &[]).await;
        assert_eq!(dropped, vec!["mine".to_string()]);
        assert!(git_credential(&r, "other", "github.com").await.is_ok());
        // a section nothing was registered under drops nothing
        assert!(replace_section(&r, "nothing-here", &[]).await.is_empty());
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn a_name_registered_again_under_another_section_belongs_to_that_one() {
        let r = rig(vec![], vec![], false).await;
        register(&r, "moved", b"tok", b"ctx", token_binding("github.com"))
            .await
            .unwrap();
        register_in(
            &r,
            "elsewhere",
            "moved",
            b"tok",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        assert!(replace_section(&r, SECTION, &[]).await.is_empty());
        assert!(git_credential(&r, "moved", "github.com").await.is_ok());
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn a_withdrawn_secret_is_no_longer_escrowed() {
        // Every attempt fails, so the escrow keeps retrying until the secret is withdrawn.
        let r = rig(vec![], vec![], false).await;
        r.failing.store(true, std::sync::atomic::Ordering::SeqCst);
        register(&r, "github", b"tok", b"ctx", token_binding("github.com"))
            .await
            .unwrap();
        wait_until("a first attempt", || stored_len(&r) >= 1).await;
        replace_section(&r, SECTION, &[]).await;
        settle().await;
        let attempts = stored_len(&r);
        settle().await;
        settle().await;
        assert_eq!(
            stored_len(&r),
            attempts,
            "it kept escrowing a withdrawn secret"
        );
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn no_refusal_repeats_the_secret() {
        let r = rig(vec![], vec![], false).await;
        register(
            &r,
            "github",
            b"ghp-super-secret",
            b"ctx",
            token_binding("github.com"),
        )
        .await
        .unwrap();
        let refused = git_credential(&r, "github", "evil.example.org")
            .await
            .unwrap_err();
        assert!(!format!("{refused} {refused:?}").contains("ghp-super-secret"));
        let wrong_context = ractor::call!(r.actor, |reply| CryptoMessage::RegisterSecret {
            name: "other".to_string(),
            section: SECTION.to_string(),
            secret: escrowed_bytes(&r, b"ghp-super-secret", b"ctx-a"),
            context: b"ctx-b".to_vec(),
            binding: token_binding("github.com"),
            reply,
        })
        .unwrap()
        .unwrap_err();
        assert!(!format!("{wrong_context} {wrong_context:?}").contains("ghp-super-secret"));
        stop_and_release_key(&r.actor).await;
    }

    #[tokio::test]
    async fn encrypt_for_recipient_genuinely_round_trips_a_real_message_to_a_real_recipient_public_key()
     {
        let mut rng = rand::rng();
        let recipient_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let recipient_public_key_pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &recipient_key.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
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
        stop_and_release_key(&crypto_actor).await;
    }

    #[tokio::test]
    async fn encrypt_for_recipient_genuinely_caches_the_recipient_key_after_the_first_query() {
        let mut rng = rand::rng();
        let recipient_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let recipient_public_key_pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &recipient_key.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
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
        stop_and_release_key(&crypto_actor).await;
    }

    /// Every agent's escrow key is P-256 now, held by its custody daemon: sealed to with HPKE.
    #[tokio::test]
    async fn encrypt_for_recipient_seals_to_a_p256_recipient_with_hpke() {
        use p256::pkcs8::EncodePublicKey as _;
        let recipient = loop {
            if let Ok(key) = p256::SecretKey::from_bytes(&rand::random::<[u8; 32]>().into()) {
                break key;
            }
        };
        let pem = recipient
            .public_key()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .unwrap();
        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                recipient_public_key_pem: Some(pem),
                ..Default::default()
            })
            .await;
        let wire = ractor::call!(crypto_actor, |reply| CryptoMessage::EncryptForRecipient {
            recipient_identity: "some-other-agent".to_string(),
            plaintext: b"to a P-256 recipient".to_vec(),
            reply,
        })
        .unwrap()
        .unwrap();
        let escrowed = rkyv::from_bytes::<EscrowedSecret, rkyv::rancor::Error>(&wire).unwrap();
        assert_eq!(
            escrowed.suite(),
            credentials_agent_common::escrow_secret::Suite::HpkeP256
        );
        assert_eq!(
            &*escrowed.decrypt_p256(&recipient).unwrap(),
            b"to a P-256 recipient"
        );
        stop_and_release_key(&crypto_actor).await;
    }

    /// docs/architecture/key-custody.md, "Also in this work": a key handed out in a certificate the pinned CA did not
    /// sign is not encrypted to, however it came.
    #[tokio::test]
    async fn encrypt_for_recipient_refuses_a_key_whose_certificate_cannot_be_believed() {
        let recipient_key = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
        let pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &recipient_key.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let ((crypto_actor, _), _, _) =
            spawn_crypto_actor_with_fake_credentials_agent_configured(FakeCredentialsAgentConfig {
                recipient_public_key_pem: Some(pem),
                forge_recipient_certificate: true,
                ..Default::default()
            })
            .await;
        let result = ractor::call!(crypto_actor, |reply| CryptoMessage::EncryptForRecipient {
            recipient_identity: "some-other-agent".to_string(),
            plaintext: b"never sealed to a forger".to_vec(),
            reply,
        })
        .unwrap();
        assert!(
            matches!(&result, Err(CryptoError::KeyEncoding(why)) if why.contains("pinned CA")),
            "{result:?}"
        );
        stop_and_release_key(&crypto_actor).await;
    }

    #[tokio::test]
    async fn encrypt_for_recipient_fails_with_recipient_key_not_found_when_the_recipient_has_no_usable_key()
     {
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
        stop_and_release_key(&crypto_actor).await;
    }
}
