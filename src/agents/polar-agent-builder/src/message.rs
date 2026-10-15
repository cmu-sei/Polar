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

//! The real, type-state message flow -- Outbound<T> -> Encrypted ->
//! .publish() -- so a caller writes
//! `outbound(payload).encrypt_for(recipient).await?.publish(topic)`,
//! and publishing raw plaintext where encryption was intended
//! becomes a genuine compile error, not a code-review concern.
//! `.publish()` simply doesn't exist on Outbound<T> at all -- only on
//! Encrypted and Plaintext, each reached only through an explicit,
//! real, named step.

use crate::crypto::CryptoClient;
use cassini_client::{OfflineBehavior, PublishRequest};
use polar::cassini::{CassiniClient, CassiniClientError, TcpClient};
use polar_crypto::CryptoError;

/// A real, plain payload, not yet meant for the wire -- the real,
/// starting state of every message this flow produces. Genuinely has
/// no `.publish()` method of its own at all.
pub struct Outbound<T> {
    payload: T,
    crypto: CryptoHandleRef,
    tcp_client: TcpClient,
}

/// A real, small, internal alias -- CryptoClient is used as a trait
/// object here (Box<dyn CryptoClient>) specifically so Outbound<T>
/// stays usable against either a real CryptoHandle or a real, fake,
/// in-memory test implementation of the same trait, without a real,
/// separate generic parameter threading through every type in this
/// module.
type CryptoHandleRef = std::sync::Arc<dyn CryptoClient>;

impl<T: Into<Vec<u8>>> Outbound<T> {
    pub fn new(payload: T, crypto: CryptoHandleRef, tcp_client: TcpClient) -> Self {
        Self {
            payload,
            crypto,
            tcp_client,
        }
    }

    /// Consumes this Outbound<T>, encrypting its own, real payload to
    /// `recipient`'s own, real escrow public key -- see
    /// CryptoClient::encrypt_for_recipient's own doc comment. Returns
    /// Encrypted, the one, real, only state `.publish()` exists on.
    pub async fn encrypt_for(self, recipient: impl Into<String>) -> Result<Encrypted, CryptoError> {
        let ciphertext = self
            .crypto
            .encrypt_for_recipient(recipient.into(), self.payload.into())
            .await?;
        Ok(Encrypted {
            ciphertext,
            tcp_client: self.tcp_client,
        })
    }

    /// The real, deliberate, explicitly-named escape hatch --
    /// reachable only on purpose, never by accident, unlike a
    /// `.publish()` method that existed directly on Outbound<T>
    /// itself would be.
    pub fn as_plaintext(self) -> Plaintext {
        Plaintext {
            payload: self.payload.into(),
            tcp_client: self.tcp_client,
        }
    }
}

/// The real, only state with a genuine `.publish()` method reached
/// through encryption -- ciphertext, ready for the wire.
pub struct Encrypted {
    ciphertext: Vec<u8>,
    tcp_client: TcpClient,
}

impl Encrypted {
    pub fn publish(self, topic: impl Into<String>) -> Result<(), CassiniClientError> {
        self.tcp_client.publish(PublishRequest {
            topic: topic.into(),
            payload: self.ciphertext,
            trace_ctx: None,
            offline_behavior: OfflineBehavior::default(),
            share_identity: true,
        })
    }
}

/// The real, only other state with a genuine `.publish()` method --
/// reached only through Outbound::as_plaintext's own, deliberately,
/// explicitly named escape hatch.
pub struct Plaintext {
    payload: Vec<u8>,
    tcp_client: TcpClient,
}

