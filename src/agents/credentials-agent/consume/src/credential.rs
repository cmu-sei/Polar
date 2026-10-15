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

//! Determines whether an incoming credential is a Kubernetes service
//! account token (skip RFC 7662 introspection -- it has no equivalent
//! endpoint) or something else (attempt introspection). No config
//! system duplicating cert-issuer's own multi-issuer IssuerConfig --
//! matched against this deployment's real, exact configured Kubernetes
//! issuer URL(s) only. Anything else is assumed to need introspection;
//! if introspection then genuinely fails, that's the real failure
//! signal, not a separate "unrecognized issuer" case.

pub const KUBERNETES_ISSUERS_DEFAULT: &str = "https://kubernetes.default.svc.cluster.local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Kubernetes,
    LikelyIdp,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialKindError {
    /// The credential has no peekable `iss` claim at all -- not even
    /// enough shape to guess at, let alone validate. Fails closed,
    /// per "if we can't figure it out, that's a fail" -- never
    /// defaults to attempting introspection on something this
    /// malformed.
    #[error("credential has no peekable iss claim")]
    NoIssuerClaim,
}

/// Classifies `credential` by peeking its unverified `iss` claim (via
/// `jwt_utils::peek_claim` -- never treated as trustworthy, purely a
/// routing decision) and exact-matching it against
/// `known_kubernetes_issuers`.
pub fn classify_credential(
    credential: &str,
    known_kubernetes_issuers: &[String],
) -> Result<CredentialKind, CredentialKindError> {
    let iss = jwt_utils::peek_claim(credential, "iss").ok_or(CredentialKindError::NoIssuerClaim)?;

    if known_kubernetes_issuers.iter().any(|k| k == &iss) {
        Ok(CredentialKind::Kubernetes)
    } else {
        Ok(CredentialKind::LikelyIdp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn make_token(iss: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{iss}"}}"#));
        format!("{header}.{payload}.")
    }

    #[test]
    fn kubernetes_issuer_is_classified_correctly() {
        let known = vec!["https://kubernetes.default.svc.cluster.local".to_string()];
        let token = make_token("https://kubernetes.default.svc.cluster.local");
        assert_eq!(
            classify_credential(&token, &known),
            Ok(CredentialKind::Kubernetes)
        );
    }

    #[test]
    fn anything_else_is_assumed_to_need_introspection() {
        let known = vec!["https://kubernetes.default.svc.cluster.local".to_string()];
        let token =
            make_token("http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar");
        assert_eq!(
            classify_credential(&token, &known),
            Ok(CredentialKind::LikelyIdp)
        );
    }

    #[test]
    fn malformed_credential_fails_closed_not_treated_as_idp() {
        let known = vec!["https://kubernetes.default.svc.cluster.local".to_string()];
        assert_eq!(
            classify_credential("not-a-real-token", &known),
            Err(CredentialKindError::NoIssuerClaim)
        );
    }
}
