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

//! The credentials agent's signature on a session offer (docs/architecture/core-restarts.md, "The plan", step 1c).
//!
//! A workload seals its service-account token to the public key in a [`SessionOffer`] that reaches it over plain
//! HTTP through the observer. Unsigned, whoever can change that exchange could offer a key of their own and open
//! the token. So the credentials agent signs each offer with a key it keeps for nothing else, certified by
//! cert-issuer as a signing certificate (`CertType::Signing`) for the credentials agent's own identity, and the
//! workload checks, before it seals, that the signature holds under the CA it was given, that the certificate is a
//! signing certificate and no TLS one, and that it names the credentials agent.
//!
//! What is signed is the session id, the public key and the lifetime, under a domain line of its own
//! ([`OFFER_DOMAIN`]), so the signature cannot be passed off as a configuration document's, nor the reverse.

use crate::workload_issuance::SessionOffer;
use polar_signing::{Algorithm, Document, Domain, SignatureBlock, Trust, VerifyError};
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

/// What an offer's signature is for.
pub const OFFER_DOMAIN: Domain = Domain::new(b"polar-session-offer-v1\n");

/// The signature an offer carries: the algorithm, the signature, and the signer's certificate, then whatever links it
/// to the CA. The empty one signs nothing, and is refused.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    Archive,
    SerdeDeserialize,
    SerdeSerialize,
)]
pub struct OfferSignature {
    pub algorithm: String,
    pub value: Vec<u8>,
    pub certificate_chain: Vec<String>,
}

