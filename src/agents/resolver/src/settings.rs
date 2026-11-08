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

//! What the OCI resolver reads from its configuration, declared once.
//!
//! `DOCKER_CONFIG` was read from the process environment by the `docker_credential` crate itself,
//! so it was not declared or checked before the agent started, and could not reach an agent whose
//! environment is a pointer to a configuration document. Declared here it is in its scaffold, and
//! the resolver is handed the value. The CA a private registry or a TLS-intercepting proxy needs
//! trusted is the egress section's (`polar::egress`).

use polar_env::{Config, ConfigError, Defaulted, Described, Optional, Section};

pub static DOCKER_CONFIG: Optional<String> = Optional::text(
    "DOCKER_CONFIG",
    "the directory holding the Docker credential file config.json that registry credentials are \
     looked up in; when unset, the usual places (the home directory's .docker) are tried, and \
     without credentials the registry is asked anonymously",
);

pub static HOME: Optional<String> = Optional::text(
    "HOME",
    "the home directory: when DOCKER_CONFIG is unset, registry credentials are looked up in its .docker",
);

/// The registry a local cluster pulls from, over http: kept as the default so a deployment that
/// says nothing behaves as the resolver always did.
pub const DEFAULT_INSECURE_REGISTRIES: &str = "registry.local-registry.svc.cluster.local:5000";

pub static INSECURE_REGISTRIES: Defaulted<Vec<String>> = Defaulted::list(
    "OCI_INSECURE_REGISTRIES",
    "registries, as host:port, that are spoken to over plain http instead of https, comma-separated: \
     a development registry. Every other registry is https only",
    DEFAULT_INSECURE_REGISTRIES,
);

static VARIABLES: &[&dyn Described] = &[&DOCKER_CONFIG, &HOME, &INSECURE_REGISTRIES];

/// The section the OCI resolver registers.
pub static SECTION: Section = Section {
    name: "oci-resolver",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct ResolverSettings {
    /// Where the credential file is looked for: `DOCKER_CONFIG`, else the home's `.docker`, as the `docker_credential` crate
    /// would. Always a directory when either is set, so that a credential file that is not there is no credentials (anonymous
    /// access), and one that is there and cannot be read is an error. Left to the crate's own lookup, which reads the
    /// environment itself, a home with no `.docker/config.json` (every pod's) was "Unable to read config", and failed every
    /// resolution.
    pub docker_config: Option<String>,
    pub insecure_registries: Vec<String>,
}

impl ResolverSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            docker_config: config
                .read(&DOCKER_CONFIG)?
                .or(config.read(&HOME)?.map(|home| format!("{home}/.docker"))),
            insecure_registries: config.read(&INSECURE_REGISTRIES)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let env: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::builder(&env).register(&SECTION).build()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn nothing_is_required() {
        let settings = ResolverSettings::read(&config(&[])).unwrap();
        assert_eq!(settings.docker_config, None);
        assert_eq!(
            settings.insecure_registries,
            vec![DEFAULT_INSECURE_REGISTRIES.to_string()]
        );
        assert!(config(&[]).check().is_empty());
    }

    #[test]
    fn without_docker_config_the_homes_docker_directory_is_used() {
        let settings = ResolverSettings::read(&config(&[("HOME", "/home/polar")])).unwrap();
        assert_eq!(
            settings.docker_config.as_deref(),
            Some("/home/polar/.docker")
        );
        let both = ResolverSettings::read(&config(&[
            ("HOME", "/home/polar"),
            ("DOCKER_CONFIG", "/etc/oci-registry-auth"),
        ]))
        .unwrap();
        assert_eq!(
            both.docker_config.as_deref(),
            Some("/etc/oci-registry-auth")
        );
    }

    #[test]
    fn what_is_set_is_used() {
        let settings = ResolverSettings::read(&config(&[
            ("DOCKER_CONFIG", "/etc/oci-registry-auth"),
            ("OCI_INSECURE_REGISTRIES", "10.0.2.2:5000, reg.local:5000"),
        ]))
        .unwrap();
        assert_eq!(
            settings.insecure_registries,
            vec!["10.0.2.2:5000", "reg.local:5000"]
        );
        assert_eq!(
            settings.docker_config.as_deref(),
            Some("/etc/oci-registry-auth")
        );
    }
}
