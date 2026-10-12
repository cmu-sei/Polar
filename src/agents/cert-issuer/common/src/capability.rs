//! What a certificate's holder may *do to the Credentials Agent itself*
//! -- administrative capabilities, embedded in the certificate's SAN
//! set as `c:<name>`, alongside (and never confused with) the `p:`
//! communication permissions in permission.rs. Modeled on Linux
//! capabilities: a closed set of discrete privileges, each checked by
//! the one operation that needs it, with no single "admin" flag.
//!
//! The Credentials Agent's own ledger is the authority for who holds
//! which capability; the SAN copy exists for external verifiers that
//! cannot see the ledger.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Deserialize,
    Serialize,
    Archive,
    SerdeDeserialize,
    SerdeSerialize,
)]
pub enum Capability {
    /// Provision a new Polar account (with any permissions and capabilities).
    ProvisionAccount,
    /// Grant permissions to an existing identity.
    Grant,
    /// Revoke a certificate.
    Revoke,
    /// Submit a policy update.
    UpdatePolicy,
    /// Run a signed command on an agent (docs/architecture/commands.md).
    RunCommand,
    /// Change the configuration of agents, by a signed configuration document
    /// (docs/architecture/signed-configuration.md). Not needed to change Cedar policy: that is
    /// `UpdatePolicy`, and a repository holds both only if it holds both kinds of content. Appended, so that
    /// what was archived with the earlier ones keeps its meaning.
    UpdateConfiguration,
    /// Change what an agent works on, by a signed runtime document (docs/architecture/git-controller.md):
    /// which repositories it observes, under what admission rules. Not `UpdateConfiguration`, which changes
    /// how the agent runs, and not `UpdatePolicy`. Appended, so that what was archived with the earlier ones
    /// keeps its meaning; a consumer built before it rejects a certificate that carries it as malformed, so
    /// the consumers are redeployed before any certificate does.
    UpdateRuntime,
    /// Change who is in which group, by a signed membership document
    /// (docs/architecture/authority-distribution.md). Not `Grant`, which gives an identity permissions, and
    /// not `UpdatePolicy`, which writes the rules that refer to groups. Appended; consumers are redeployed
    /// before any certificate carries it.
    UpdateGroups,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapabilityParseError {
    #[error("missing 'c:' prefix")]
    MissingPrefix,
    #[error("unknown capability: {0:?}")]
    Unknown(String),
}

impl Capability {
    pub const ALL: [Capability; 8] = [
        Capability::ProvisionAccount,
        Capability::Grant,
        Capability::Revoke,
        Capability::UpdatePolicy,
        Capability::RunCommand,
        Capability::UpdateConfiguration,
        Capability::UpdateRuntime,
        Capability::UpdateGroups,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::ProvisionAccount => "provision-account",
            Self::Grant => "grant",
            Self::Revoke => "revoke",
            Self::UpdatePolicy => "update-policy",
            Self::RunCommand => "run-command",
            Self::UpdateConfiguration => "update-configuration",
            Self::UpdateRuntime => "update-runtime",
            Self::UpdateGroups => "update-groups",
        }
    }

    /// The literal SAN entry: `c:provision-account`.
    pub fn to_san_string(&self) -> String {
        format!("c:{}", self.name())
    }

    /// The inverse of `to_san_string`. Also usable as a clap `value_parser`.
    pub fn from_san_string(s: &str) -> Result<Self, CapabilityParseError> {
        let name = s
            .strip_prefix("c:")
            .ok_or(CapabilityParseError::MissingPrefix)?;
        Self::ALL
            .into_iter()
            .find(|capability| capability.name() == name)
            .ok_or_else(|| CapabilityParseError::Unknown(name.to_string()))
    }
}

