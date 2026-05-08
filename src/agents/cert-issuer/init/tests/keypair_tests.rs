// tests/keypair_tests.rs
//! Tests for Ed25519 keypair generation and CSR construction.
//!
//! The init container is the only producer of CSRs in this system.
//! These tests pin down the exact structural properties the cert
//! issuer's parser expects, so drift between the two sides is caught
//! here rather than at runtime as an IdentityMismatch or NoSan error.
//!
//! The identity passed to `generate_csr` must already be DNS-safe.
//! Normalization from Kubernetes SA form to DNS form is tested
//! separately in cert_issuer_common's identity tests.

use cert_issuer_init::keypair::{CsrOutput, KeypairError, generate_csr};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::*;

/// Parse the CSR PEM and extract DNS SANs using the same logic
/// the cert issuer's csr module uses. This is the interop check —
/// if this function returns the expected identity, the cert issuer
/// will too.
fn extract_dns_sans_from_csr(csr_pem: &str) -> Vec<String> {
    let (_, pem) = parse_x509_pem(csr_pem.as_bytes()).expect("valid PEM");
    let (_, csr) = X509CertificationRequest::from_der(&pem.contents).expect("valid DER");
    csr.verify_signature().expect("valid self-signature");

    let mut names = Vec::new();
    if let Some(exts) = csr.requested_extensions() {
        for ext in exts {
            if let ParsedExtension::SubjectAlternativeName(san) = ext {
                for name in &san.general_names {
                    if let GeneralName::DNSName(dns) = name {
                        names.push(dns.to_string());
                    }
                }
                break;
            }
        }
    }
    names
}

#[test]
fn generates_valid_pem_blocks() {
    let out = generate_csr("git-observer.polar.serviceaccount.cluster.local")
        .expect("generation must succeed for a valid DNS identity");

    assert!(
        out.csr_pem.contains("-----BEGIN CERTIFICATE REQUEST-----"),
        "CSR PEM must have correct header"
    );
    assert!(
        out.csr_pem.contains("-----END CERTIFICATE REQUEST-----"),
        "CSR PEM must have correct footer"
    );
    assert!(
        out.private_key_pem.contains("-----BEGIN PRIVATE KEY-----"),
        "key PEM must use PKCS#8 header, not algorithm-specific header"
    );
    assert!(
        out.private_key_pem.contains("-----END PRIVATE KEY-----"),
        "key PEM must have correct footer"
    );
}

#[test]
fn csr_contains_identity_as_dns_san() {
    let identity = "git-observer.polar.serviceaccount.cluster.local";
    let out = generate_csr(identity).expect("generation must succeed");

    let sans = extract_dns_sans_from_csr(&out.csr_pem);
    assert_eq!(
        sans,
        vec![identity],
        "CSR must contain exactly the provided identity as a DNS SAN"
    );
}

#[test]
fn csr_contains_exactly_one_san() {
    // The cert issuer's parser rejects multi-SAN CSRs. The generator
    // must never produce more than one SAN regardless of input.
    let out = generate_csr("git-observer.polar.serviceaccount.cluster.local")
        .expect("generation must succeed");

    let sans = extract_dns_sans_from_csr(&out.csr_pem);
    assert_eq!(
        sans.len(),
        1,
        "CSR must contain exactly one DNS SAN, got: {sans:?}"
    );
}

#[test]
fn csr_self_signature_verifies() {
    // The cert issuer verifies the CSR's self-signature before
    // trusting any of its contents. If this fails, every issuance
    // attempt will be rejected with InvalidSignature regardless of
    // whether the token is valid.
    let out = generate_csr("git-observer.polar.serviceaccount.cluster.local")
        .expect("generation must succeed");

    let (_, pem) = parse_x509_pem(out.csr_pem.as_bytes()).expect("valid PEM");
    let (_, csr) = X509CertificationRequest::from_der(&pem.contents).expect("valid DER");
    csr.verify_signature()
        .expect("CSR self-signature must verify against its own public key");
}

#[test]
fn key_algorithm_is_ed25519() {
    // Ed25519 is the required algorithm per the architecture spec.
    // If rcgen's default changes or the generator is updated to use
    // a different algorithm, this test catches it before the cert
    // issuer rejects the CSR with DisallowedAlgorithm.
    let out = generate_csr("git-observer.polar.serviceaccount.cluster.local")
        .expect("generation must succeed");

    let (_, pem) = parse_x509_pem(out.csr_pem.as_bytes()).expect("valid PEM");
    let (_, csr) = X509CertificationRequest::from_der(&pem.contents).expect("valid DER");

    let oid = &csr
        .certification_request_info
        .subject_pki
        .algorithm
        .algorithm;
    // Ed25519 OID: 1.3.101.112 (RFC 8410)
    assert_eq!(
        oid.to_id_string(),
        "1.3.101.112",
        "key algorithm must be Ed25519"
    );
}

#[test]
fn each_call_produces_distinct_keypair() {
    // Two pods starting the same agent simultaneously must not share
    // a private key. This is a fundamental security property.
    let a = generate_csr("git-observer.polar.serviceaccount.cluster.local").unwrap();
    let b = generate_csr("git-observer.polar.serviceaccount.cluster.local").unwrap();

    assert_ne!(
        a.private_key_pem, b.private_key_pem,
        "each invocation must produce a distinct private key"
    );
    assert_ne!(
        a.csr_pem, b.csr_pem,
        "CSRs must differ because public keys differ"
    );
}

#[test]
fn empty_identity_is_rejected() {
    let err = generate_csr("").expect_err("empty identity must be rejected before CSR generation");
    assert!(
        matches!(err, KeypairError::EmptyIdentity),
        "expected EmptyIdentity, got {err:?}"
    );
}

#[test]
fn csr_label_is_certificate_request_not_new_certificate_request() {
    // The cert issuer's PEM parser rejects "NEW CERTIFICATE REQUEST"
    // labels. Some older tooling emits this. rcgen emits the correct
    // label but this test pins that so a rcgen version bump that
    // changes it is caught immediately.
    let out = generate_csr("git-observer.polar.serviceaccount.cluster.local").unwrap();
    let (_, pem) = parse_x509_pem(out.csr_pem.as_bytes()).expect("valid PEM");
    assert_eq!(
        pem.label, "CERTIFICATE REQUEST",
        "PEM label must be 'CERTIFICATE REQUEST', not 'NEW CERTIFICATE REQUEST'"
    );
}
