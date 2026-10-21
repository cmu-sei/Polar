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
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    Archive,
    SerdeDeserialize,
    SerdeSerialize,
)]
pub enum PermissionScope {
    ServiceToService,
    ServiceToUser,
    UserToService,
    UserToUser,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
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
    #[error("{0:?} is not a topic a certificate can carry")]
    InvalidTopic(String),
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

/// Whether a certificate's holder may publish or subscribe to a topic.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, SerdeDeserialize, SerdeSerialize,
)]
pub enum TopicAccess {
    Publish,
    Subscribe,
}

/// A topic the catalog says a workload uses, carried in its certificate as `p:pub:<topic>` or `p:sub:<topic>`
/// (docs/architecture/policy-enforcement.md, stage 3): the floor the broker decides on where no policy speaks. Only
/// declared topics are written; the implied ones follow from the identity and its groups.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, SerdeDeserialize, SerdeSerialize)]
pub struct TopicPermission {
    pub access: TopicAccess,
    pub topic: String,
}

/// Whether `topic` can be carried in a certificate: letters, digits, `.`, `_`, `:` and `-`, and not empty. That is
/// every topic Polar uses, and it keeps the name a valid URI, which every TLS stack that reads a certificate's URI
/// names (some strictly) accepts.
pub fn is_carried_topic(topic: &str) -> bool {
    !topic.is_empty()
        && topic
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

impl TopicPermission {
    /// `p:pub:<topic>` or `p:sub:<topic>`.
    pub fn to_san_string(&self) -> String {
        let access = match self.access {
            TopicAccess::Publish => "pub",
            TopicAccess::Subscribe => "sub",
        };
        format!("p:{access}:{}", self.topic)
    }

    /// The inverse of [`to_san_string`](Self::to_san_string). A topic that cannot be carried is refused.
    pub fn from_san_string(s: &str) -> Result<Self, PermissionParseError> {
        let rest = s
            .strip_prefix("p:")
            .ok_or(PermissionParseError::MissingPrefix)?;
        let (access, topic) = rest
            .split_once(':')
            .ok_or_else(|| PermissionParseError::UnknownScope(rest.to_string()))?;
        let access = match access {
            "pub" => TopicAccess::Publish,
            "sub" => TopicAccess::Subscribe,
            other => return Err(PermissionParseError::UnknownScope(other.to_string())),
        };
        if !is_carried_topic(topic) {
            return Err(PermissionParseError::InvalidTopic(topic.to_string()));
        }
        Ok(Self {
            access,
            topic: topic.to_string(),
        })
    }
}

/// Every topic permission a certificate carries; anything else in its names is skipped.
pub fn extract_topic_permissions(
    certificate_pem: &str,
) -> Result<Vec<TopicPermission>, PermissionExtractionError> {
    Ok(san_strings(certificate_pem)?
        .iter()
        .filter_map(|s| TopicPermission::from_san_string(s).ok())
        .collect())
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
    Ok(san_strings(certificate_pem)?
        .iter()
        .filter_map(|s| Permission::from_san_string(s).ok())
        .collect())
}

/// Every DNS-name and URI SAN value in the certificate, in order. cert-issuer
/// issues `p:` and `c:` entries as URI SANs (see ca.rs); DNS names are read
/// too so certificates issued before that change still resolve. Anything
/// else (IP, email) is skipped. A certificate with no SAN extension yields
/// an empty list, not an error.
pub(crate) fn san_strings(certificate_pem: &str) -> Result<Vec<String>, PermissionExtractionError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes())
        .map_err(|e| PermissionExtractionError::InvalidPem(e.to_string()))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents)
        .map_err(|e| PermissionExtractionError::InvalidDer(e.to_string()))?;

    let Some(san_extension) = cert
        .tbs_certificate
        .subject_alternative_name()
        .map_err(|e| PermissionExtractionError::InvalidSanExtension(e.to_string()))?
    else {
        return Ok(Vec::new());
    };

    Ok(san_extension
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            x509_parser::extensions::GeneralName::DNSName(s)
            | x509_parser::extensions::GeneralName::URI(s) => Some(s.to_string()),
            _ => None,
        })
        .collect())
}

