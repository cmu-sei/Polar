// cert_issuer_common/src/identity.rs

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("unrecognized identity format: {0}")]
    UnrecognizedFormat(String),
}

/// Map a JWT `sub` claim to a DNS-safe identity string suitable for
/// use as a DNS SAN in a CSR.
///
/// v1 supports only Kubernetes service account tokens, whose `sub`
/// claim has the form `system:serviceaccount:<namespace>:<name>`.
/// This maps to `<name>.<namespace>.serviceaccount.cluster.local`.
///
/// The mapping is intentionally simple and reversible. The suffix
/// `.serviceaccount.cluster.local` is fixed — it unambiguously
/// scopes the identity to the Kubernetes SA namespace and won't
/// collide with real DNS names or SPIFFE IDs.
///
/// This function is the single source of truth for the mapping.
/// Both the cert issuer (called before `verify_identity`) and the
/// init container (called before `generate_csr`) import this
/// function. There is no "cert issuer version" and "init container
/// version" — drift between the two sides is structurally impossible.
pub fn normalize_identity(sub: &str) -> Result<String, IdentityError> {
    if sub.is_empty() {
        return Err(IdentityError::UnrecognizedFormat(sub.to_string()));
    }

    // Expected: "system:serviceaccount:<namespace>:<name>"
    let parts: Vec<&str> = sub.splitn(4, ':').collect();
    if parts.len() != 4
        || parts[0] != "system"
        || parts[1] != "serviceaccount"
        || parts[2].is_empty()
        || parts[3].is_empty()
    {
        return Err(IdentityError::UnrecognizedFormat(sub.to_string()));
    }

    let namespace = parts[2];
    let name = parts[3];

    Ok(format!("{name}.{namespace}.serviceaccount.cluster.local"))
}

#[cfg(test)]
mod unit_tests {
    // tests/identity_tests.rs
    //! Tests for the identity normalization function in cert_issuer_common.
    //!
    //! This is the most operationally critical mapping in the system.
    //! The init container calls normalize_identity before generate_csr;
    //! the cert issuer calls normalize_identity before verify_identity.
    //! If these produce different output for the same input, every
    //! issuance attempt fails with IdentityMismatch and no cert is issued.
    //!
    //! The function lives in cert_issuer_common so both crates import
    //! the same implementation — there is no "init container version"
    //! and "cert issuer version."

    use super::IdentityError;
    use super::normalize_identity;

    #[test]
    fn kubernetes_sa_identity_normalizes_to_dns_form() {
        // The canonical Kubernetes SA subject: system:serviceaccount:<ns>:<name>
        // Maps to <name>.<ns>.serviceaccount.cluster.local
        assert_eq!(
            normalize_identity("system:serviceaccount:polar:git-observer"),
            Ok("git-observer.polar.serviceaccount.cluster.local".to_string()),
        );
    }

    #[test]
    fn normalization_is_deterministic() {
        let a = normalize_identity("system:serviceaccount:polar:git-observer").unwrap();
        let b = normalize_identity("system:serviceaccount:polar:git-observer").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_identities_produce_different_dns_names() {
        let a = normalize_identity("system:serviceaccount:polar:git-observer").unwrap();
        let b = normalize_identity("system:serviceaccount:polar:oci-resolver").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn different_namespaces_produce_different_dns_names() {
        let a = normalize_identity("system:serviceaccount:polar:git-observer").unwrap();
        let b = normalize_identity("system:serviceaccount:staging:git-observer").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn non_kubernetes_sub_claim_is_rejected() {
        // v1 only supports Kubernetes SA tokens. A sub claim from a
        // GitLab OIDC token or AWS IRSA will not match the expected
        // prefix and must be rejected with a clear error rather than
        // producing a nonsense DNS name.
        let err = normalize_identity("project_path:mygroup/myproject:ref_type:branch:ref:main")
            .expect_err("non-Kubernetes sub must be rejected in v1");
        assert!(
            matches!(err, IdentityError::UnrecognizedFormat(_)),
            "expected UnrecognizedFormat, got {err:?}"
        );
    }

    #[test]
    fn empty_input_is_rejected() {
        let err = normalize_identity("").expect_err("empty input must be rejected");
        assert!(matches!(err, IdentityError::UnrecognizedFormat(_)));
    }

    #[test]
    fn missing_name_segment_is_rejected() {
        // Malformed: only three colon-separated segments instead of four.
        let err = normalize_identity("system:serviceaccount:polar")
            .expect_err("malformed SA string must be rejected");
        assert!(matches!(err, IdentityError::UnrecognizedFormat(_)));
    }

    #[test]
    fn output_is_a_valid_dns_name() {
        // Every segment of the output must be a valid DNS label:
        // letters, digits, hyphens only; no underscores, no colons,
        // no dots within a segment.
        let dns = normalize_identity("system:serviceaccount:polar:git-observer").unwrap();
        for label in dns.split('.') {
            assert!(!label.is_empty(), "DNS label must not be empty: {dns:?}");
            assert!(
                label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "DNS label '{label}' contains invalid characters in '{dns}'"
            );
        }
    }
}