impl Plaintext {
    pub fn publish(self, topic: impl Into<String>) -> Result<(), CassiniClientError> {
        self.tcp_client.publish(PublishRequest {
            topic: topic.into(),
            payload: self.payload,
            trace_ctx: None,
            offline_behavior: OfflineBehavior::default(),
            share_identity: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ractor::{Actor, ActorProcessingErr, ActorRef};
    use std::sync::{Arc, Mutex};

    /// A real, fake, in-memory CryptoClient -- proving the whole,
    /// real point of CryptoClient being a trait, not just an
    /// inherent impl on CryptoHandle: this flow works against it with
    /// no spawned CryptoActor, no real Credentials Agent, and no real
    /// network connection at all.
    struct FakeCrypto;

    #[async_trait]
    impl CryptoClient for FakeCrypto {
        async fn store_secret(
            &self,
            _name: String,
            _plaintext: Vec<u8>,
        ) -> Result<(), CryptoError> {
            Ok(())
        }
        async fn decrypt_own_message(&self, ciphertext: Vec<u8>) -> Result<Vec<u8>, CryptoError> {
            Ok(ciphertext)
        }
        async fn encrypt_for_recipient(
            &self,
            _recipient_identity: String,
            plaintext: Vec<u8>,
        ) -> Result<Vec<u8>, CryptoError> {
            // A real, deliberately simple, fake "encryption" -- just
            // prefixes the real, given plaintext, so a test can
            // genuinely distinguish it from the untouched original.
            let mut result = b"FAKE-ENCRYPTED:".to_vec();
            result.extend(plaintext);
            Ok(result)
        }
        async fn ensure_bootstrapped(&self) -> Result<(), CryptoError> {
            Ok(())
        }
        async fn retrieve_secret(
            &self,
            _name: String,
        ) -> Result<Option<polar_crypto::Zeroizing<Vec<u8>>>, CryptoError> {
            Ok(None)
        }
        async fn register_secret(
            &self,
            _name: String,
            _section: String,
            _secret: Vec<u8>,
            _context: Vec<u8>,
            _binding: polar_crypto::registry::SecretBinding,
        ) -> Result<(), CryptoError> {
            Ok(())
        }
        async fn load_secret(&self, _name: String, _section: String) -> Result<bool, CryptoError> {
            Ok(false)
        }
        async fn replace_section(
            &self,
            _section: String,
            _keep: Vec<String>,
        ) -> Result<Vec<String>, CryptoError> {
            Ok(Vec::new())
        }
        async fn git_credential(
            &self,
            name: String,
            _host: String,
        ) -> Result<polar_crypto::registry::GitCredential, CryptoError> {
            Err(CryptoError::SecretNotRegistered(name))
        }
    }

    struct FakeBroker {
        received: Arc<Mutex<Vec<cassini_client::TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for FakeBroker {
        type Msg = cassini_client::TcpClientMessage;
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
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    #[tokio::test]
    async fn outbound_encrypt_for_publish_genuinely_flows_through_to_a_real_publish_call() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (broker, _) = Actor::spawn(
            None,
            FakeBroker {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let tcp_client = TcpClient::from_actor_ref(broker);
        let crypto: CryptoHandleRef = Arc::new(FakeCrypto);

        let outbound = Outbound::new(b"a real, plain payload".to_vec(), crypto, tcp_client);
        outbound
            .encrypt_for("some-recipient")
            .await
            .unwrap()
            .publish("a-real-topic")
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let messages = received.lock().unwrap();
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            cassini_client::TcpClientMessage::Publish { topic, payload, .. } => {
                assert_eq!(topic, "a-real-topic");
                assert_eq!(payload, b"FAKE-ENCRYPTED:a real, plain payload");
            }
            other => panic!("expected Publish, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn as_plaintext_publishes_the_real_untouched_payload_directly() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (broker, _) = Actor::spawn(
            None,
            FakeBroker {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let tcp_client = TcpClient::from_actor_ref(broker);
        let crypto: CryptoHandleRef = Arc::new(FakeCrypto);

        let outbound = Outbound::new(b"never encrypted".to_vec(), crypto, tcp_client);
        outbound.as_plaintext().publish("a-real-topic").unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let messages = received.lock().unwrap();
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            cassini_client::TcpClientMessage::Publish { payload, .. } => {
                assert_eq!(payload, b"never encrypted");
            }
            other => panic!("expected Publish, got {other:?}"),
        }
    }
}
