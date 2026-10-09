//! The capability vocabulary an agent's code can require, shared with the
//! deployment catalog.
//!
//! The names here must equal the keys of `infra/config/capabilities.nuon`
//! (checked in both directions by `names_match_the_deployment_catalog`), so
//! one declaration drives both sides: the catalog says what a deployment
//! provides, and this enum says what the code requires. The deployment
//! publishes what it provides in `POLAR_CAPABILITIES`; `AgentBuilder::build()`
//! compares the two and refuses to start on a mismatch, with a message that
//! names the missing capability.

use std::fmt;

/// Environment variable the deployment sets to the comma-separated list of
/// capabilities it provides.
pub const PROVIDED_ENV: &str = crate::environment::CAPABILITIES.name();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// A Cassini client with mTLS. Every agent built by `AgentBuilder`.
    Cassini,
    /// Writes to the graph (Neo4j) over mTLS.
    GraphWrite,
    /// Encrypts payloads through CryptoActor. Opt-in.
    Encryption,
    /// Reads the Kubernetes API with the workload's own ServiceAccount token.
    KubeApiRead,
}

impl Capability {
    /// Every capability, in the order the catalog lists them.
    pub const ALL: [Capability; 4] = [
        Capability::Cassini,
        Capability::GraphWrite,
        Capability::Encryption,
        Capability::KubeApiRead,
    ];

    /// The capability a name in `POLAR_CAPABILITIES` stands for, if this code knows it.
    pub fn from_name(name: &str) -> Option<Capability> {
        Capability::ALL.into_iter().find(|c| c.as_str() == name)
    }

    /// The known capabilities in a comma-separated `POLAR_CAPABILITIES` value. Names this
    /// code does not know are ignored, as in [`check`].
    pub fn parse_list(raw: &str) -> Vec<Capability> {
        raw.split(',')
            .map(str::trim)
            .filter_map(Capability::from_name)
            .collect()
    }

