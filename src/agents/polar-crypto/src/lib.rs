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
//! Today's scope: the general-secret store pass-through only.
//! Retrieving one's own escrow keypair (and everything that depends
//! on it -- the ephemeral X25519 exchange, kernel-keyring custody)
//! is real, deliberately deferred work: the Credentials Agent's own
//! retrieve endpoint doesn't exist yet, so this actor has nothing
//! real to call for that flow today.

use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use credentials_agent_common::escrow_secret::EscrowedSecret;
use credentials_agent_common::secret::{StoreRefusalReason, StoreSecretReply, StoreSecretRequest};
use credentials_agent_common::{
    CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC, CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_STORE_SECRET_REPLIES_TOPIC, CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
};
use polar::request_reply::{Envelope, ReplyRouter};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, async_trait};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, warn};

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
}

pub enum CryptoMessage {
    /// A general secret to escrow -- e.g. an access token an observer
    /// holds and wants the Credentials Agent to store on its behalf.
    /// Not the agent's own escrow private key; that flow depends on
    /// retrieve, not built yet (see this crate's own top-level doc
    /// comment).
    StoreSecret {
        name: String,
        plaintext: Vec<u8>,
        reply: RpcReplyPort<Result<(), CryptoError>>,
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
}

pub struct CryptoActorState {
    tcp_client: ActorRef<TcpClientMessage>,
    escrow_key_reply_router: Arc<ReplyRouter<credentials_agent_common::escrow_key::EscrowKeyReply>>,
    store_secret_reply_router: Arc<ReplyRouter<StoreSecretReply>>,
    /// The Credentials Agent's own current escrow public key, learned
    /// on first use and reused afterward -- avoids re-querying on
    /// every single store, per credentials-agent-architecture.md's
    /// own note on CryptoActor's local caching. Arc<Mutex<_>> rather
    /// than a plain field: the actual store work runs in a spawned
    /// task, not inline in handle() (see handle()'s own StoreSecret
    /// arm for why), so this needs to be mutable from outside the
    /// actor's own sequential mailbox loop.
    cached_credentials_agent_escrow_key: Arc<tokio::sync::Mutex<Option<String>>>,
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

        Ok(CryptoActorState {
            tcp_client: args.tcp_client,
            escrow_key_reply_router: Arc::new(ReplyRouter::new()),
            store_secret_reply_router: Arc::new(ReplyRouter::new()),
            cached_credentials_agent_escrow_key: Arc::new(tokio::sync::Mutex::new(None)),
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
                } else {
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
                    let decrypted = envelope
                        .payload
                        .secret
                        .decrypt(&self.escrow_private_key)
                        .expect("submitted secret must genuinely decrypt with the real key");
                    assert_eq!(&*decrypted, b"a real secret from a real actor");

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
            }

            Ok(())
        }
    }

    async fn spawn_crypto_actor_with_fake_credentials_agent() -> (
        ActorRef<CryptoMessage>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let (private_key, public_key_pem) = test_credentials_agent_keypair();
        let events_output = Arc::new(OutputPort::default());
        let escrow_key_queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let (fake_agent, _) = Actor::spawn(
            None,
            FakeCredentialsAgent {
                events_output: events_output.clone(),
                escrow_private_key: private_key,
                escrow_public_key_pem: public_key_pem,
                escrow_key_queries: escrow_key_queries.clone(),
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
            },
        )
        .await
        .unwrap();

        (crypto_actor, escrow_key_queries)
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
}
