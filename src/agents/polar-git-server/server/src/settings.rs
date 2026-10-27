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

//! What the git server reads from its configuration, declared once.

use polar_custody::client::ServerKey;
// The custody socket is the Cassini client's declaration, which this agent registers: a variable is declared once.
use polar_custody::environment::{KeyGiven, one_key};
use polar_env::{Config, ConfigError, Defaulted, Described, Optional, Required, Section};

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

/// The seeds the image was built with: the Nix build sets the variable to the store path holding
/// `infra/seeds`. Elsewhere there are none unless the variable is set at run time.
pub const DEFAULT_SEED_ROOT: &str = match option_env!("POLAR_GIT_SERVER_BUILT_SEED_ROOT") {
    Some(path) => path,
    None => "/nonexistent/polar-git-server-seeds",
};

pub static SEED_ROOT: Defaulted<String> = Defaulted::text(
    "POLAR_GIT_SERVER_SEED_ROOT",
    "the directory a relative `seed` in POLAR_GIT_SERVER_REPOS is resolved against; the image \
     carries the deployment's seed trees here",
    DEFAULT_SEED_ROOT,
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

const KEY_DOC: &str = "path to the private key (PEM) for the server's certificate; this or POLAR_CUSTODY_SOCKET, the custody daemon holding it, not both";

pub static KEY: Optional<String> = Optional::text("POLAR_GIT_SERVER_KEY", KEY_DOC);

pub static CA_CERT: Required<String> = Required::text(
    "POLAR_GIT_SERVER_CA_CERT",
    "path to the CA certificate (PEM) that clients' certificates must chain to",
);

pub static ADMINS: Optional<Vec<String>> = Optional::list(
    "POLAR_GIT_SERVER_ADMINS",
    "identities, comma-separated, that may do anything to every repository and create new ones, \
     in addition to the `admins` in POLAR_GIT_SERVER_REPOS: the DNS name in the certificate of \
     each person (their account's identity) or workload. Kept apart from the repositories because \
     who administers a server is a fact about the deployment, and the repositories are not",
);

static VARIABLES: &[&dyn Described] = &[
    &PORT, &REPO_ROOT, &SEED_ROOT, &REPOS, &ADMINS, &CERT, &KEY, &CA_CERT,
];

pub static SECTION: Section = Section {
    name: "polar-git-server",
    vars: VARIABLES,
    relations: |env| {
        read_key(env)
            .err()
            .map(|e| e.to_string())
            .into_iter()
            .collect()
    },
};

/// The server's key: a file, or the custody daemon's server slot (docs/architecture/key-custody.md, step 4). Exactly
/// one is given.
pub fn read_key(env: &dyn polar_env::Env) -> Result<ServerKey, polar_env::EnvError> {
    Ok(match one_key(env, &KEY, KEY_DOC)? {
        KeyGiven::File(path) => ServerKey::File(path),
        KeyGiven::Socket(socket) => ServerKey::Custody(socket),
    })
}

pub struct GitServerSettings {
    pub port: u16,
    pub repo_root: String,
    pub seed_root: String,
    pub repos: String,
    pub admins: Vec<String>,
    pub cert: String,
    /// The certificate's key: a file, or the custody daemon's.
    pub key: ServerKey,
    pub ca_cert: String,
}

impl GitServerSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            port: config.read(&PORT)?,
            repo_root: config.read(&REPO_ROOT)?,
            seed_root: config.read(&SEED_ROOT)?,
            repos: config.read(&REPOS)?,
            admins: config.read(&ADMINS)?.unwrap_or_default(),
            cert: config.read(&CERT)?,
            key: read_key(config).map_err(ConfigError::Env)?,
            ca_cert: config.read(&CA_CERT)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The Cassini client's variables, as the pod gives them: its key a file, or with the custody socket the daemon's.
    const CLIENT: [(&str, &str); 4] = [
        ("TLS_CLIENT_CERT", "/c/client-cert.pem"),
        ("TLS_CA_CERT", "/c/ca.pem"),
        ("BROKER_ADDR", "cassini:8080"),
        ("CASSINI_SERVER_NAME", "cassini"),
    ];

    fn config(pairs: &[(&str, &str)]) -> Config {
        let custody = pairs.iter().any(|(k, _)| *k == "POLAR_CUSTODY_SOCKET");
        let client_key = (!custody).then_some(("TLS_CLIENT_KEY", "/c/client-key.pem"));
        let env: HashMap<String, String> = CLIENT
            .iter()
            .chain(client_key.iter())
            .chain(pairs.iter())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        // As the agent registers them: its own section, and the Cassini client's, which declares the custody socket.
        Config::builder(&env)
            .register(&SECTION)
            .register(&cassini_client::environment::SECTION)
            .build()
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
    fn no_administrators_are_named_unless_the_deployment_names_them() {
        assert_eq!(
            GitServerSettings::read(&config(&ALL)).unwrap().admins,
            Vec::<String>::new()
        );
        let mut set = ALL.to_vec();
        set.push(("POLAR_GIT_SERVER_ADMINS", "uuid-one, uuid-two"));
        assert_eq!(
            GitServerSettings::read(&config(&set)).unwrap().admins,
            vec!["uuid-one", "uuid-two"]
        );
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
            "POLAR_GIT_SERVER_CA_CERT",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(name)),
                "{name}: {problems:?}"
            );
        }
        assert!(config(&ALL).check().is_empty());
        // The key is judged with the custody socket, once the variables it is judged with are given.
        let mut keyless = ALL.to_vec();
        keyless.retain(|(k, _)| *k != "POLAR_GIT_SERVER_KEY");
        let problems = config(&keyless).check().join(" | ");
        assert!(
            problems.contains("POLAR_GIT_SERVER_KEY") && problems.contains("POLAR_CUSTODY_SOCKET"),
            "{problems}"
        );
    }

    // docs/architecture/key-custody.md, step 4: the server's key may be the custody daemon's server slot.
    #[test]
    fn the_key_may_be_the_custody_daemon_s_and_is_one_or_the_other() {
        let mut custody = ALL.to_vec();
        custody.retain(|(k, _)| *k != "POLAR_GIT_SERVER_KEY");
        custody.push(("POLAR_CUSTODY_SOCKET", "/run/polar-custody/custody.sock"));
        assert!(config(&custody).check().is_empty());
        assert_eq!(
            GitServerSettings::read(&config(&custody)).unwrap().key,
            ServerKey::Custody("/run/polar-custody/custody.sock".into())
        );
        let mut both = ALL.to_vec();
        both.push(("POLAR_CUSTODY_SOCKET", "/run/polar-custody/custody.sock"));
        assert!(config(&both).check().join(" | ").contains("set one"));
        assert_eq!(
            GitServerSettings::read(&config(&ALL)).unwrap().key,
            ServerKey::File("/c/key.pem".into())
        );
    }
}