    /// The name used in `capabilities.nuon` and in `POLAR_CAPABILITIES`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::Cassini => "cassini",
            Capability::GraphWrite => "graph-write",
            Capability::Encryption => "encryption",
            Capability::KubeApiRead => "kube-api-read",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The outcome of comparing what the code requires with what the deployment
/// says it provides.
#[derive(Debug, PartialEq, Eq)]
pub enum CapabilityCheck {
    /// Every required capability is provided.
    Satisfied,
    /// The deployment did not say what it provides (`POLAR_CAPABILITIES` is
    /// unset), for example a run outside the catalog. Nothing can be checked.
    NotDeclared,
    /// These required capabilities are not provided.
    Missing(Vec<Capability>),
}

/// Compares `required` with the deployment's comma-separated `provided`
/// list. Names in `provided` this code does not know are ignored: a
/// deployment may provide more than one agent needs.
pub fn check(required: &[Capability], provided: Option<&str>) -> CapabilityCheck {
    let Some(provided) = provided else {
        return CapabilityCheck::NotDeclared;
    };
    let provided: Vec<&str> = provided
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let mut missing = Vec::new();
    for c in required {
        if !provided.contains(&c.as_str()) && !missing.contains(c) {
            missing.push(*c);
        }
    }

    if missing.is_empty() {
        CapabilityCheck::Satisfied
    } else {
        CapabilityCheck::Missing(missing)
    }
}

/// The message `build()` fails with when capabilities are missing.
pub fn missing_message(missing: &[Capability], provided: Option<&str>) -> String {
    let names: Vec<&str> = missing.iter().map(|c| c.as_str()).collect();
    format!(
        "this agent requires capabilit{} {} but the deployment provides: [{}] \
         ({PROVIDED_ENV}). Add the missing capabilit{} to the workload's `needs` in \
         infra/config/workloads, or stop requiring {} in the agent.",
        if names.len() == 1 { "y" } else { "ies" },
        names.join(", "),
        provided.unwrap_or(""),
        if names.len() == 1 { "y" } else { "ies" },
        if names.len() == 1 { "it" } else { "them" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn names_map_to_capabilities_and_unknown_ones_are_ignored() {
        assert_eq!(
            Capability::parse_list(" cassini, graph-write ,nonsense,,"),
            [Capability::Cassini, Capability::GraphWrite]
        );
        assert_eq!(
            Capability::from_name("encryption"),
            Some(Capability::Encryption)
        );
        assert_eq!(Capability::from_name("Cassini"), None);
    }

    #[test]
    fn satisfied_when_every_requirement_is_provided() {
        let r = check(
            &[Capability::Cassini, Capability::GraphWrite],
            Some("cassini,graph-write,encryption"),
        );
        assert_eq!(r, CapabilityCheck::Satisfied);
    }

    #[test]
    fn a_deployment_may_provide_more_than_the_code_requires() {
        let r = check(
            &[Capability::Cassini],
            Some("cassini,graph-write,future-thing"),
        );
        assert_eq!(r, CapabilityCheck::Satisfied);
    }

    #[test]
    fn missing_lists_exactly_what_is_not_provided() {
        let r = check(
            &[
                Capability::Cassini,
                Capability::GraphWrite,
                Capability::Encryption,
            ],
            Some("cassini"),
        );
        assert_eq!(
            r,
            CapabilityCheck::Missing(vec![Capability::GraphWrite, Capability::Encryption])
        );
    }

    #[test]
    fn whitespace_and_empty_entries_are_tolerated() {
        let r = check(&[Capability::Cassini], Some(" cassini , ,graph-write "));
        assert_eq!(r, CapabilityCheck::Satisfied);
    }

    #[test]
    fn an_empty_list_provides_nothing() {
        let r = check(&[Capability::Cassini], Some(""));
        assert_eq!(r, CapabilityCheck::Missing(vec![Capability::Cassini]));
    }

    #[test]
    fn an_undeclared_deployment_cannot_be_checked() {
        assert_eq!(
            check(&[Capability::Cassini], None),
            CapabilityCheck::NotDeclared
        );
    }

    #[test]
    fn duplicate_requirements_are_reported_once() {
        let r = check(
            &[Capability::Encryption, Capability::Encryption],
            Some("cassini"),
        );
        assert_eq!(r, CapabilityCheck::Missing(vec![Capability::Encryption]));
    }

    #[test]
    fn the_message_names_the_missing_capability() {
        let m = missing_message(&[Capability::GraphWrite], Some("cassini"));
        assert!(m.contains("graph-write"), "{m}");
        assert!(m.contains("cassini"), "{m}");
        assert!(m.contains(PROVIDED_ENV), "{m}");
    }

    /// Top-level keys of `infra/config/capabilities.nuon`: lines indented by
    /// exactly two spaces that read `name: {`.
    fn catalog_keys(nuon: &str) -> Vec<String> {
        nuon.lines()
            .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
            .filter_map(|l| l.trim_end().strip_suffix(": {"))
            .map(|k| k.trim().to_string())
            .filter(|k| {
                !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            })
            .collect()
    }

    /// Capabilities the catalog gives a deployment that no code ever requires: the shape of the
    /// pod (a persistent volume, a pinned CA, an init container that skips the escrow bootstrap),
    /// not something the agent's own health depends on. They are in `POLAR_CAPABILITIES`, which
    /// ignores names the code does not know.
    const DEPLOYMENT_ONLY: &[&str] = &["cassini-server", "escrow-exempt", "persistent-data", "pinned-ca"];

    #[test]
    fn names_match_the_deployment_catalog() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/config/capabilities.nuon");
        // Built from a source tree that does not include infra/ (a filtered
        // nix source, a published crate): nothing to compare against.
        let Ok(nuon) = std::fs::read_to_string(&path) else {
            eprintln!("skipping: {} not found", path.display());
            return;
        };

        let mut in_catalog = catalog_keys(&nuon);
        for name in DEPLOYMENT_ONLY {
            assert!(
                in_catalog.iter().any(|k| k == name),
                "{name} is listed as deployment-only but the catalog has no such capability"
            );
        }
        in_catalog.retain(|k| !DEPLOYMENT_ONLY.contains(&k.as_str()));
        let mut in_code: Vec<String> = Capability::ALL
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        in_catalog.sort();
        in_code.sort();

        assert_eq!(
            in_code, in_catalog,
            "polar_agent_builder::Capability and infra/config/capabilities.nuon must list the \
             same capabilities: add the missing one on the side that lacks it"
        );
    }

    #[test]
    fn the_key_parser_ignores_nested_and_commented_lines() {
        let nuon = "# header\n{\n  # a comment: {\n  alpha: {\n    pod: {\n    }\n  }\n  beta-two: {\n  }\n}\n";
        assert_eq!(
            catalog_keys(nuon),
            vec!["alpha".to_string(), "beta-two".to_string()]
        );
    }
}
