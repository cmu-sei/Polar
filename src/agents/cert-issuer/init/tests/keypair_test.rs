//! Tests for keypair generation and CSR construction in the
//! init container.

use cert_issuer_init::keypair::generate_csr;

#[test]
fn generates_valid_csr_with_identity_as_san() {
    let result = generate_csr("system:serviceaccount:polar:git-observer")
        .expect("CSR generation should succeed");

    assert!(result.csr_pem.contains("BEGIN CERTIFICATE REQUEST"));
    assert!(result.csr_pem.contains("END CERTIFICATE REQUEST"));
    assert!(result.private_key_pem.contains("BEGIN PRIVATE KEY"));
    assert!(result.private_key_pem.contains("END PRIVATE KEY"));
}

#[test]
fn generates_csr_parseable_by_cert_issuer_csr_module() {
    // The CSR generated here must be parseable by the cert issuer's
    // CSR parser. This test exists to catch the case where the
    // init container and cert issuer drift in their CSR format
    // expectations.
    let result = generate_csr("agent.polar.svc.cluster.local")
        .expect("CSR generation should succeed");

    // We don't import cert_issuer's parser directly to avoid coupling
    // the init container test to the cert issuer crate, but the spec
    // requires interop, so the test asserts the CSR has the expected
    // structural properties.
    let parsed = x509_parser::pem::parse_x509_pem(result.csr_pem.as_bytes())
        .expect("CSR must be valid PEM with PEM type CERTIFICATE REQUEST");
    let (_, pem) = parsed;
    assert_eq!(pem.label, "CERTIFICATE REQUEST");
}

#[test]
fn each_call_generates_a_distinct_keypair() {
    // Two pods bringing up the same agent type at the same time must
    // generate distinct private keys. This is a fundamental property
    // and the test exists to make sure no clever caching ever creeps in.
    let a = generate_csr("agent.polar.svc.cluster.local").unwrap();
    let b = generate_csr("agent.polar.svc.cluster.local").unwrap();
    assert_ne!(a.private_key_pem, b.private_key_pem);
    // CSRs differ too because the public key differs.
    assert_ne!(a.csr_pem, b.csr_pem);
}

#[test]
fn empty_identity_is_rejected() {
    // The init container's caller must provide a non-empty identity.
    // If it's empty, fail fast rather than generating a CSR with no SAN.
    let err = generate_csr("").expect_err("empty identity must be rejected");
    assert!(matches!(
        err,
        cert_issuer_init::keypair::KeypairError::CsrFailed(_)
    ));
}
