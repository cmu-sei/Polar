//! CryptoHandle -- wraps a real, live polar_crypto::CryptoActor's
//! own ActorRef<CryptoMessage>, exposing its four, real messages as
//! direct, ordinary async methods instead of requiring every, real
//! caller to construct a CryptoMessage and call ractor::call! by
//! hand. The real, sole reason this crate exists: hiding every
//! ractor-specific primitive (ActorRef, RpcReplyPort, ractor::call!)
//! behind a real, ordinary Rust API surface.
//!
//! A real trait (CryptoClient), not just an inherent impl -- so a
//! test can implement the same trait with a fake, in-memory version,
//! with no spawned actor, no real Credentials Agent, and no real
//! network connection needed at all.

use async_trait::async_trait;
use polar_crypto::registry::{GitCredential, SecretBinding};
use polar_crypto::{CryptoError, CryptoMessage};
use ractor::ActorRef;

#[async_trait]
pub trait CryptoClient: Send + Sync {
    /// See CryptoMessage::StoreSecret's own doc comment.
    async fn store_secret(&self, name: String, plaintext: Vec<u8>) -> Result<(), CryptoError>;

    /// See CryptoMessage::DecryptOwnMessage's own doc comment.
    async fn decrypt_own_message(&self, ciphertext: Vec<u8>) -> Result<Vec<u8>, CryptoError>;

    /// See CryptoMessage::EncryptForRecipient's own doc comment.
    async fn encrypt_for_recipient(
        &self,
        recipient_identity: String,
        plaintext: Vec<u8>,
    ) -> Result<Vec<u8>, CryptoError>;

    /// See CryptoMessage::EnsureBootstrapped's own doc comment.
    async fn ensure_bootstrapped(&self) -> Result<(), CryptoError>;

    /// See CryptoMessage::RetrieveSecret's own doc comment. `Ok(None)` is a name nothing is stored
    /// under; a secret this agent may not have is an error that says policy refused it.
    async fn retrieve_secret(
        &self,
        name: String,
    ) -> Result<Option<polar_crypto::Zeroizing<Vec<u8>>>, CryptoError>;

    /// See CryptoMessage::RegisterSecret's own doc comment.
    async fn register_secret(
        &self,
        name: String,
        secret: Vec<u8>,
        context: Vec<u8>,
        binding: SecretBinding,
    ) -> Result<(), CryptoError>;

    /// See CryptoMessage::LoadSecret's own doc comment.
    async fn load_secret(&self, name: String) -> Result<bool, CryptoError>;

    /// See CryptoMessage::GitCredential's own doc comment.
    async fn git_credential(
        &self,
        name: String,
        host: String,
    ) -> Result<GitCredential, CryptoError>;
}

/// The real, live implementation of CryptoClient -- a thin wrapper
/// around a real, already-spawned CryptoActor's own ActorRef.
#[derive(Clone)]
pub struct CryptoHandle {
    actor: ActorRef<CryptoMessage>,
}

impl CryptoHandle {
    pub fn new(actor: ActorRef<CryptoMessage>) -> Self {
        Self { actor }
    }

    /// The real, deliberate escape hatch -- for the rare, genuine
    /// case a caller needs the raw ActorRef directly (e.g. to spawn
    /// something else linked against it), rather than going through
    /// this handle's own, ordinary methods. Mirrors
    /// lib::cassini::TcpClient::as_actor_ref's own, already-established
    /// convention exactly.
    pub fn as_actor_ref(&self) -> ActorRef<CryptoMessage> {
        self.actor.clone()
    }
}

/// Flattens ractor::call!'s own, real, outer RactorErr (a genuine
/// messaging/mailbox failure, distinct from CryptoError itself, which
/// only ever describes the real, remote Credentials Agent flow's own
/// failure) into CryptoError::BrokerUnreachable -- the same, real,
/// existing variant CryptoActor's own, internal request/reply
/// helpers already use for an analogous, real failure.
fn flatten_call_result<T>(
    result: Result<Result<T, CryptoError>, ractor::RactorErr<CryptoMessage>>,
) -> Result<T, CryptoError> {
    match result {
        Ok(inner) => inner,
        Err(e) => Err(CryptoError::BrokerUnreachable(e.to_string())),
    }
}

