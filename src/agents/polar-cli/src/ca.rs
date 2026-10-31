//! A certificate authority for a cluster: a key pair and its self-signed certificate.
//!
//! The certificate issuer signs every certificate in the cluster with one, so a plan that stands the
//! cluster up has to make it, once, before anything runs. It is made here the way the fork's own
//! `bootstrap-ca` makes it (an unconstrained CA, an ECDSA P-256 key, both as PEM), so the issuer
//! loads what this makes; the tests load it the way the issuer does.
//!
//! Nothing here prints the key. [`Authority`] shows `<secret>` for it in `Debug`.

use crate::secrets::Secret;
use anyhow::{Context, Result};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use time::{Duration, OffsetDateTime};

pub struct Authority {
    /// The self-signed certificate, PEM. Public.
    pub certificate: String,
    /// The private key, PKCS#8 PEM.
    pub key: Secret,
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Authority {{ certificate: {} bytes, key: {:?} }}", self.certificate.len(), self.key)
    }
}

/// Make a CA named `common_name` that is valid from now for `validity_days`.
pub fn generate(common_name: &str, validity_days: u32) -> Result<Authority> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(DnType::CommonName, common_name);
    params.not_before = OffsetDateTime::now_utc();
    params.not_after = OffsetDateTime::now_utc() + Duration::days(i64::from(validity_days));

    let key_pair = KeyPair::generate().context("cannot make the CA's key")?;
    let certificate = params.self_signed(&key_pair).context("cannot sign the CA's certificate")?;
    Ok(Authority { certificate: certificate.pem(), key: Secret::new(key_pair.serialize_pem()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateSigningRequestParams, Issuer};
    use x509_parser::pem::parse_x509_pem;

    #[test]
    fn it_makes_a_certificate_and_a_key_in_pem() {
        let ca = generate("Test CA", 30).unwrap();
        assert!(ca.certificate.contains("BEGIN CERTIFICATE"));
        assert!(ca.key.expose().contains("BEGIN PRIVATE KEY"), "PKCS#8, which is what the issuer reads");
        assert!(!ca.certificate.contains("PRIVATE KEY"));
    }

    #[test]
    fn it_is_a_ca_with_the_name_and_lifetime_asked_for() {
        let ca = generate("Polar Internal CA", 3650).unwrap();
        let (_, pem) = parse_x509_pem(ca.certificate.as_bytes()).unwrap();
        let cert = pem.parse_x509().unwrap();
        assert!(cert.is_ca());
        assert_eq!(cert.subject().iter_common_name().next().unwrap().as_str().unwrap(), "Polar Internal CA");
        let days = (cert.validity().not_after.timestamp() - cert.validity().not_before.timestamp()) / 86_400;
        assert_eq!(days, 3650);
        assert!(cert.verify_signature(None).is_ok(), "self-signed by its own key");
    }

    /// The issuer's own loading: the key from PEM, then the certificate with that key as an issuer, which
    /// signs a request. What it signs verifies under the certificate.
    #[test]
    fn the_issuer_can_load_it_and_sign_with_it() {
        let ca = generate("Polar Internal CA", 30).unwrap();
        let key = KeyPair::from_pem(ca.key.expose()).expect("the CA private key loads");
        let issuer = Issuer::from_ca_cert_pem(&ca.certificate, key).expect("the CA cert and key agree");

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf = CertificateParams::new(vec!["agent.polar.svc".to_string()]).unwrap();
        leaf.distinguished_name.push(DnType::CommonName, "agent");
        let csr = leaf.serialize_request(&leaf_key).unwrap();
        let csr = CertificateSigningRequestParams::from_pem(&csr.pem().unwrap()).unwrap();
        let signed = csr.signed_by(&issuer).unwrap();

        let (_, ca_pem) = parse_x509_pem(ca.certificate.as_bytes()).unwrap();
        let ca_cert = ca_pem.parse_x509().unwrap();
        let (_, leaf_pem) = parse_x509_pem(signed.pem().as_bytes()).unwrap();
        let leaf_cert = leaf_pem.parse_x509().unwrap();
        assert!(leaf_cert.verify_signature(Some(ca_cert.public_key())).is_ok());
    }

    #[test]
    fn two_are_never_the_same() {
        let a = generate("CA", 1).unwrap();
        let b = generate("CA", 1).unwrap();
        assert_ne!(a.certificate, b.certificate);
        assert_ne!(a.key, b.key);
    }

    #[test]
    fn the_key_is_not_in_a_debug_print() {
        let ca = generate("CA", 1).unwrap();
        let shown = format!("{ca:?}");
        assert!(!shown.contains("PRIVATE") && !shown.contains(ca.key.expose()), "{shown}");
        assert!(shown.contains("<secret>"));
    }
}