/// Extracts the certificate holder's own, real workload identity
/// from `certificate_pem`'s own SAN set -- the real, general
/// read-side counterpart to `generate_csr`'s own, documented
/// guarantee that the identity SAN "is always first and always
/// present," with every other, real, extra SAN entry (permissions
/// included) purely additive after it. The one, real difference
/// from `extract_permissions`, above: this takes the first, real
/// DNS-name SAN entry unconditionally, rather than filtering for
/// ones that parse as a `Permission` -- the identity SAN is
/// deliberately, genuinely never itself a `p:`-prefixed permission
/// string, so the two extractions never collide.
///
/// General, real infrastructure -- not specific to any one, real
/// caller. Any agent whose own, long-running process needs to learn
/// its own workload identity from its own, already-mounted mTLS
/// client certificate (kube-consumer's own, upcoming CryptoActor
/// wiring is the first, real caller) can call this directly, rather
/// than each caller needing its own, separate, real source for the
/// same, real identity string `polar-init` already, separately
/// computed once, at issuance time.
pub fn extract_own_identity(certificate_pem: &str) -> Result<String, PermissionExtractionError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes())
        .map_err(|e| PermissionExtractionError::InvalidPem(e.to_string()))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents)
        .map_err(|e| PermissionExtractionError::InvalidDer(e.to_string()))?;

    let Some(san_extension) = cert
        .tbs_certificate
        .subject_alternative_name()
        .map_err(|e| PermissionExtractionError::InvalidSanExtension(e.to_string()))?
    else {
        return Err(PermissionExtractionError::InvalidSanExtension(
            "certificate carries no SAN extension at all -- no identity to extract".to_string(),
        ));
    };

    san_extension
        .value
        .general_names
        .iter()
        .find_map(|name| match name {
            x509_parser::extensions::GeneralName::DNSName(s) => Some(s.to_string()),
            _ => None,
        })
        .ok_or_else(|| {
            PermissionExtractionError::InvalidSanExtension(
                "certificate's SAN set carries no DNS-name entry at all -- no identity to extract"
                    .to_string(),
            )
        })
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

        assert_eq!(
            extracted.len(),
            2,
            "the real identity SAN must not be mistaken for a permission"
        );
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
        assert!(matches!(
            result,
            Err(PermissionExtractionError::InvalidPem(_))
        ));
    }

    #[test]
    fn extract_own_identity_returns_the_real_identity_san_with_no_permissions_present() {
        let certificate_pem =
            build_test_certificate(vec!["kube-consumer.polar.svc.cluster.local".to_string()]);

        let identity = extract_own_identity(&certificate_pem).unwrap();

        assert_eq!(identity, "kube-consumer.polar.svc.cluster.local");
    }

    #[test]
    fn extract_own_identity_returns_the_first_san_even_when_real_permissions_follow_it() {
        let permission = Permission {
            scope: PermissionScope::ServiceToService,
            target: None,
        };
        let certificate_pem = build_test_certificate(vec![
            "kube-consumer.polar.svc.cluster.local".to_string(),
            permission.to_san_string(),
        ]);

        let identity = extract_own_identity(&certificate_pem).unwrap();

        assert_eq!(identity, "kube-consumer.polar.svc.cluster.local");
    }

    #[test]
    fn extract_own_identity_on_malformed_pem_is_a_real_error_not_a_panic() {
        let result = extract_own_identity("not a real certificate");
        assert!(matches!(
            result,
            Err(PermissionExtractionError::InvalidPem(_))
        ));
    }
}

#[cfg(test)]
mod uri_san_tests {
    use super::*;
    use crate::capability::{Capability, extract_capabilities};