#[async_trait]
impl CryptoClient for CryptoHandle {
    async fn store_secret(&self, name: String, plaintext: Vec<u8>) -> Result<(), CryptoError> {
        let result = ractor::call!(self.actor, |reply| CryptoMessage::StoreSecret {
            name,
            plaintext,
            reply,
        });
        flatten_call_result(result)
    }

    async fn decrypt_own_message(&self, ciphertext: Vec<u8>) -> Result<Vec<u8>, CryptoError> {
        let result = ractor::call!(self.actor, |reply| CryptoMessage::DecryptOwnMessage {
            ciphertext,
            reply,
        });
        flatten_call_result(result)
    }

    async fn encrypt_for_recipient(
        &self,
        recipient_identity: String,
        plaintext: Vec<u8>,
    ) -> Result<Vec<u8>, CryptoError> {
        let result = ractor::call!(self.actor, |reply| CryptoMessage::EncryptForRecipient {
            recipient_identity,
            plaintext,
            reply,
        });
        flatten_call_result(result)
    }

    async fn retrieve_secret(
        &self,
        name: String,
    ) -> Result<Option<polar_crypto::Zeroizing<Vec<u8>>>, CryptoError> {
        let result = ractor::call!(self.actor, |reply| CryptoMessage::RetrieveSecret {
            name,
            reply,
        });
        flatten_call_result(result)
    }

    async fn register_secret(
        &self,
        name: String,
        secret: Vec<u8>,
        context: Vec<u8>,
        binding: SecretBinding,
    ) -> Result<(), CryptoError> {
        let result = ractor::call!(self.actor, |reply| CryptoMessage::RegisterSecret {
            name,
            secret,
            context,
            binding,
            reply,
        });
        flatten_call_result(result)
    }

    async fn load_secret(&self, name: String) -> Result<bool, CryptoError> {
        let result = ractor::call!(self.actor, |reply| CryptoMessage::LoadSecret {
            name,
            reply
        });
        flatten_call_result(result)
    }

    async fn git_credential(
        &self,
        name: String,
        host: String,
    ) -> Result<GitCredential, CryptoError> {
        let result = ractor::call!(self.actor, |reply| CryptoMessage::GitCredential {
            name,
            host,
            reply,
        });
        flatten_call_result(result)
    }