/// Every capability embedded in `certificate_pem`'s SAN set. Entries
/// that aren't `c:` capabilities (the identity, `p:` permissions) are
/// silently skipped, the same filtering discipline as
/// `extract_permissions`.
pub fn extract_capabilities(
    certificate_pem: &str,
) -> Result<Vec<Capability>, crate::permission::PermissionExtractionError> {
    Ok(crate::permission::san_strings(certificate_pem)?
        .iter()
        .filter_map(|s| Capability::from_san_string(s).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionExtractionError;
    use crate::permission::{Permission, PermissionScope, extract_permissions};

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
    fn san_string_matches_the_documented_format() {
        assert_eq!(
            Capability::ProvisionAccount.to_san_string(),
            "c:provision-account"
        );
        assert_eq!(Capability::UpdatePolicy.to_san_string(), "c:update-policy");
    }

    #[test]
    fn run_command_is_the_last_capability_and_has_its_own_san_string() {
        assert_eq!(Capability::RunCommand.to_san_string(), "c:run-command");
        assert_eq!(
            Capability::from_san_string("c:run-command"),
            Ok(Capability::RunCommand)
        );
        // Appended, never inserted: the four before it keep their places.
        assert_eq!(Capability::ALL[4], Capability::RunCommand);
        assert_eq!(Capability::ALL[3], Capability::UpdatePolicy);
        // A close relative is not it.
        assert!(Capability::from_san_string("c:run").is_err());
        assert!(Capability::from_san_string("c:run-commands").is_err());
    }

    #[test]
    fn update_configuration_is_its_own_capability_and_comes_last() {
        assert_eq!(
            Capability::UpdateConfiguration.to_san_string(),
            "c:update-configuration"
        );
        assert_eq!(
            Capability::from_san_string("c:update-configuration"),
            Ok(Capability::UpdateConfiguration)
        );
        assert_eq!(Capability::ALL[5], Capability::UpdateConfiguration);
        // Appended after run-command, which keeps its place, as the policy capability does.
        assert_eq!(Capability::ALL[4], Capability::RunCommand);
        assert_eq!(Capability::ALL[3], Capability::UpdatePolicy);
        assert_ne!(Capability::UpdateConfiguration, Capability::UpdatePolicy);
        assert!(Capability::from_san_string("c:update-config").is_err());
    }

    #[test]
    fn update_runtime_is_its_own_capability_and_comes_last() {
        assert_eq!(
            Capability::UpdateRuntime.to_san_string(),
            "c:update-runtime"
        );
        assert_eq!(
            Capability::from_san_string("c:update-runtime"),
            Ok(Capability::UpdateRuntime)
        );
        // Appended after update-configuration, which keeps its place: what was archived with the
        // earlier ones keeps its meaning.
        assert_eq!(Capability::ALL[6], Capability::UpdateRuntime);
        assert_eq!(Capability::ALL[5], Capability::UpdateConfiguration);
        // Changing what an agent works on is not changing how it runs, and not changing policy.
        assert_ne!(Capability::UpdateRuntime, Capability::UpdateConfiguration);
        assert_ne!(Capability::UpdateRuntime, Capability::UpdatePolicy);
        assert!(Capability::from_san_string("c:update-runtimes").is_err());
    }

    #[test]
    fn update_groups_is_its_own_capability_and_comes_last() {
        assert_eq!(
            Capability::UpdateGroups.to_san_string(),
            "c:update-groups"
        );
        assert_eq!(
            Capability::from_san_string("c:update-groups"),
            Ok(Capability::UpdateGroups)
        );
        // Appended after update-runtime, which keeps its place.
        assert_eq!(Capability::ALL[7], Capability::UpdateGroups);
        assert_eq!(Capability::ALL[6], Capability::UpdateRuntime);
        assert_eq!(Capability::ALL.len(), 8);
        // Putting someone in a group is not granting them permissions, and not writing the rules about
        // groups: three authorities.
        assert_ne!(Capability::UpdateGroups, Capability::Grant);
        assert_ne!(Capability::UpdateGroups, Capability::UpdatePolicy);
        assert!(Capability::from_san_string("c:update-group").is_err());
    }

    #[test]
    fn every_capability_round_trips_through_its_san_string() {
        for capability in Capability::ALL {
            let restored = Capability::from_san_string(&capability.to_san_string()).unwrap();
            assert_eq!(restored, capability);
        }
    }

    #[test]
    fn every_capability_round_trips_through_serde_json_and_rkyv() {
        for capability in Capability::ALL {
            let json = serde_json::to_string(&capability).unwrap();
            assert_eq!(
                serde_json::from_str::<Capability>(&json).unwrap(),
                capability
            );

            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&capability)
                .unwrap()
                .to_vec();
            let restored = rkyv::from_bytes::<Capability, rkyv::rancor::Error>(&bytes).unwrap();
            assert_eq!(restored, capability);
        }
    }

    #[test]
    fn from_san_string_rejects_a_missing_prefix() {
        assert_eq!(
            Capability::from_san_string("grant"),
            Err(CapabilityParseError::MissingPrefix)
        );
    }

    #[test]
    fn from_san_string_rejects_an_unknown_capability() {
        assert_eq!(
            Capability::from_san_string("c:root"),
            Err(CapabilityParseError::Unknown("root".to_string()))
        );
    }

    #[test]
    fn a_permission_string_is_not_a_capability() {
        assert!(Capability::from_san_string("p:s2s").is_err());
    }

    #[test]
    fn extract_capabilities_finds_only_capabilities_and_extract_permissions_ignores_them() {
        let permission = Permission {
            scope: PermissionScope::UserToService,
            target: None,
        };
        let certificate_pem = build_test_certificate(vec![
            "dana.polar.svc.cluster.local".to_string(),
            permission.to_san_string(),
            Capability::Grant.to_san_string(),
            Capability::Revoke.to_san_string(),
        ]);

        let capabilities = extract_capabilities(&certificate_pem).unwrap();
        assert_eq!(capabilities.len(), 2);
        assert!(capabilities.contains(&Capability::Grant));
        assert!(capabilities.contains(&Capability::Revoke));

        let permissions = extract_permissions(&certificate_pem).unwrap();
        assert_eq!(permissions, vec![permission]);
    }

    #[test]
    fn extract_capabilities_returns_empty_when_none_are_embedded() {
        let certificate_pem =
            build_test_certificate(vec!["dana.polar.svc.cluster.local".to_string()]);
        assert!(extract_capabilities(&certificate_pem).unwrap().is_empty());
    }

    #[test]
    fn extract_capabilities_on_malformed_pem_is_an_error_not_a_panic() {
        assert!(matches!(
            extract_capabilities("not a real certificate"),
            Err(PermissionExtractionError::InvalidPem(_))
        ));
    }
}
