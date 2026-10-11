//! What the git server reads from its configuration, declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Required, Section};

pub const DEFAULT_PORT: &str = "8443";

pub static PORT: Defaulted<u16> = Defaulted::number(
    "POLAR_GIT_SERVER_PORT",
    "the port the git server listens on, on every address",
    DEFAULT_PORT,
);

pub static REPO_ROOT: Required<String> = Required::text(
    "POLAR_GIT_SERVER_REPO_ROOT",
    "the directory the bare repositories live in; it must be persistent storage",
);

pub static REPOS: Required<String> = Required::text(
    "POLAR_GIT_SERVER_REPOS",
    "JSON declaring the repositories, who may read and write each, and the directory each is \
     seeded from the first time it is created: \
     {\"admins\": [..], \"repos\": [{\"name\", \"readers\", \"writers\", \"seed\"}]}",
);

pub static CERT: Required<String> = Required::text(
    "POLAR_GIT_SERVER_CERT",
    "path to the server's own certificate chain (PEM), which it presents to every client",
);

pub static KEY: Required<String> = Required::text(
    "POLAR_GIT_SERVER_KEY",
    "path to the private key (PEM) for the server's certificate",
);

pub static CA_CERT: Required<String> = Required::text(
    "POLAR_GIT_SERVER_CA_CERT",
    "path to the CA certificate (PEM) that clients' certificates must chain to",
);

static VARIABLES: &[&dyn Described] = &[&PORT, &REPO_ROOT, &REPOS, &CERT, &KEY, &CA_CERT];

pub static SECTION: Section = Section {
    name: "polar-git-server",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct GitServerSettings {
    pub port: u16,
    pub repo_root: String,
    pub repos: String,
    pub cert: String,
    pub key: String,
    pub ca_cert: String,
}

impl GitServerSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            port: config.read(&PORT)?,
            repo_root: config.read(&REPO_ROOT)?,
            repos: config.read(&REPOS)?,
            cert: config.read(&CERT)?,
            key: config.read(&KEY)?,
            ca_cert: config.read(&CA_CERT)?,
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

    const ALL: [(&str, &str); 5] = [
        ("POLAR_GIT_SERVER_REPO_ROOT", "/data"),
        ("POLAR_GIT_SERVER_REPOS", "{}"),
        ("POLAR_GIT_SERVER_CERT", "/c/cert.pem"),
        ("POLAR_GIT_SERVER_KEY", "/c/key.pem"),
        ("POLAR_GIT_SERVER_CA_CERT", "/c/ca.pem"),
    ];

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn the_port_is_8443_unless_set() {
        assert_eq!(GitServerSettings::read(&config(&ALL)).unwrap().port, 8443);
        let mut set = ALL.to_vec();
        set.push(("POLAR_GIT_SERVER_PORT", "9443"));
        assert_eq!(GitServerSettings::read(&config(&set)).unwrap().port, 9443);
    }

    #[test]
    fn what_the_server_cannot_do_without_is_a_problem_before_it_starts() {
        let problems = config(&[]).check();
        for name in [
            "POLAR_GIT_SERVER_REPO_ROOT",
            "POLAR_GIT_SERVER_REPOS",
            "POLAR_GIT_SERVER_CERT",
            "POLAR_GIT_SERVER_KEY",
            "POLAR_GIT_SERVER_CA_CERT",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(name)),
                "{name}: {problems:?}"
            );
        }
        assert!(config(&ALL).check().is_empty());
    }
}
