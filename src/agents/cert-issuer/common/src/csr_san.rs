//! Reads the DNS SANs a certificate signing request asks for, so the
//! credentials agent can confirm a claimant built its CSR around the
//! account_id it was offered before asking cert-issuer to sign anything.
//! This only reads what is requested; cert-issuer itself still parses the
//! CSR and checks its signature.

use thiserror::Error;
use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::FromDer;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CsrSanError {
    #[error("not valid PEM: {0}")]
    BadPem(String),
    #[error("not a valid certificate signing request: {0}")]
    BadCsr(String),
}

/// Every DNS SAN in the CSR's requested extensions, in order. A CSR with no
/// SAN extension yields an empty list.
pub fn csr_dns_sans(csr_pem: &str) -> Result<Vec<String>, CsrSanError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(csr_pem.as_bytes())
        .map_err(|e| CsrSanError::BadPem(e.to_string()))?;
    let (_, csr) = X509CertificationRequest::from_der(&pem.contents)
        .map_err(|e| CsrSanError::BadCsr(e.to_string()))?;

    let mut sans = Vec::new();
    if let Some(extensions) = csr.requested_extensions() {
        for extension in extensions {
            if let ParsedExtension::SubjectAlternativeName(san) = extension {
                for name in &san.general_names {
                    if let GeneralName::DNSName(dns) = name {
                        sans.push(dns.to_string());
                    }
                }
            }
        }
    }
    Ok(sans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypair::{KeyAlgorithm, generate_csr};

    #[test]
    fn text_that_is_not_pem_is_refused() {
        assert!(matches!(
            csr_dns_sans("not pem"),
            Err(CsrSanError::BadPem(_))
        ));
    }

    #[test]
    fn a_generated_csr_yields_the_identity_it_was_built_around() {
        let csr = generate_csr("3f2c9a1e-0000-4000-8000-000000000001", &KeyAlgorithm::Ed25519, &[])
            .expect("CSR generation succeeds");
        let sans = csr_dns_sans(&csr.csr_pem).expect("the CSR parses");
        assert!(
            sans.contains(&"3f2c9a1e-0000-4000-8000-000000000001".to_string()),
            "SANs were {sans:?}"
        );
    }

    #[test]
    fn extra_sans_are_reported_too() {
        let csr = generate_csr(
            "acct-1",
            &KeyAlgorithm::EcdsaP256,
            &["p:u2s".to_string()],
        )
        .expect("CSR generation succeeds");
        let sans = csr_dns_sans(&csr.csr_pem).expect("the CSR parses");
        assert!(sans.contains(&"acct-1".to_string()), "SANs were {sans:?}");
        assert!(sans.contains(&"p:u2s".to_string()), "SANs were {sans:?}");
    }
}
