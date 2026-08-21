// cert_issuer_common/src/identity.rs

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("unrecognized identity format: {0}")]
    UnrecognizedFormat(String),
}

/// How a validated token's claims are turned into a DNS-safe identity
/// string suitable for use as a DNS SAN in a CSR.
///
/// This is not a formatting choice -- it's the mechanism that gives the
/// resulting certificate a real, disambiguating uniqueness guarantee.
/// A bare claim value (a username, say) is only unique *within* the
/// issuer that vouched for it; embedding that issuer's own scope into
/// the output is what makes two different issuers' identical usernames
/// resolve to two different, non-colliding certificate identities.
///
/// Configured per issuer (`IssuerConfig.identity_normalization`), not
/// global -- different issuers legitimately need different strategies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum IdentityNormalization {
    /// v1's original, unchanged behavior. Parses a Kubernetes service
    /// account token's `sub` claim,
    /// `system:serviceaccount:<namespace>:<name>`, producing
    /// `<name>.<namespace>.serviceaccount.<cluster_domain>`.
    ///
    /// `cluster_domain` is genuine server configuration, not something
    /// derived from the token -- Kubernetes service account tokens
    /// carry no claim describing the cluster's own DNS domain. It
    /// defaults to `cluster.local` (Kubernetes' own convention) at the
    /// config layer, but MUST be overridable: a real cluster's kubelet
    /// can be configured with a different `--cluster-domain`, and if
    /// this cert-issuer is ever configured to trust more than one
    /// Kubernetes cluster, each one's own domain is what actually keeps
    /// their identities from colliding.
    KubernetesServiceAccount { cluster_domain: String },

    /// For issuers whose claim is a bare, unscoped identity with no
    /// structure of its own -- Keycloak's `preferred_username`, for
    /// example. The scope value is a second claim from the *same*
    /// token (named by the issuer's own `identity_scope_claim`
    /// config, extracted by the caller and passed in already-resolved
    /// as `scope_claim_value`) -- kept inside the token itself, via
    /// that issuer's own claim-mapper configuration, rather than
    /// duplicated as separately-maintained server config that a
    /// client would also need to know and could drift from.
    SuffixedClaim,
}

/// Map a validated token's identity claim (and, for issuers that need
/// one, a second scope claim from the same token) to a DNS-safe
/// identity string suitable for use as a DNS SAN in a CSR.
///
/// This function is the single source of truth for the mapping. Both
/// the cert issuer (called before `verify_identity`) and every client
/// that generates its own CSR (`polar-init`, for Kubernetes workloads;
/// a future human-facing tool, for issuers using `SuffixedClaim`)
/// import this function. There is no per-side copy of the logic --
/// drift between what a client puts in its CSR and what the server
/// expects is structurally impossible, *provided* both sides are
/// configured with the same strategy for a given issuer. A mismatch
/// there is a reliability failure (every request from that
/// misconfigured side gets rejected), not a security hole -- the
/// server's own independent recomputation and exact-string comparison
/// in `verify_identity` is what actually prevents a forged identity
/// claim from ever being accepted.
pub fn normalize_identity(
    identity_claim_value: &str,
    scope_claim_value: Option<&str>,
    strategy: &IdentityNormalization,
) -> Result<String, IdentityError> {
    match strategy {
        IdentityNormalization::KubernetesServiceAccount { cluster_domain } => {
            if identity_claim_value.is_empty() {
                return Err(IdentityError::UnrecognizedFormat(
                    identity_claim_value.to_string(),
                ));
            }

            // Expected: "system:serviceaccount:<namespace>:<name>"
            let parts: Vec<&str> = identity_claim_value.splitn(4, ':').collect();
            if parts.len() != 4
                || parts[0] != "system"
                || parts[1] != "serviceaccount"
                || parts[2].is_empty()
                || parts[3].is_empty()
            {
                return Err(IdentityError::UnrecognizedFormat(
                    identity_claim_value.to_string(),
                ));
            }

            let namespace = parts[2];
            let name = parts[3];

            Ok(format!("{name}.{namespace}.serviceaccount.{cluster_domain}"))
        }
        IdentityNormalization::SuffixedClaim => {
            let Some(scope) = scope_claim_value else {
                // Config-time validation (ServiceConfig::validate) is
                // meant to reject a SuffixedClaim issuer with no
                // identity_scope_claim configured before this can ever
                // happen in practice. Handled defensively here anyway
                // rather than panicking -- there's nothing more
                // specific to say than "this token doesn't have what
                // this strategy needs."
                return Err(IdentityError::UnrecognizedFormat(
                    identity_claim_value.to_string(),
                ));
            };

            if identity_claim_value.is_empty()
                || !is_dns_safe(identity_claim_value)
                || scope.is_empty()
                || !is_dns_safe(scope)
            {
                return Err(IdentityError::UnrecognizedFormat(
                    identity_claim_value.to_string(),
                ));
            }

            Ok(format!("{identity_claim_value}.{scope}"))
        }
    }
}

