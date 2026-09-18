/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! What a certificate's holder is permitted to communicate with --
//! RBAC-style, embedded into the certificate's own SAN set at
//! issuance time, the same way Kubernetes stores RBAC-relevant
//! information in its own client certificates. Lives here, not in
//! credentials-agent-common, because embedding permissions into a
//! certificate's SAN set is fundamentally cert-issuer's own concern;
//! credentials-agent-common depends on this crate already (the
//! reverse would be a backwards dependency).
//!
//! A closed, hard-selected scope first, before any specific target is
//! even considered -- the same "don't fall back to free text when the
//! set is genuinely enumerable" discipline already applied to
//! RevocationReason/GrantReason.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum PermissionScope {
    ServiceToService,
    ServiceToUser,
    UserToService,
    UserToUser,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub struct Permission {
    pub scope: PermissionScope,
    /// A specific FQDN this permission is scoped to (e.g.
    /// "cassini.cluster.local"), narrowing it from "any target within
    /// this scope" to one named target. Always None today -- FQDN-
    /// scoping (letting a certificate say "service-to-service,
    /// specifically with cassini") is real, deliberately deferred
    /// future work, not a rejected idea.
    pub target: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PermissionParseError {
    #[error("missing 'p:' prefix")]
    MissingPrefix,
    #[error("unknown permission scope abbreviation: {0:?}")]
    UnknownScope(String),
    #[error("invalid FQDN target {0:?}: {1}")]
    InvalidTarget(String, String),
}

impl PermissionScope {
    fn abbreviation(self) -> &'static str {
        match self {
            Self::ServiceToService => "s2s",
            Self::ServiceToUser => "s2u",
            Self::UserToService => "u2s",
            Self::UserToUser => "u2u",
        }
    }

    fn from_abbreviation(s: &str) -> Option<Self> {
        match s {
            "s2s" => Some(Self::ServiceToService),
            "s2u" => Some(Self::ServiceToUser),
            "u2s" => Some(Self::UserToService),
            "u2u" => Some(Self::UserToUser),
            _ => None,
        }
    }
}

impl Permission {
    /// The literal form embedded as a Subject Alternative Name entry
    /// on the issued certificate itself -- the same mechanism
    /// IssueRequest's own extra_sans field already uses, just with
    /// this structured type converted to a string instead of an
    /// already-plain string passed straight through. Terse by
    /// deliberate choice: `p:s2s:cassini.cluster.local`, or `p:s2s`
    /// with no target.
    pub fn to_san_string(&self) -> String {
        match &self.target {
            Some(target) => format!("p:{}:{target}", self.scope.abbreviation()),
            None => format!("p:{}", self.scope.abbreviation()),
        }
    }

