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

//! Checking a signed document, and saying who signed it.
//!
//! In order, refusing at the first failure: the chain to a pinned CA (each certificate valid at `now`,
//! each issuer a CA, each signature on the way up good), the signer's certificate (a key this system signs
//! with, `digitalSignature` key usage), and then the signature over the domain line and the canonical
//! payload. Whether this signer may make this change is [`crate::authority`]'s question, asked next.

use crate::canonical::CanonicalError;
use crate::cert;
use crate::document::{Algorithm, Document, Domain, signing_input_in};
use cert_issuer_common::capability::{Capability, extract_capabilities};
use ed25519_dalek::Verifier as _;
use x509_parser::certificate::X509Certificate;
use x509_parser::der_parser::oid::Oid;
use x509_parser::time::ASN1Time;

/// The longest chain accepted, the signer's certificate included, anchor excluded.
const MAX_CHAIN: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("the certificate chain has {0} certificates, and at most {MAX_CHAIN} are accepted")]
    ChainTooLong(usize),
    #[error("a certificate in the chain does not parse: {0}")]
    BadCertificate(String),
    #[error("the signer's key is neither Ed25519 nor ECDSA P-256")]
    UnsupportedKeyType,
    #[error("the signature says {claimed} but the signer's certificate has a {actual} key")]
    AlgorithmMismatch {
        claimed: &'static str,
        actual: &'static str,
    },
    #[error("the certificate for `{0}` is not valid at the time of checking")]
    NotValidNow(String),
    #[error("the certificate for `{0}` was not issued by the next certificate in the chain")]
    ChainBroken(String),
    #[error("a certificate that issued another is not a CA: `{0}`")]
    IssuerNotCa(String),
    #[error("the chain does not lead to a CA this verifier trusts")]
    UntrustedRoot,
    #[error("the signer's certificate does not permit digital signatures")]
    KeyUsage,
    #[error("the signature does not verify")]
    BadSignature,
    #[error("the certificate is not made for encrypting to: it does not permit key encipherment")]
    NotForEncryption,
    #[error("the certificate to encrypt to is a CA certificate")]
    LeafIsCa,
    #[error("the certificate to encrypt to is not an escrow certificate: {0}")]
    NotAnEscrowCertificate(&'static str),
    #[error("the authority's certificate is not a signing certificate: {0}")]
    NotASigningCertificate(&'static str),
    #[error("the certificate does not name `{expected}`")]
    WrongIdentity { expected: String },
    #[error("the certificate's public key cannot be read: {0}")]
    NoUsableKey(String),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

/// The CA certificates a verifier trusts: pinned, never taken from a document.
pub struct Trust {
    anchors: Vec<Vec<u8>>,
}

impl Trust {
    pub fn from_pem(anchors: &[String]) -> Result<Self, VerifyError> {
        let anchors = anchors
            .iter()
            .map(|pem| cert::der_from_pem(pem).map_err(VerifyError::BadCertificate))
            .collect::<Result<Vec<_>, _>>()?;
        for der in &anchors {
            cert::parse(der).map_err(VerifyError::BadCertificate)?;
        }
        Ok(Self { anchors })
    }
}

/// Who signed, as their certificate says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerInfo {
    /// The `uid` in the certificate's Subject: the account's username.
    pub username: Option<String>,
    /// The `mail` in the Subject: the account's email.
    pub email: Option<String>,
    /// The first DNS name in the SAN: the identity the rest of the system knows the holder by.
    pub identity: Option<String>,
    pub serial: String,
    /// The `c:` capabilities the certificate carries.
    pub capabilities: Vec<Capability>,
}

fn name_of(cert: &X509Certificate<'_>) -> String {
    cert.subject().to_string()
}

fn valid_at(cert: &X509Certificate<'_>, now: i64) -> Result<(), VerifyError> {
    let at =
        ASN1Time::from_timestamp(now).map_err(|e| VerifyError::BadCertificate(e.to_string()))?;
    if cert.validity().is_valid_at(at) {
        Ok(())
    } else {
        Err(VerifyError::NotValidNow(name_of(cert)))
    }
}

fn issued_by(
    cert: &X509Certificate<'_>,
    issuer: &X509Certificate<'_>,
    now: i64,
) -> Result<(), VerifyError> {
    if cert.issuer() != issuer.subject() {
        return Err(VerifyError::ChainBroken(name_of(cert)));
    }
    let is_ca = matches!(issuer.basic_constraints(), Ok(Some(bc)) if bc.value.ca);
    if !is_ca {
        return Err(VerifyError::IssuerNotCa(name_of(issuer)));
    }
    valid_at(issuer, now)?;
    cert.verify_signature(Some(&issuer.tbs_certificate.subject_pki))
        .map_err(|_| VerifyError::ChainBroken(name_of(cert)))
}

/// The public key of the agent `identity`, from its escrow certificate, if the certificate can be believed:
/// valid at `now`, chaining to one of `trust`'s anchors, not a CA, made for key encipherment, an escrow certificate
/// by its extended key usage (`emailProtection`, and no TLS purpose), and carrying `identity` as a DNS name exactly. `chain_pem` is the certificate first and then, if there are any, the
/// certificates that lead to the anchor (the anchor itself may be the last).
///
/// This is what lets an author outside the cluster encrypt a credential to an agent on the word of a
/// gateway that speaks plain HTTP: the answer is believed because of what it is, not where it came from.
pub fn verify_escrow_certificate(
    chain_pem: &str,
    trust: &Trust,
    now: i64,
    identity: &str,
) -> Result<String, VerifyError> {
    believed_leaf(chain_pem, trust, now, identity, |leaf| {
        match leaf.key_usage() {
            Ok(Some(usage)) if usage.value.key_encipherment() => {}
            _ => return Err(VerifyError::NotForEncryption),
        }
        // The application layer's certificate, not one the transport layer would take.
        match leaf.extended_key_usage() {
            Ok(Some(eku)) if eku.value.any || eku.value.client_auth || eku.value.server_auth => {
                Err(VerifyError::NotAnEscrowCertificate(
                    "it may be used for TLS",
                ))
            }
            Ok(Some(eku)) if eku.value.email_protection => Ok(()),
            Ok(Some(_)) => Err(VerifyError::NotAnEscrowCertificate(
                "its extended key usage is not emailProtection",
            )),
            _ => Err(VerifyError::NotAnEscrowCertificate(
                "it has no extended key usage, so it is taken for any use",
            )),
        }
    })
}

/// The public key of the credentials agent `identity`'s authority key, from the certificate a snapshot of its authority
/// carries (docs/architecture/authority-distribution.md, "Pinning the key to the CA"), if the certificate can be
/// believed: valid at `now`, chaining to one of `trust`'s anchors, not a CA, permitting digital signatures, a signing
/// certificate by its extended key usage (`id-kp-documentSigning`, and no TLS purpose), and carrying `identity` as a DNS
/// name exactly. Then the key is the credentials agent's, and not merely whatever key the snapshot names.
pub fn verify_authority_certificate(
    chain_pem: &str,
    trust: &Trust,
    now: i64,
    identity: &str,
) -> Result<String, VerifyError> {
    believed_leaf(chain_pem, trust, now, identity, |leaf| {
        match leaf.key_usage() {
            Ok(Some(usage)) if usage.value.digital_signature() => {}
            _ => return Err(VerifyError::KeyUsage),
        }
        let document_signing =
            x509_parser::der_parser::oid::Oid::from(cert_issuer_common::OID_DOCUMENT_SIGNING)
                .map_err(|_| VerifyError::NotASigningCertificate("the document-signing OID"))?;
        match leaf.extended_key_usage() {
            Ok(Some(eku)) if eku.value.any || eku.value.client_auth || eku.value.server_auth => {
                Err(VerifyError::NotASigningCertificate(
                    "it may be used for TLS",
                ))
            }
            Ok(Some(eku)) if eku.value.other.contains(&document_signing) => Ok(()),
            Ok(Some(_)) => Err(VerifyError::NotASigningCertificate(
                "it is not for signing documents",
            )),
            _ => Err(VerifyError::NotASigningCertificate(
                "it has no extended key usage, so it is taken for any use",
            )),
        }
    })
}

/// The public key of the leaf of `chain_pem` if it chains to `trust`, is valid at `now`, is no CA, passes `purpose`, and
/// carries `identity` as a DNS name exactly: what the escrow and the authority certificates share.
fn believed_leaf(
    chain_pem: &str,
    trust: &Trust,
    now: i64,
    identity: &str,
    purpose: impl Fn(&X509Certificate<'_>) -> Result<(), VerifyError>,
) -> Result<String, VerifyError> {
    let blocks = pem_blocks(chain_pem);
    if blocks.is_empty() {
        return Err(VerifyError::BadCertificate(
            "no certificate was given".to_string(),
        ));
    }
    let mut ders = blocks
        .iter()
        .map(|pem| cert::der_from_pem(pem).map_err(VerifyError::BadCertificate))
        .collect::<Result<Vec<_>, _>>()?;
    if ders.len() > 1 && trust.anchors.contains(ders.last().expect("non-empty")) {
        ders.pop();
    }
    if ders.len() > MAX_CHAIN {
        return Err(VerifyError::ChainTooLong(ders.len()));
    }
    let certs = ders
        .iter()
        .map(|der| cert::parse(der).map_err(VerifyError::BadCertificate))
        .collect::<Result<Vec<_>, _>>()?;
    let leaf = certs.first().expect("non-empty");

    valid_at(leaf, now)?;
    for pair in certs.windows(2) {
        issued_by(&pair[0], &pair[1], now)?;
    }
    let top = certs.last().expect("non-empty");
    let anchored = trust.anchors.iter().any(|der| {
        cert::parse(der)
            .map(|anchor| issued_by(top, &anchor, now).is_ok())
            .unwrap_or(false)
    });
    if !anchored {
        return Err(VerifyError::UntrustedRoot);
    }

    if matches!(leaf.basic_constraints(), Ok(Some(bc)) if bc.value.ca) {
        return Err(VerifyError::LeafIsCa);
    }
    purpose(leaf)?;
    let names_it = leaf
        .subject_alternative_name()
        .ok()
        .flatten()
        .is_some_and(|san| {
            san.value.general_names.iter().any(|name| {
                matches!(name, x509_parser::extensions::GeneralName::DNSName(dns) if *dns == identity)
            })
        });
    if !names_it {
        return Err(VerifyError::WrongIdentity {
            expected: identity.to_string(),
        });
    }

    cert_issuer_common::public_key::extract_public_key_pem(&blocks[0])
        .map_err(|e| VerifyError::NoUsableKey(e.to_string()))
}

/// The certificates in a PEM string, one string each, in order.
fn pem_blocks(text: &str) -> Vec<String> {
    const END: &str = "-----END CERTIFICATE-----";
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN CERTIFICATE-----") {
        let Some(end) = rest[start..].find(END) else {
            break;
        };
        let stop = start + end + END.len();
        blocks.push(format!("{}\n", rest[start..stop].trim()));
        rest = &rest[stop..];
    }
    blocks
}

/// Checks a configuration document, and says who signed it.
pub fn verify(document: &Document, trust: &Trust, now: i64) -> Result<SignerInfo, VerifyError> {
    verify_in(Domain::CONFIGURATION, document, trust, now)
}

/// Checks a document signed for `domain`, as [`verify`] does for configuration.
pub fn verify_in(
    domain: Domain,
    document: &Document,
    trust: &Trust,
    now: i64,
) -> Result<SignerInfo, VerifyError> {
    let pems = &document.signature.certificate_chain;
    let mut ders = pems
        .iter()
        .map(|pem| cert::der_from_pem(pem).map_err(VerifyError::BadCertificate))
        .collect::<Result<Vec<_>, _>>()?;
    // A chain that carries the pinned root itself is trusted by the pin, not by what it carries.
    if ders.len() > 1 && trust.anchors.contains(ders.last().expect("non-empty")) {
        ders.pop();
    }
    if ders.len() > MAX_CHAIN {
        return Err(VerifyError::ChainTooLong(ders.len()));
    }
    let certs = ders
        .iter()
        .map(|der| cert::parse(der).map_err(VerifyError::BadCertificate))
        .collect::<Result<Vec<_>, _>>()?;
    let leaf = certs.first().expect("a document has a certificate");

    let (algorithm, public_key) = cert::key_of(leaf).ok_or(VerifyError::UnsupportedKeyType)?;
    if algorithm != document.signature.algorithm {
        return Err(VerifyError::AlgorithmMismatch {
            claimed: document.signature.algorithm.as_str(),
            actual: algorithm.as_str(),
        });
    }

    // The way up to a pinned CA.
    valid_at(leaf, now)?;
    for pair in certs.windows(2) {
        issued_by(&pair[0], &pair[1], now)?;
    }
    let top = certs.last().expect("non-empty");
    let anchored = trust.anchors.iter().any(|der| {
        cert::parse(der)
            .map(|anchor| issued_by(top, &anchor, now).is_ok())
            .unwrap_or(false)
    });
    if !anchored {
        return Err(VerifyError::UntrustedRoot);
    }

    // The signer's certificate is one to sign with.
    match leaf.key_usage() {
        Ok(Some(usage)) if usage.value.digital_signature() => {}
        _ => return Err(VerifyError::KeyUsage),
    }

    // The signature itself.
    let message = signing_input_in(domain, &document.payload)?;
    match algorithm {
        Algorithm::Ed25519 => {
            let bytes: [u8; 32] = public_key
                .as_slice()
                .try_into()
                .map_err(|_| VerifyError::UnsupportedKeyType)?;
            let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
                .map_err(|_| VerifyError::UnsupportedKeyType)?;
            let signature = ed25519_dalek::Signature::from_slice(&document.signature.value)
                .map_err(|_| VerifyError::BadSignature)?;
            key.verify_strict(&message, &signature)
                .map_err(|_| VerifyError::BadSignature)?;
        }
        Algorithm::EcdsaP256Sha256 => {
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&public_key)
                .map_err(|_| VerifyError::UnsupportedKeyType)?;
            let signature = p256::ecdsa::Signature::from_slice(&document.signature.value)
                .map_err(|_| VerifyError::BadSignature)?;
            key.verify(&message, &signature)
                .map_err(|_| VerifyError::BadSignature)?;
        }
    }

    Ok(signer_info(leaf, &pems[0]))
}

fn attribute(cert: &X509Certificate<'_>, arcs: &[u64]) -> Option<String> {
    let oid = Oid::from(arcs).ok()?;
    cert.subject()
        .iter_by_oid(&oid)
        .next()
        .and_then(|attr| attr.as_str().ok())
        .map(str::to_string)
}

fn signer_info(cert: &X509Certificate<'_>, pem: &str) -> SignerInfo {
    use x509_parser::extensions::GeneralName;
    let identity = cert
        .subject_alternative_name()
        .ok()
        .flatten()
        .and_then(|san| {
            san.value.general_names.iter().find_map(|name| match name {
                GeneralName::DNSName(dns) => Some((*dns).to_string()),
                _ => None,
            })
        });
    SignerInfo {
        username: attribute(cert, &[0, 9, 2342, 19200300, 100, 1, 1]),
        email: attribute(cert, &[0, 9, 2342, 19200300, 100, 1, 3]),
        identity,
        serial: cert_issuer_common::serial::serial_from_cert(cert),
        capabilities: extract_capabilities(pem).unwrap_or_default(),
    }
}
