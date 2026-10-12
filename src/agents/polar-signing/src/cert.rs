//! What is read from a certificate, in one place.

use crate::document::Algorithm;
use x509_parser::certificate::X509Certificate;

const OID_ED25519: &str = "1.3.101.112";
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_P256: &str = "1.2.840.10045.3.1.7";

/// The DER of the first certificate in a PEM string.
pub(crate) fn der_from_pem(pem: &str) -> Result<Vec<u8>, String> {
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map(|(_, block)| block.contents)
        .map_err(|e| e.to_string())
}

pub(crate) fn parse(der: &[u8]) -> Result<X509Certificate<'_>, String> {
    x509_parser::parse_x509_certificate(der)
        .map(|(_, cert)| cert)
        .map_err(|e| e.to_string())
}

/// The signature algorithm the certificate's key supports, and the key's bytes, if it is one this
/// system signs with: Ed25519 or ECDSA on P-256.
pub(crate) fn key_of(cert: &X509Certificate<'_>) -> Option<(Algorithm, Vec<u8>)> {
    let spki = cert.public_key();
    let oid = spki.algorithm.algorithm.to_id_string();
    let bytes = spki.subject_public_key.data.to_vec();
    if oid == OID_ED25519 {
        return Some((Algorithm::Ed25519, bytes));
    }
    if oid == OID_EC_PUBLIC_KEY {
        let curve = spki
            .algorithm
            .parameters
            .as_ref()
            .and_then(|p| p.as_oid().ok())
            .map(|o| o.to_id_string());
        if curve.as_deref() == Some(OID_P256) {
            return Some((Algorithm::EcdsaP256Sha256, bytes));
        }
    }
    None
}