/// Every dot-separated segment of `value` must be a valid DNS label:
/// letters, digits, and hyphens only, and non-empty. Applied to raw
/// claim values before they become part of a SAN -- unlike the
/// Kubernetes strategy, which only ever extracts already-bounded
/// namespace/name substrings via its own colon-split, a claim value
/// used directly by `SuffixedClaim` has had no such implicit
/// validation and needs an explicit check here.
fn is_dns_safe(value: &str) -> bool {
    value.split('.').all(|label| {
        !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

#[cfg(test)]
mod unit_tests {
    //! Tests for identity normalization in cert_issuer_common.
    //!
    //! This is the most operationally critical mapping in the system.
    //! A client calls normalize_identity before generate_csr; the cert
    //! issuer calls it before verify_identity. If the two sides ever
    //! produce different output for the same input, every issuance
    //! attempt fails with IdentityMismatch and no cert is issued.
    //!
    //! The function lives in cert_issuer_common so every crate that
    //! needs it imports the same implementation -- there is no
    //! per-side copy to drift.

    use super::{IdentityError, IdentityNormalization, normalize_identity};

    fn k8s(cluster_domain: &str) -> IdentityNormalization {
        IdentityNormalization::KubernetesServiceAccount {
            cluster_domain: cluster_domain.to_string(),
        }
    }

    #[test]
    fn kubernetes_sa_identity_normalizes_to_dns_form() {
        assert_eq!(
            normalize_identity(
                "system:serviceaccount:polar:git-observer",
                None,
                &k8s("cluster.local"),
            ),
            Ok("git-observer.polar.serviceaccount.cluster.local".to_string()),
        );
    }

    #[test]
    fn kubernetes_cluster_domain_is_configurable() {
        // The whole point of making this a parameter: a real cluster's
        // kubelet can run with a different --cluster-domain, and this
        // must not be a ceiling on what's possible.
        assert_eq!(
            normalize_identity(
                "system:serviceaccount:polar:git-observer",
                None,
                &k8s("prod.internal"),
            ),
            Ok("git-observer.polar.serviceaccount.prod.internal".to_string()),
        );
    }

    #[test]
    fn different_cluster_domains_produce_different_dns_names() {
        // Two IssuerConfigs trusting two different Kubernetes clusters,
        // each with a service account named "default" in namespace
        // "polar", must not collide -- this is the real security
        // property the configurable suffix protects, not cosmetics.
        let a = normalize_identity(
            "system:serviceaccount:polar:default",
            None,
            &k8s("cluster-a.local"),
        )
        .unwrap();
        let b = normalize_identity(
            "system:serviceaccount:polar:default",
            None,
            &k8s("cluster-b.local"),
        )
        .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn normalization_is_deterministic() {
        let a = normalize_identity(
            "system:serviceaccount:polar:git-observer",
            None,
            &k8s("cluster.local"),
        )
        .unwrap();
        let b = normalize_identity(
            "system:serviceaccount:polar:git-observer",
            None,
            &k8s("cluster.local"),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_identities_produce_different_dns_names() {
        let a = normalize_identity(
            "system:serviceaccount:polar:git-observer",
            None,
            &k8s("cluster.local"),
        )
        .unwrap();
        let b = normalize_identity(
            "system:serviceaccount:polar:oci-resolver",
            None,
            &k8s("cluster.local"),
        )
        .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn different_namespaces_produce_different_dns_names() {
        let a = normalize_identity(
            "system:serviceaccount:polar:git-observer",
            None,
            &k8s("cluster.local"),
        )
        .unwrap();
        let b = normalize_identity(
            "system:serviceaccount:staging:git-observer",
            None,
            &k8s("cluster.local"),
        )
        .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn non_kubernetes_sub_claim_is_rejected_under_kubernetes_strategy() {
        let err = normalize_identity(
            "project_path:mygroup/myproject:ref_type:branch:ref:main",
            None,
            &k8s("cluster.local"),
        )
        .expect_err("non-Kubernetes-shaped claim must be rejected under this strategy");
        assert!(matches!(err, IdentityError::UnrecognizedFormat(_)));
    }

    #[test]
    fn empty_input_is_rejected_under_kubernetes_strategy() {
        let err = normalize_identity("", None, &k8s("cluster.local"))
            .expect_err("empty input must be rejected");
        assert!(matches!(err, IdentityError::UnrecognizedFormat(_)));
    }

    #[test]
    fn missing_name_segment_is_rejected_under_kubernetes_strategy() {
        let err = normalize_identity("system:serviceaccount:polar", None, &k8s("cluster.local"))
            .expect_err("malformed SA string must be rejected");
        assert!(matches!(err, IdentityError::UnrecognizedFormat(_)));
    }

    #[test]
    fn kubernetes_output_is_a_valid_dns_name() {
        let dns = normalize_identity(
            "system:serviceaccount:polar:git-observer",
            None,
            &k8s("cluster.local"),
        )
        .unwrap();
        for label in dns.split('.') {
            assert!(!label.is_empty(), "DNS label must not be empty: {dns:?}");
            assert!(
                label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "DNS label '{label}' contains invalid characters in '{dns}'"
            );
        }
    }

    #[test]
    fn suffixed_claim_combines_identity_and_scope() {
        assert_eq!(
            normalize_identity(
                "testuser",
                Some("polar.keycloak-issuer"),
                &IdentityNormalization::SuffixedClaim,
            ),
            Ok("testuser.polar.keycloak-issuer".to_string()),
        );
    }

    #[test]
    fn suffixed_claim_without_a_scope_value_is_rejected() {
        // Config-time validation is meant to prevent this from ever
        // happening in a real deployment; the function still handles
        // it defensively rather than panicking.
        let err = normalize_identity("testuser", None, &IdentityNormalization::SuffixedClaim)
            .expect_err("a SuffixedClaim strategy with no scope value must be rejected");
        assert!(matches!(err, IdentityError::UnrecognizedFormat(_)));
    }

    #[test]
    fn suffixed_claim_rejects_unsafe_identity_characters() {
        // A raw claim value has had no implicit validation the way
        // Kubernetes's colon-split segments have -- this must be
        // checked explicitly.
        let err = normalize_identity(
            "test user!",
            Some("polar.keycloak-issuer"),
            &IdentityNormalization::SuffixedClaim,
        )
        .expect_err("unsafe characters in the identity claim must be rejected");
        assert!(matches!(err, IdentityError::UnrecognizedFormat(_)));
    }

    #[test]
    fn different_scopes_produce_different_identities() {
        // The whole point: the same bare username from two different
        // issuers/realms must not collide.
        let a = normalize_identity(
            "testuser",
            Some("realm-a.keycloak-issuer"),
            &IdentityNormalization::SuffixedClaim,
        )
        .unwrap();
        let b = normalize_identity(
            "testuser",
            Some("realm-b.keycloak-issuer"),
            &IdentityNormalization::SuffixedClaim,
        )
        .unwrap();
        assert_ne!(a, b);
    }
}