    /// Parses a SAN entry back into a Permission -- the inverse of
    /// `to_san_string`. A present target is validated as a genuine,
    /// well-formed FQDN shape via the `fqdn` crate (label/name length
    /// limits, character restrictions, no leading/trailing hyphen per
    /// label -- deliberately not `strict-rfc`'s full bundle, which
    /// also requires an explicit trailing dot; an ordinary target
    /// like "cassini.cluster.local" should validate without that
    /// unusual convention). Used purely as a validation gate -- the
    /// caller's original string is what gets stored, not
    /// fqdn::FQDN's own output, avoiding any surprise normalization.
    pub fn from_san_string(s: &str) -> Result<Self, PermissionParseError> {
        let rest = s
            .strip_prefix("p:")
            .ok_or(PermissionParseError::MissingPrefix)?;
        let mut parts = rest.splitn(2, ':');
        let scope_str = parts.next().unwrap_or("");
        let scope = PermissionScope::from_abbreviation(scope_str)
            .ok_or_else(|| PermissionParseError::UnknownScope(scope_str.to_string()))?;

        let target = match parts.next() {
            Some(t) => {
                t.parse::<fqdn::FQDN>().map_err(|e| {
                    PermissionParseError::InvalidTarget(t.to_string(), e.to_string())
                })?;
                Some(t.to_string())
            }
            None => None,
        };

        Ok(Self { scope, target })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionExtractionError {
    #[error("failed to parse certificate PEM: {0}")]
    InvalidPem(String),
    #[error("failed to parse certificate DER: {0}")]
    InvalidDer(String),
    #[error("failed to parse the certificate's SubjectAlternativeName extension: {0}")]
    InvalidSanExtension(String),
}

/// Extracts every real `Permission` embedded in `certificate_pem`'s
/// own SAN set -- the real, general read-side counterpart to
/// `Permission::to_san_string`, which only ever writes these in at
/// issuance time (see cert-issuer/admin's own, real, existing use).
/// Nothing in this workspace read them back out before this.
///
/// Every real SAN entry in a certificate issued via this workspace's
/// own `generate_csr` is a DNS-name-type entry (see that function's
/// own, real implementation) -- both this agent's own identity and
/// any real, embedded permission string are indistinguishable at the
/// raw X.509 level. `Permission::from_san_string` genuinely, itself,
/// is the real filtering mechanism: every DNS-name SAN is attempted,
/// and only the ones that actually parse as a real, valid permission
/// string are kept -- everything else (this agent's own identity,
/// any other, ordinary extra SAN) is silently, correctly skipped,
/// not treated as an error.
///
/// General, real infrastructure -- not specific to any one, real
/// caller. Any agent that needs a certificate holder's own, real,
/// embedded permissions (Credentials Agent's own, upcoming policy-
/// authorization work is the first, real caller, but deliberately
/// not the only one this was built for) can call this directly.
pub fn extract_permissions(
    certificate_pem: &str,
) -> Result<Vec<Permission>, PermissionExtractionError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes())
        .map_err(|e| PermissionExtractionError::InvalidPem(e.to_string()))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents)
        .map_err(|e| PermissionExtractionError::InvalidDer(e.to_string()))?;

    let Some(san_extension) = cert
        .tbs_certificate
        .subject_alternative_name()
        .map_err(|e| PermissionExtractionError::InvalidSanExtension(e.to_string()))?
    else {
        // No SAN extension at all -- genuinely, correctly no
        // permissions, not an error.
        return Ok(Vec::new());
    };

    Ok(san_extension
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            x509_parser::extensions::GeneralName::DNSName(s) => Permission::from_san_string(s).ok(),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_round_trips_through_serde_json() {
        let original = Permission {
            scope: PermissionScope::ServiceToService,
            target: Some("cassini".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: Permission = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn permission_with_no_target_round_trips_through_serde_json() {
        let original = Permission {
            scope: PermissionScope::UserToService,
            target: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: Permission = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn permission_round_trips_through_rkyv() {
        let original = Permission {
            scope: PermissionScope::UserToUser,
            target: None,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: Permission =
            rkyv::from_bytes::<Permission, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn every_scope_round_trips_through_serde_json() {
        for scope in [
            PermissionScope::ServiceToService,
            PermissionScope::ServiceToUser,
            PermissionScope::UserToService,
            PermissionScope::UserToUser,
        ] {
            let original = Permission {
                scope,
                target: None,
            };
            let json = serde_json::to_string(&original).unwrap();
            let restored: Permission = serde_json::from_str(&json).unwrap();
            assert_eq!(original, restored);
        }
    }

    #[test]
    fn to_san_string_with_a_target_matches_the_documented_terse_format() {
        let permission = Permission {
            scope: PermissionScope::ServiceToService,
            target: Some("cassini.cluster.local".to_string()),
        };
        assert_eq!(permission.to_san_string(), "p:s2s:cassini.cluster.local");
    }

    #[test]
    fn to_san_string_with_no_target_omits_the_trailing_colon() {
        let permission = Permission {
            scope: PermissionScope::UserToUser,
            target: None,
        };
        assert_eq!(permission.to_san_string(), "p:u2u");
    }

    #[test]
    fn every_scope_round_trips_through_san_string_with_a_target() {
        for scope in [
            PermissionScope::ServiceToService,
            PermissionScope::ServiceToUser,
            PermissionScope::UserToService,
            PermissionScope::UserToUser,
        ] {
            let original = Permission {
                scope,
                target: Some("cassini.cluster.local".to_string()),
            };
            let san = original.to_san_string();
            let restored = Permission::from_san_string(&san).unwrap();
            assert_eq!(original, restored);
        }
    }

    #[test]
    fn every_scope_round_trips_through_san_string_with_no_target() {
        for scope in [
            PermissionScope::ServiceToService,
            PermissionScope::ServiceToUser,
            PermissionScope::UserToService,
            PermissionScope::UserToUser,
        ] {
            let original = Permission {
                scope,
                target: None,
            };
            let san = original.to_san_string();
            let restored = Permission::from_san_string(&san).unwrap();
            assert_eq!(original, restored);
        }
    }

    #[test]
    fn from_san_string_rejects_a_missing_prefix() {
        let result = Permission::from_san_string("s2s:cassini.cluster.local");
        assert_eq!(result, Err(PermissionParseError::MissingPrefix));
    }

    #[test]
    fn from_san_string_rejects_an_unknown_scope_abbreviation() {
        let result = Permission::from_san_string("p:x9x");
        assert_eq!(
            result,
            Err(PermissionParseError::UnknownScope("x9x".to_string()))
        );
    }

    #[test]
    fn from_san_string_rejects_a_target_with_an_empty_label() {
        let result = Permission::from_san_string("p:s2s:cassini..local");
        assert!(matches!(
            result,
            Err(PermissionParseError::InvalidTarget(_, _))
        ));
    }

    #[test]
    fn from_san_string_rejects_a_target_with_invalid_characters() {
        let result = Permission::from_san_string("p:s2s:cassini_cluster*local");
        assert!(matches!(
            result,
            Err(PermissionParseError::InvalidTarget(_, _))
        ));
    }

    #[test]
    fn from_san_string_accepts_an_ordinary_target_with_no_trailing_dot() {
        // The whole reason strict-rfc's bundled trailing-dot
        // requirement was deliberately excluded -- confirms an
        // ordinary-looking FQDN actually validates.
        let result = Permission::from_san_string("p:s2s:cassini.cluster.local");
        assert!(result.is_ok());
    }

    fn build_test_certificate(sans: Vec<String>) -> String {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let private_key_pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params = rcgen::CertificateParams::new(sans).unwrap();
        params.self_signed(&key_pair).unwrap().pem()
    }

    #[test]
    fn extract_permissions_finds_every_real_embedded_permission() {
        let permission_a = Permission {
            scope: PermissionScope::ServiceToService,
            target: None,
        };
        let permission_b = Permission {
            scope: PermissionScope::UserToUser,
            target: Some("cassini.cluster.local".to_string()),
        };

        let certificate_pem = build_test_certificate(vec![
            "test.polar.svc.cluster.local".to_string(),
            permission_a.to_san_string(),
            permission_b.to_san_string(),
        ]);

        let extracted = extract_permissions(&certificate_pem).unwrap();

        assert_eq!(extracted.len(), 2, "the real identity SAN must not be mistaken for a permission");
        assert!(extracted.contains(&permission_a));
        assert!(extracted.contains(&permission_b));
    }

    #[test]
    fn extract_permissions_returns_empty_when_no_permissions_are_embedded() {
        let certificate_pem =
            build_test_certificate(vec!["test.polar.svc.cluster.local".to_string()]);

        let extracted = extract_permissions(&certificate_pem).unwrap();

        assert!(extracted.is_empty());
    }

    #[test]
    fn extract_permissions_on_malformed_pem_is_a_real_error_not_a_panic() {
        let result = extract_permissions("not a real certificate");
        assert!(matches!(result, Err(PermissionExtractionError::InvalidPem(_))));
    }
}
