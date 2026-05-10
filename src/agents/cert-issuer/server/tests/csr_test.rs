//! Unit tests for CSR parsing.
//!
//! Generates real CSRs with rcgen, then feeds them to the parser to
//! verify both the happy path and the rejection cases.

use cert_issuer::csr::{CsrError, parse_csr, verify_identity};
use rcgen::{CertificateParams, KeyPair};

fn generate_csr_pem(sans: Vec<String>) -> String {
    let params = CertificateParams::new(sans).expect("valid params");
    let key_pair = KeyPair::generate().expect("keypair generation");
    let csr = params
        .serialize_request(&key_pair)
        .expect("CSR serialization");
    csr.pem().expect("PEM encoding")
}

#[test]
fn well_formed_csr_with_single_san_parses() {
    let pem = generate_csr_pem(vec!["agent.polar.svc.cluster.local".to_string()]);
    let parsed = parse_csr(&pem).expect("CSR should parse");
    assert_eq!(parsed.san, vec!["agent.polar.svc.cluster.local"]);
}

#[test]
fn malformed_pem_is_rejected() {
    let bad_pem =
        "-----BEGIN CERTIFICATE REQUEST-----\nnot base64\n-----END CERTIFICATE REQUEST-----";
    let err = parse_csr(bad_pem).expect_err("malformed PEM must be rejected");
    assert_eq!(err, CsrError::Malformed);
}

#[test]
fn empty_string_is_rejected() {
    let err = parse_csr("").expect_err("empty input must be rejected");
    assert_eq!(err, CsrError::Malformed);
}

#[test]
fn csr_with_multiple_sans_is_rejected_in_v1() {
    // v1's identity binding is unambiguous: one SAN, one identity.
    // Multi-SAN CSRs are rejected. This is a v1 policy choice; v2
    // may relax it if we have a use case.
    let pem = generate_csr_pem(vec![
        "agent.polar.svc.cluster.local".to_string(),
        "other.polar.svc.cluster.local".to_string(),
    ]);
    let err = parse_csr(&pem).expect_err("multi-SAN must be rejected in v1");
    assert_eq!(err, CsrError::MultipleSans);
}

#[test]
fn csr_with_no_san_is_rejected() {
    let pem = generate_csr_pem(vec![]);
    let err = parse_csr(&pem).expect_err("no-SAN CSR must be rejected");
    assert_eq!(err, CsrError::NoSan);
}

#[test]
fn ed25519_key_is_recognized() {
    // rcgen defaults vary; this test pins down that whatever rcgen
    // emits gets a recognizable algorithm string. If rcgen emits
    // something we don't recognize, the parser should call it out.
    let pem = generate_csr_pem(vec!["agent.polar.svc.cluster.local".to_string()]);
    let parsed = parse_csr(&pem).expect("CSR should parse");
    assert!(
        cert_issuer::csr::allowed_key_algorithms()
            .iter()
            .any(|a| *a == parsed.key_algorithm),
        "key algorithm '{}' should be in allowed list",
        parsed.key_algorithm,
    );
}

// Identity verification tests

#[test]
fn matching_san_and_identity_passes() {
    let pem = generate_csr_pem(vec!["agent.polar.svc.cluster.local".to_string()]);
    let parsed = parse_csr(&pem).expect("CSR parses");
    let result = verify_identity(&parsed, "agent.polar.svc.cluster.local");
    assert!(result.is_ok());
}

#[test]
fn san_mismatch_is_rejected() {
    let pem = generate_csr_pem(vec!["impersonator.polar.svc.cluster.local".to_string()]);
    let parsed = parse_csr(&pem).expect("CSR parses");
    let err = verify_identity(&parsed, "victim.polar.svc.cluster.local")
        .expect_err("mismatch must be rejected");
    assert!(matches!(err, CsrError::IdentityMismatch { .. }));
}

#[test]
fn case_sensitivity_is_enforced_in_identity_match() {
    // SAN matching is exact-string. Case differences are mismatches.
    // This test pins down the policy so it doesn't drift.
    let pem = generate_csr_pem(vec!["Agent.Polar.Svc.Cluster.Local".to_string()]);
    let parsed = parse_csr(&pem).expect("CSR parses");
    let err = verify_identity(&parsed, "agent.polar.svc.cluster.local")
        .expect_err("case mismatch must be rejected");
    assert!(matches!(err, CsrError::IdentityMismatch { .. }));
}

#[test]
fn whitespace_in_identity_is_not_trimmed() {
    // If somehow whitespace ends up in either side, we don't silently
    // strip it. Mismatch is mismatch.
    let pem = generate_csr_pem(vec!["agent.polar.svc.cluster.local".to_string()]);
    let parsed = parse_csr(&pem).expect("CSR parses");
    let err = verify_identity(&parsed, "agent.polar.svc.cluster.local ")
        .expect_err("trailing whitespace must be a mismatch");
    assert!(matches!(err, CsrError::IdentityMismatch { .. }));
}