    /// A certificate laid out the way cert-issuer's ca.rs issues one:
    /// identity as a DNS SAN, `p:` and `c:` entries as URI SANs.
    fn issued_like_cert_issuer() -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["acct-1".to_string()]).unwrap();
        params
            .subject_alt_names
            .push(rcgen::SanType::URI("p:u2s".to_string().try_into().unwrap()));
        params.subject_alt_names.push(rcgen::SanType::URI(
            Capability::ProvisionAccount
                .to_san_string()
                .try_into()
                .unwrap(),
        ));
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn permissions_are_read_from_uri_sans() {
        let permissions = extract_permissions(&issued_like_cert_issuer()).unwrap();
        assert_eq!(
            permissions,
            vec![Permission::from_san_string("p:u2s").unwrap()]
        );
    }

    #[test]
    fn capabilities_are_read_from_uri_sans_and_not_confused_with_permissions() {
        let capabilities = extract_capabilities(&issued_like_cert_issuer()).unwrap();
        assert_eq!(capabilities, vec![Capability::ProvisionAccount]);
    }

    /// A workload's certificate as stage 3 issues one: its identity, its declared topics and its groups.
    fn issued_for_a_workload() -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![
            "kube-consumer.polar.serviceaccount.cluster.local".to_string(),
        ])
        .unwrap();
        for san in [
            "p:sub:kubernetes.cluster.consumer.Pod",
            "p:pub:polar.provenance.discovery",
            "g:cassini-client",
            "g:encryption",
        ] {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI(san.to_string().try_into().unwrap()));
        }
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn a_workload_s_topics_and_groups_are_read_from_uri_sans_and_nothing_else_is_taken_for_them() {
        let pem = issued_for_a_workload();
        let topics = extract_topic_permissions(&pem).unwrap();
        assert_eq!(
            topics,
            vec![
                TopicPermission::from_san_string("p:sub:kubernetes.cluster.consumer.Pod").unwrap(),
                TopicPermission::from_san_string("p:pub:polar.provenance.discovery").unwrap(),
            ]
        );
        let groups = crate::group::extract_groups(&pem).unwrap();
        assert_eq!(
            groups,
            vec![
                crate::group::Group("cassini-client".into()),
                crate::group::Group("encryption".into())
            ]
        );
        assert!(extract_permissions(&pem).unwrap().is_empty());
        assert!(extract_capabilities(&pem).unwrap().is_empty());
        assert_eq!(
            extract_own_identity(&pem).unwrap(),
            "kube-consumer.polar.serviceaccount.cluster.local"
        );
        // And the topics and groups are not in an account's certificate.
        assert!(
            extract_topic_permissions(&issued_like_cert_issuer())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_identity_is_still_the_dns_san() {
        assert_eq!(
            extract_own_identity(&issued_like_cert_issuer()).unwrap(),
            "acct-1"
        );
    }

    mod topics {
        use super::super::*;
        use proptest::prelude::*;

        #[test]
        fn a_topic_permission_is_written_in_the_documented_form() {
            let publish = TopicPermission {
                access: TopicAccess::Publish,
                topic: "kubernetes.cluster.consumer.Pod".into(),
            };
            assert_eq!(
                publish.to_san_string(),
                "p:pub:kubernetes.cluster.consumer.Pod"
            );
            // A topic with colons in it keeps them.
            let subscribe =
                TopicPermission::from_san_string("p:sub:jira:consumer:projects").unwrap();
            assert_eq!(subscribe.access, TopicAccess::Subscribe);
            assert_eq!(subscribe.topic, "jira:consumer:projects");
        }

        #[test]
        fn what_is_not_a_topic_permission_is_refused() {
            for bad in [
                "pub:x",
                "p:pub",
                "p:pub:",
                "p:put:x",
                "p:s2s",
                "p:sub:two words",
                "p:sub:{workload}",
                "p:pub:caf\u{e9}",
            ] {
                assert!(TopicPermission::from_san_string(bad).is_err(), "{bad}");
            }
            // And a communication permission is not taken for one, nor the other way round.
            assert!(TopicPermission::from_san_string("p:s2s:cassini.cluster.local").is_err());
            assert!(Permission::from_san_string("p:pub:x").is_err());
        }

        fn topic() -> impl Strategy<Value = String> {
            "[A-Za-z0-9._:-]{1,40}"
        }

        proptest! {
            /// Every topic a certificate can carry reads back as written, either way.
            #[test]
            fn a_topic_permission_reads_back_as_written(topic in topic(), publish in any::<bool>()) {
                let written = TopicPermission {
                    access: if publish { TopicAccess::Publish } else { TopicAccess::Subscribe },
                    topic,
                };
                prop_assert_eq!(TopicPermission::from_san_string(&written.to_san_string()).unwrap(), written);
            }

            /// A topic with any character outside the carried set is refused, wherever the character is.
            #[test]
            fn a_topic_with_a_character_it_cannot_carry_is_refused(
                before in "[a-z.]{0,10}",
                bad in "[^A-Za-z0-9._:-]",
                after in "[a-z.]{0,10}",
            ) {
                let topic = format!("{before}{bad}{after}");
                prop_assert!(!is_carried_topic(&topic));
                let san = format!("p:pub:{topic}");
                prop_assert!(TopicPermission::from_san_string(&san).is_err());
            }
        }
    }
}