/// What is signed: the three fields that have an effect, in `polar-signing`'s canonical encoding.
fn payload(session_id: &str, public_key: &[u8; 32], valid_for_secs: u64) -> Value {
    json!({
        "session_id": session_id,
        "public_key": public_key.as_slice(),
        "valid_for_secs": valid_for_secs,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfferSigningError {
    #[error("the signing certificate does not parse: {0}")]
    BadCertificate(String),
    #[error("cannot sign the offer: {0}")]
    Sign(#[from] polar_signing::SignError),
}

/// The credentials agent's signing key and its certificate, held in memory.
pub struct OfferSigner {
    certificate_chain: Vec<String>,
    key_pem: Zeroizing<String>,
    not_before: i64,
    not_after: i64,
}

impl std::fmt::Debug for OfferSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferSigner")
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("key_pem", &"<withheld>")
            .finish()
    }
}

impl OfferSigner {
    /// `certificate_chain` is the signing certificate first, then whatever links it to the CA; `key_pem` is its key,
    /// which is wiped from memory when the signer is dropped. Refused if the key is not the certificate's.
    pub fn new(certificate_chain: Vec<String>, key_pem: String) -> Result<Self, OfferSigningError> {
        let key_pem = Zeroizing::new(key_pem);
        let leaf = certificate_chain
            .first()
            .ok_or_else(|| OfferSigningError::BadCertificate("no certificate".to_string()))?;
        let (not_before, not_after) =
            validity_of(leaf).map_err(OfferSigningError::BadCertificate)?;
        let signer = Self {
            certificate_chain,
            key_pem,
            not_before,
            not_after,
        };
        // A key that is not the certificate's is found now, not at the first offer.
        signer.sign(String::new(), [0; 32], 0)?;
        Ok(signer)
    }

    /// When the certificate stops being valid, in Unix seconds.
    pub fn not_after(&self) -> i64 {
        self.not_after
    }

    /// When it started, in Unix seconds.
    pub fn not_before(&self) -> i64 {
        self.not_before
    }

    /// Whether the certificate is valid at `now`, in Unix seconds: an offer signed outside it would be refused.
    pub fn valid_at(&self, now: i64) -> bool {
        self.not_before <= now && now <= self.not_after
    }

    pub fn sign(
        &self,
        session_id: String,
        public_key: [u8; 32],
        valid_for_secs: u64,
    ) -> Result<SessionOffer, OfferSigningError> {
        let document = polar_signing::sign_in(
            OFFER_DOMAIN,
            payload(&session_id, &public_key, valid_for_secs),
            &self.certificate_chain,
            &self.key_pem,
        )?;
        Ok(SessionOffer {
            session_id,
            public_key,
            valid_for_secs,
            signature: OfferSignature {
                algorithm: document.signature.algorithm.as_str().to_string(),
                value: document.signature.value,
                certificate_chain: document.signature.certificate_chain,
            },
        })
    }
}

fn validity_of(pem: &str) -> Result<(i64, i64), String> {
    let (_, block) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).map_err(|e| e.to_string())?;
    let (_, cert) =
        x509_parser::parse_x509_certificate(&block.contents).map_err(|e| e.to_string())?;
    Ok((
        cert.validity().not_before.timestamp(),
        cert.validity().not_after.timestamp(),
    ))
}

/// Why an offer is not to be sealed to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfferRefusal {
    #[error("the offer's signature is not one: {0}")]
    Malformed(String),
    #[error("the offer's signature does not hold: {0}")]
    Signature(#[from] VerifyError),
    #[error("the offer was not signed with a signing certificate: {0}")]
    NotASigningCertificate(String),
    #[error("the offer was signed by {signer:?}, not the credentials agent `{expected}`")]
    WrongSigner {
        signer: Option<String>,
        expected: String,
    },
}

/// What a workload checks an offer against: the CA it was given, and the credentials agent's identity.
pub struct OfferCheck {
    trust: Trust,
    credentials_agent: String,
}

impl OfferCheck {
    pub fn new(pinned_ca_pem: &str, credentials_agent: String) -> Result<Self, VerifyError> {
        Ok(Self {
            trust: Trust::from_pem(&[pinned_ca_pem.to_string()])?,
            credentials_agent,
        })
    }

    /// Whether `offer` was signed by the credentials agent's signing certificate, valid at `now` (Unix seconds),
    /// under the pinned CA. Only then is a token sealed to it.
    pub fn check(&self, offer: &SessionOffer, now: i64) -> Result<(), OfferRefusal> {
        let signature = &offer.signature;
        let algorithm = Algorithm::parse(&signature.algorithm).ok_or_else(|| {
            OfferRefusal::Malformed(format!("unknown algorithm `{}`", signature.algorithm))
        })?;
        let leaf = signature
            .certificate_chain
            .first()
            .ok_or_else(|| OfferRefusal::Malformed("no certificate".to_string()))?;
        let document = Document {
            payload: payload(&offer.session_id, &offer.public_key, offer.valid_for_secs),
            signature: SignatureBlock {
                algorithm,
                value: signature.value.clone(),
                certificate_chain: signature.certificate_chain.clone(),
            },
        };
        let signer = polar_signing::verify_in(OFFER_DOMAIN, &document, &self.trust, now)?;
        signing_only(leaf).map_err(OfferRefusal::NotASigningCertificate)?;
        if signer.identity.as_deref() != Some(self.credentials_agent.as_str()) {
            return Err(OfferRefusal::WrongSigner {
                signer: signer.identity,
                expected: self.credentials_agent.clone(),
            });
        }
        Ok(())
    }
}

/// Whether `pem` is a signing certificate as cert-issuer makes one: an extended key usage that names document
/// signing and no TLS role, nor any purpose at all. One with none is taken for every use, TLS included.
fn signing_only(pem: &str) -> Result<(), String> {
    let (_, block) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).map_err(|e| e.to_string())?;
    let (_, cert) =
        x509_parser::parse_x509_certificate(&block.contents).map_err(|e| e.to_string())?;
    let eku = cert
        .extended_key_usage()
        .map_err(|e| e.to_string())?
        .ok_or("it has no extended key usage, so it is taken for any use")?
        .value;
    if eku.any || eku.client_auth || eku.server_auth {
        return Err("it may be used for TLS".to_string());
    }
    let document_signing =
        x509_parser::der_parser::oid::Oid::from(cert_issuer_common::OID_DOCUMENT_SIGNING)
            .map_err(|_| "the document-signing OID".to_string())?;
    if !eku.other.contains(&document_signing) {
        return Err("it is not for signing documents".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_signing::testing::{Kind, Spec, now, root, signer};
    use rcgen::ExtendedKeyUsagePurpose;

    const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    fn document_signing() -> ExtendedKeyUsagePurpose {
        ExtendedKeyUsagePurpose::Other(cert_issuer_common::OID_DOCUMENT_SIGNING.to_vec())
    }

    /// A certificate for `identity` with these extended key usages, as cert-issuer would make a signing one.
    fn spec(kind: Kind, identity: &'static str, ekus: Vec<ExtendedKeyUsagePurpose>) -> Spec {
        Spec {
            kind,
            identity,
            capabilities: vec![],
            extended_key_usages: ekus,
            ..Spec::default()
        }
    }

    fn signer_for(ca: &polar_signing::testing::Authority, spec: Spec) -> OfferSigner {
        let s = signer(ca, spec);
        OfferSigner::new(vec![s.cert_pem], s.key_pem).unwrap()
    }

    fn check(ca: &polar_signing::testing::Authority) -> OfferCheck {
        OfferCheck::new(&ca.pem, CREDENTIALS_AGENT.to_string()).unwrap()
    }

    fn an_offer(signer: &OfferSigner) -> SessionOffer {
        signer.sign("session-1".to_string(), [7; 32], 60).unwrap()
    }

    #[test]
    fn an_offer_the_credentials_agent_signed_is_believed_for_either_key_type() {
        let ca = root("Polar Internal CA");
        for kind in [Kind::Ed25519, Kind::P256] {
            let signer = signer_for(&ca, spec(kind, CREDENTIALS_AGENT, vec![document_signing()]));
            assert_eq!(check(&ca).check(&an_offer(&signer), now()), Ok(()));
        }
    }

    #[test]
    fn an_offer_changed_after_it_was_signed_is_refused() {
        let ca = root("Polar Internal CA");
        let signer = signer_for(
            &ca,
            spec(Kind::Ed25519, CREDENTIALS_AGENT, vec![document_signing()]),
        );
        let changes: [fn(&mut SessionOffer); 3] = [
            |o| o.session_id.push('x'),
            |o| o.public_key[0] ^= 1,
            |o| o.valid_for_secs += 1,
        ];
        for change in changes {
            let mut offer = an_offer(&signer);
            change(&mut offer);
            assert_eq!(
                check(&ca).check(&offer, now()),
                Err(OfferRefusal::Signature(VerifyError::BadSignature))
            );
        }
    }

    #[test]
    fn an_offer_signed_under_another_ca_is_refused() {
        let ca = root("Polar Internal CA");
        let other = root("Polar Internal CA");
        let signer = signer_for(
            &other,
            spec(Kind::Ed25519, CREDENTIALS_AGENT, vec![document_signing()]),
        );
        assert_eq!(
            check(&ca).check(&an_offer(&signer), now()),
            Err(OfferRefusal::Signature(VerifyError::UntrustedRoot))
        );
    }

    #[test]
    fn an_offer_signed_with_a_tls_certificate_or_one_for_any_use_is_refused() {
        let ca = root("Polar Internal CA");
        for ekus in [
            vec![ExtendedKeyUsagePurpose::ClientAuth],
            vec![
                ExtendedKeyUsagePurpose::ServerAuth,
                ExtendedKeyUsagePurpose::ClientAuth,
            ],
            vec![document_signing(), ExtendedKeyUsagePurpose::ClientAuth],
            vec![document_signing(), ExtendedKeyUsagePurpose::ServerAuth],
            vec![document_signing(), ExtendedKeyUsagePurpose::Any],
            vec![ExtendedKeyUsagePurpose::CodeSigning],
            vec![],
        ] {
            let signer = signer_for(&ca, spec(Kind::Ed25519, CREDENTIALS_AGENT, ekus.clone()));
            assert!(
                matches!(
                    check(&ca).check(&an_offer(&signer), now()),
                    Err(OfferRefusal::NotASigningCertificate(_))
                ),
                "{ekus:?}"
            );
        }
    }

    #[test]
    fn an_offer_signed_by_anyone_but_the_credentials_agent_is_refused() {
        let ca = root("Polar Internal CA");
        let signer = signer_for(
            &ca,
            spec(
                Kind::Ed25519,
                "cassini.polar.serviceaccount.cluster.local",
                vec![document_signing()],
            ),
        );
        assert_eq!(
            check(&ca).check(&an_offer(&signer), now()),
            Err(OfferRefusal::WrongSigner {
                signer: Some("cassini.polar.serviceaccount.cluster.local".to_string()),
                expected: CREDENTIALS_AGENT.to_string(),
            })
        );
    }

    #[test]
    fn a_configuration_signature_over_the_same_fields_is_not_an_offer_signature() {
        let ca = root("Polar Internal CA");
        let s = signer(
            &ca,
            spec(Kind::Ed25519, CREDENTIALS_AGENT, vec![document_signing()]),
        );
        let mut offer =
            an_offer(&OfferSigner::new(vec![s.cert_pem.clone()], s.key_pem.clone()).unwrap());
        let config = polar_signing::sign(
            payload(&offer.session_id, &offer.public_key, offer.valid_for_secs),
            &[s.cert_pem],
            &s.key_pem,
        )
        .unwrap();
        offer.signature.value = config.signature.value;
        assert_eq!(
            check(&ca).check(&offer, now()),
            Err(OfferRefusal::Signature(VerifyError::BadSignature))
        );
    }

    #[test]
    fn an_offer_with_no_signature_or_an_unknown_algorithm_is_refused_and_does_not_panic() {
        let ca = root("Polar Internal CA");
        let signer = signer_for(
            &ca,
            spec(Kind::Ed25519, CREDENTIALS_AGENT, vec![document_signing()]),
        );
        let mut unsigned = an_offer(&signer);
        unsigned.signature = OfferSignature::default();
        assert!(matches!(
            check(&ca).check(&unsigned, now()),
            Err(OfferRefusal::Malformed(_))
        ));
        let mut no_chain = an_offer(&signer);
        no_chain.signature.certificate_chain.clear();
        assert!(matches!(
            check(&ca).check(&no_chain, now()),
            Err(OfferRefusal::Malformed(_))
        ));
        let mut unknown = an_offer(&signer);
        unknown.signature.algorithm = "rsa-pkcs1".to_string();
        assert!(matches!(
            check(&ca).check(&unknown, now()),
            Err(OfferRefusal::Malformed(_))
        ));
    }

    #[test]
    fn an_offer_signed_with_a_certificate_not_valid_now_is_refused() {
        let ca = root("Polar Internal CA");
        let expired = signer_for(
            &ca,
            Spec {
                not_before_year: 2020,
                not_after_year: 2021,
                ..spec(Kind::Ed25519, CREDENTIALS_AGENT, vec![document_signing()])
            },
        );
        assert!(!expired.valid_at(now()));
        assert!(matches!(
            check(&ca).check(&an_offer(&expired), now()),
            Err(OfferRefusal::Signature(VerifyError::NotValidNow(_)))
        ));
    }

    #[test]
    fn a_signer_knows_its_certificate_s_validity_and_refuses_a_key_that_is_not_its_own() {
        let ca = root("Polar Internal CA");
        let s = signer(
            &ca,
            spec(Kind::Ed25519, CREDENTIALS_AGENT, vec![document_signing()]),
        );
        let signer = OfferSigner::new(vec![s.cert_pem.clone()], s.key_pem).unwrap();
        // The test signer's certificate runs from 2026 to 2030.
        assert_eq!(signer.not_before(), 1_767_225_600);
        assert_eq!(signer.not_after(), 1_893_456_000);
        assert!(signer.valid_at(signer.not_before()) && signer.valid_at(signer.not_after()));
        assert!(
            !signer.valid_at(signer.not_before() - 1) && !signer.valid_at(signer.not_after() + 1)
        );

        let other = signer_for_key(&ca);
        assert!(matches!(
            OfferSigner::new(vec![s.cert_pem], other),
            Err(OfferSigningError::Sign(
                polar_signing::SignError::KeyCertificateMismatch
            ))
        ));
        assert!(OfferSigner::new(vec![], String::new()).is_err());
    }

    fn signer_for_key(ca: &polar_signing::testing::Authority) -> String {
        signer(
            ca,
            spec(Kind::Ed25519, CREDENTIALS_AGENT, vec![document_signing()]),
        )
        .key_pem
    }

    #[test]
    fn a_signed_offer_survives_json_and_rkyv_and_is_still_believed() {
        use crate::workload_issuance::WorkloadIssuanceReply;
        let ca = root("Polar Internal CA");
        let signer = signer_for(
            &ca,
            spec(Kind::P256, CREDENTIALS_AGENT, vec![document_signing()]),
        );
        let offer = an_offer(&signer);
        let from_json: SessionOffer =
            serde_json::from_str(&serde_json::to_string(&offer).unwrap()).unwrap();
        assert_eq!(check(&ca).check(&from_json, now()), Ok(()));
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&WorkloadIssuanceReply::Offer(offer))
            .unwrap()
            .to_vec();
        match rkyv::from_bytes::<WorkloadIssuanceReply, rkyv::rancor::Error>(&bytes).unwrap() {
            WorkloadIssuanceReply::Offer(back) => {
                assert_eq!(check(&ca).check(&back, now()), Ok(()))
            }
            other => panic!("expected an offer, got {other:?}"),
        }
    }
}