    async fn ensure_bootstrapped(&self) -> Result<(), CryptoError> {
        // How the key ended up held matters to the init container, which reports it to the
        // workload; here the bootstrap is only needed to have happened.
        let result = ractor::call!(self.actor, |reply| CryptoMessage::EnsureBootstrapped {
            reply,
        })
        .map(|outcome| outcome.map(|_custody| ()));
        flatten_call_result(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for the CryptoActor: answers RetrieveSecret as it would, so what is checked is that
    /// the handle asks for the right name and passes the answer back.
    struct StubCrypto;

    #[ractor::async_trait]
    impl ractor::Actor for StubCrypto {
        type Msg = CryptoMessage;
        type State = ();
        type Arguments = ();
        async fn pre_start(
            &self,
            _: ActorRef<CryptoMessage>,
            _: (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<CryptoMessage>,
            message: CryptoMessage,
            _: &mut (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            if let CryptoMessage::RetrieveSecret { name, reply } = message {
                let _ = reply.send(match name.as_str() {
                    "known" => Ok(Some(polar_crypto::Zeroizing::new(b"value".to_vec()))),
                    "forbidden" => Err(CryptoError::RetrieveSecretDenied("no".into())),
                    _ => Ok(None),
                });
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_handle_retrieves_a_secret_by_name() {
        let (actor, _) = ractor::Actor::spawn(None, StubCrypto, ()).await.unwrap();
        let handle = CryptoHandle::new(actor);
        let known = handle.retrieve_secret("known".into()).await.unwrap();
        assert_eq!(known.as_deref().map(|v| v.as_slice()), Some(&b"value"[..]));
        assert!(
            handle
                .retrieve_secret("unknown".into())
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            handle.retrieve_secret("forbidden".into()).await,
            Err(CryptoError::RetrieveSecretDenied(_))
        ));
    }

    use credentials_agent_common::agent_escrow_key::{AgentEscrowKeyReply, AgentEscrowKeyRequest};
    use polar::request_reply::Envelope;
    use ractor::Actor;
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};

    /// A real, minimal, fake Credentials Agent -- answers
    /// AgentEscrowKeyRequest directly, real enough to prove
    /// CryptoHandle::encrypt_for_recipient genuinely, correctly
    /// delegates through to a real, live CryptoActor, without
    /// needing the full, real Credentials Agent or a real network
    /// connection at all. Mirrors polar-crypto's own, established
    /// FakeCredentialsAgent test pattern, narrowed to just the one,
    /// real flow this test needs.
    struct FakeCredentialsAgent {
        router: ActorRef<polar_router::RouterMessage>,
        recipient_public_key_pem: String,
    }

    #[async_trait]
    impl Actor for FakeCredentialsAgent {
        type Msg = cassini_client::TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ractor::ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ractor::ActorProcessingErr> {
            let cassini_client::TcpClientMessage::Publish { topic, payload, .. } = message else {
                return Ok(());
            };
            if topic == credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC {
                if let Ok(envelope) = rkyv::from_bytes::<
                    Envelope<AgentEscrowKeyRequest>,
                    rkyv::rancor::Error,
                >(&payload)
                {
                    let reply = Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: AgentEscrowKeyReply::Found {
                            public_key_pem: self.recipient_public_key_pem.clone(),
                        },
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                        .unwrap()
                        .to_vec();
                    let _ = self.router.send_message(polar_router::RouterMessage::Route(
                        polar_router::IncomingMessage {
                            topic:
                                credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC
                                    .to_string(),
                            payload: bytes,
                            published_by: None,
                            trace_ctx: None,
                        },
                    ));
                }
            }
            Ok(())
        }
    }

    fn write_test_agent_certificate() -> tempfile::NamedTempFile {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let private_key_pem = private_key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params = rcgen::CertificateParams::new(vec!["test-agent".to_string()]).unwrap();
        let certificate_pem = params.self_signed(&key_pair).unwrap().pem();

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), certificate_pem).unwrap();
        file
    }

    #[tokio::test]
    async fn encrypt_for_recipient_genuinely_delegates_through_to_a_real_crypto_actor() {
        let mut rng = rand::rng();
        let recipient_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let recipient_public_key_pem = recipient_key
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();

        let (router, _) = Actor::spawn(None, polar_router::RouterActor, ())
            .await
            .unwrap();

        let (fake_agent, _) = Actor::spawn(
            None,
            FakeCredentialsAgent {
                router: router.clone(),
                recipient_public_key_pem,
            },
            (),
        )
        .await
        .unwrap();

        let own_cert_file = write_test_agent_certificate();

        let (crypto_actor, _) = Actor::spawn(
            None,
            polar_crypto::CryptoActor,
            polar_crypto::CryptoActorArgs::new(fake_agent, router).with_settings(
                polar_crypto::environment::CryptoSettings {
                    own_certificate_path: own_cert_file.path().to_string_lossy().into_owned(),
                    service_account_token_path: "/unused".to_string(),
                    escrow_key_handle_path: "/nonexistent/escrow-key-handle".to_string(),
                },
            ),
        )
        .await
        .unwrap();

        drop(own_cert_file);

        let handle = CryptoHandle::new(crypto_actor);

        let plaintext = b"a real secret, via a real CryptoHandle";
        let wire_bytes = handle
            .encrypt_for_recipient("some-other-agent".to_string(), plaintext.to_vec())
            .await
            .expect("encrypt_for_recipient must genuinely succeed");

        let escrowed: credentials_agent_common::escrow_secret::EscrowedSecret =
            rkyv::from_bytes::<_, rkyv::rancor::Error>(&wire_bytes).unwrap();
        let decrypted = escrowed.decrypt(&recipient_key).unwrap();
        assert_eq!(&*decrypted, plaintext);
    }

    #[tokio::test]
    async fn a_recipient_with_no_usable_key_genuinely_fails_through_the_real_handle() {
        // Mirrors polar-crypto's own, established
        // encrypt_for_recipient_fails_with_recipient_key_not_found_when_the_recipient_has_no_usable_key
        // test exactly -- the real, fast-failing NotFound case, not
        // a real, slow timeout (CryptoActor's own REPLY_TIMEOUT is a
        // genuine 30 real seconds -- far too slow for a real, fast
        // unit test to wait out).
        struct FakeCredentialsAgentAnsweringNotFound {
            router: ActorRef<polar_router::RouterMessage>,
        }
        #[async_trait]
        impl Actor for FakeCredentialsAgentAnsweringNotFound {
            type Msg = cassini_client::TcpClientMessage;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _myself: ActorRef<Self::Msg>,
                _args: (),
            ) -> Result<Self::State, ractor::ActorProcessingErr> {
                Ok(())
            }
            async fn handle(
                &self,
                _myself: ActorRef<Self::Msg>,
                message: Self::Msg,
                _state: &mut Self::State,
            ) -> Result<(), ractor::ActorProcessingErr> {
                let cassini_client::TcpClientMessage::Publish { topic, payload, .. } = message
                else {
                    return Ok(());
                };
                if topic == credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC {
                    if let Ok(envelope) = rkyv::from_bytes::<
                        Envelope<AgentEscrowKeyRequest>,
                        rkyv::rancor::Error,
                    >(&payload)
                    {
                        let reply = Envelope {
                            correlation_id: envelope.correlation_id,
                            payload: AgentEscrowKeyReply::NotFound,
                        };
                        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                            .unwrap()
                            .to_vec();
                        let _ = self.router.send_message(polar_router::RouterMessage::Route(
                            polar_router::IncomingMessage {
                                topic: credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC.to_string(),
                                payload: bytes,
                                published_by: None,
                                trace_ctx: None,
                            },
                        ));
                    }
                }
                Ok(())
            }
        }

        let (router, _) = Actor::spawn(None, polar_router::RouterActor, ())
            .await
            .unwrap();

        let (fake_agent, _) = Actor::spawn(
            None,
            FakeCredentialsAgentAnsweringNotFound {
                router: router.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let own_cert_file = write_test_agent_certificate();

        let (crypto_actor, _) = Actor::spawn(
            None,
            polar_crypto::CryptoActor,
            polar_crypto::CryptoActorArgs::new(fake_agent, router).with_settings(
                polar_crypto::environment::CryptoSettings {
                    own_certificate_path: own_cert_file.path().to_string_lossy().into_owned(),
                    service_account_token_path: "/unused".to_string(),
                    escrow_key_handle_path: "/nonexistent/escrow-key-handle".to_string(),
                },
            ),
        )
        .await
        .unwrap();

        drop(own_cert_file);

        let handle = CryptoHandle::new(crypto_actor);

        let result = handle
            .encrypt_for_recipient("never-issued-agent".to_string(), b"irrelevant".to_vec())
            .await;

        match result {
            Err(CryptoError::RecipientKeyNotFound(identity)) => {
                assert_eq!(identity, "never-issued-agent");
            }
            other => panic!("expected Err(RecipientKeyNotFound(_)), got {other:?}"),
        }
    }
}
