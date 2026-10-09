//! What the OCI resolver reads from its configuration, declared once.
//!
//! `PROXY_CA_CERT` was read from the process environment by a helper in `polar`, and
//! `DOCKER_CONFIG` by the `docker_credential` crate itself, so neither was declared or checked
//! before the agent started, and neither could reach an agent whose environment is a pointer to a
//! configuration document. Declared here they are in its scaffold, and the resolver is handed the
//! values.

use polar_env::{Config, ConfigError, Described, Optional, Section};

pub static PROXY_CA_CERT: Optional<String> = Optional::text(
    "PROXY_CA_CERT",
    "path to a PEM CA certificate to trust, as well as the system's, when pulling manifests from a \
     private registry or through a proxy that intercepts TLS; a path that is set and cannot be \
     read stops the agent",
);

pub static DOCKER_CONFIG: Optional<String> = Optional::text(
    "DOCKER_CONFIG",
    "the directory holding the Docker credential file config.json that registry credentials are \
     looked up in; when unset, the usual places (the home directory's .docker) are tried, and \
     without credentials the registry is asked anonymously",
);

static VARIABLES: &[&dyn Described] = &[&PROXY_CA_CERT, &DOCKER_CONFIG];

/// The section the OCI resolver registers.
pub static SECTION: Section = Section {
    name: "oci-resolver",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct ResolverSettings {
    pub proxy_ca_cert: Option<String>,
    pub docker_config: Option<String>,
}

impl ResolverSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            proxy_ca_cert: config.read(&PROXY_CA_CERT)?,
            docker_config: config.read(&DOCKER_CONFIG)?,
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
        assert_eq!(settings.proxy_ca_cert, None);
        assert_eq!(settings.docker_config, None);
        assert!(config(&[]).check().is_empty());
    }

    #[test]
    fn what_is_set_is_used() {
        let settings = ResolverSettings::read(&config(&[
            ("PROXY_CA_CERT", "/etc/proxy/ca.pem"),
            ("DOCKER_CONFIG", "/etc/oci-registry-auth"),
        ]))
        .unwrap();
        assert_eq!(settings.proxy_ca_cert.as_deref(), Some("/etc/proxy/ca.pem"));
        assert_eq!(settings.docker_config.as_deref(), Some("/etc/oci-registry-auth"));
    }
}
