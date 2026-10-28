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

//! The credentials agent's authority key certified (docs/architecture/authority-distribution.md, "Pinning the key to
//! the CA"). The escrow key, which signs every authority update (decision 4: one key), is given a `CertType::Signing`
//! certificate for this agent's own identity on cert-issuer's `/internal/issue`, and the publisher carries it in every
//! snapshot, so that a leaf pins the key because the CA it pins says it is the credentials agent's, and not merely
//! because the broker said who published it. Renewed at half its life; tried again every few seconds while cert-issuer
//! cannot answer, the publisher publishing no snapshot until the first.

use crate::authority_publisher::PublisherMessage;
use crate::workload_issuance::{SIGNER_RETRY, renewal_delay};
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::{CertType, InternalIssueRequest};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rsa::RsaPrivateKey;
use std::time::Duration;
use tracing::{info, warn};

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Asks cert-issuer to certify `key` as `identity`'s signing key: its certificate as a chain, and how long until it is
/// to be renewed.
pub async fn certify(
    cert_issuer: &HandshakeClient,
    identity: &str,
    key: &RsaPrivateKey,
) -> Result<(Vec<String>, Duration), String> {
    use rsa::pkcs8::EncodePrivateKey;
    // The key's PEM only for as long as the CSR takes, and wiped when it goes.
    let pem = key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .map_err(|e| format!("cannot encode the signing key: {e}"))?;
    let csr = cert_issuer_common::keypair::csr_for_private_key_pem(identity, &[], &pem)
        .map_err(|e| format!("cannot make the signing key's CSR: {e}"))?;
    drop(pem);
    let issued = cert_issuer
        .issue_internal(InternalIssueRequest {
            instance_binding: None,
            expires_by: None,
            csr_pem: csr,
            identity: identity.to_string(),
            cert_type: CertType::Signing,
            extra_sans: vec![],
            subject_dn_defaults: None,
            account_subject: None,
        })
        .await
        .map_err(|e| format!("cert-issuer did not certify the signing key: {e}"))?;
    let now = unix_now();
    let delay = renewal_delay(now, issued.expires_at.unix_timestamp(), now);
    // The certificate alone: the CA that signed it is the one every pod pins.
    Ok((vec![issued.certificate_pem], delay))
}

pub struct AuthorityCertifier;

pub struct AuthorityCertifierArgs {
    pub cert_issuer: HandshakeClient,
    /// This agent's identity as cert-issuer reads it off its connection: what the certificate names.
    pub identity: String,
    pub key: RsaPrivateKey,
    pub publisher: ActorRef<PublisherMessage>,
}

pub enum AuthorityCertifierMessage {
    Renew,
}

#[ractor::async_trait]
impl Actor for AuthorityCertifier {
    type Msg = AuthorityCertifierMessage;
    type State = AuthorityCertifierArgs;
    type Arguments = AuthorityCertifierArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: AuthorityCertifierArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        myself.cast(AuthorityCertifierMessage::Renew)?;
        Ok(args)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        AuthorityCertifierMessage::Renew: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let next = match certify(&state.cert_issuer, &state.identity, &state.key).await {
            Ok((chain, delay)) => {
                info!(renewed_in = ?delay, "the signing key is certified");
                state.publisher.cast(PublisherMessage::Certified(chain))?;
                delay
            }
            Err(e) => {
                warn!("{e}; trying again in {SIGNER_RETRY:?}");
                SIGNER_RETRY
            }
        };
        drop(myself.send_after(next, || AuthorityCertifierMessage::Renew));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload_issuance::tests::{CertifiesSigning, OWN_IDENTITY, the_ca};
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn key() -> RsaPrivateKey {
        crate::escrow::tests::test_keypair().private_key().clone()
    }

    #[tokio::test]
    async fn the_escrow_key_is_certified_as_this_agent_s_signing_key_and_a_leaf_believes_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .and(body_partial_json(serde_json::json!({
                "identity": OWN_IDENTITY,
                "cert_type": "SIGNING",
                "extra_sans": [],
            })))
            .respond_with(CertifiesSigning)
            .expect(1)
            .mount(&server)
            .await;
        let (chain, delay) = certify(&HandshakeClient::new(server.uri()), OWN_IDENTITY, &key())
            .await
            .unwrap();
        // An hour's certificate, renewed at half of it.
        assert!(
            (Duration::from_secs(1800 - 5)..=Duration::from_secs(1800 + 5)).contains(&delay),
            "{delay:?}"
        );
        // The publisher takes it, and a leaf that pins the CA pins the escrow key under it.
        let mut book = credentials_agent_common::authority_book::AuthorityBook::new(key(), 0);
        book.certify(chain).unwrap();
        let trust = polar_signing::Trust::from_pem(&[the_ca().pem.clone()]).unwrap();
        assert_eq!(
            book.snapshot()
                .unwrap()
                .certified_key(&trust, OWN_IDENTITY, unix_now()),
            Ok(key().to_public_key())
        );
    }

    /// Notes each `Certified` the publisher is sent.
    struct Publisher(Arc<Mutex<Vec<Vec<String>>>>);

    #[ractor::async_trait]
    impl Actor for Publisher {
        type Msg = PublisherMessage;
        type State = ();
        type Arguments = ();
        async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            if let PublisherMessage::Certified(chain) = message {
                self.0.lock().unwrap().push(chain);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn while_cert_issuer_cannot_answer_it_tries_again_and_hands_the_publisher_the_first_it_gets()
     {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(CertifiesSigning)
            .mount(&server)
            .await;
        let certified = Arc::new(Mutex::new(Vec::new()));
        let (publisher, _) = Actor::spawn(None, Publisher(certified.clone()), ())
            .await
            .unwrap();
        let (certifier, _) = Actor::spawn(
            None,
            AuthorityCertifier,
            AuthorityCertifierArgs {
                cert_issuer: HandshakeClient::new(server.uri()),
                identity: OWN_IDENTITY.to_string(),
                key: key(),
                publisher,
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            certified.lock().unwrap().is_empty(),
            "nothing yet: cert-issuer was down"
        );
        tokio::time::sleep(SIGNER_RETRY + Duration::from_secs(1)).await;
        assert_eq!(certified.lock().unwrap().len(), 1, "the second try's");
        certifier.stop(None);
    }
}
