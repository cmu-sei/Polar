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

//! What the broker reads from its configuration, declared once.
//!
//! These were read with `std::env::var` in `BrokerArgs::new`, so nothing could list them, check
//! them before the pod started, or say what they were for. Declared here they are in the broker's
//! scaffold and judged with everything else (the builder's, the health actor's, how it logs). The broker is
//! not a Cassini client, so none of the client's variables are asked of it.

use polar_env::{Config, ConfigError, Defaulted, Described, Required, Section};

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

pub static SERVER_CERT_CHAIN: Required<String> = Required::text(
    "TLS_SERVER_CERT_CHAIN",
    "path to the broker's own certificate (PEM), which it presents to every client",
);

pub static SERVER_KEY: Required<String> = Required::text(
    "TLS_SERVER_KEY",
    "path to the private key for the broker's certificate",
);

pub static CA_CERT: Required<String> = Required::text(
    "TLS_CA_CERT",
    "path to the CA certificate the clients' certificates are verified against",
);

pub static BIND_ADDR: Defaulted<String> = Defaulted::text(
    "CASSINI_BIND_ADDR",
    "the address the broker listens on, host:port",
    DEFAULT_BIND_ADDR,
);

pub static SHUTDOWN_TOKEN: Required<String> = Required::text(
    "CASSINI_SHUTDOWN_TOKEN",
    "the token a client must present to ask the broker to shut down over the wire; the broker's \
     own orderly shutdown (a signal, a health verdict) presents it itself",
)
.secret();

/// How long a connection keeps the broker's decision about a topic (docs/architecture/policy-enforcement.md,
/// "Decisions"). A change of authority makes every kept decision stale at once; the time bounds how late a rule that
/// reads the clock takes effect. Zero keeps none, which is how enforcement without the cache is measured.
pub static DECISION_CACHE_SECS: Defaulted<u64> = Defaulted::seconds(
    "CASSINI_DECISION_CACHE_SECS",
    "how long a connection keeps the broker's decision about a topic, in seconds; a change of policy or groups ends it \
     at once, and 0 keeps none",
    "5",
);

static VARIABLES: &[&dyn Described] = &[
    &SERVER_CERT_CHAIN,
    &SERVER_KEY,
    &CA_CERT,
    &BIND_ADDR,
    &SHUTDOWN_TOKEN,
    &DECISION_CACHE_SECS,
];

/// The section the broker registers.
pub static SECTION: Section = Section {
    name: "cassini-broker",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct BrokerSettings {
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
    pub bind_addr: String,
    pub shutdown_auth_token: String,
    pub decision_cache: std::time::Duration,
}

impl BrokerSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            server_cert_file: config.read(&SERVER_CERT_CHAIN)?,
            private_key_file: config.read(&SERVER_KEY)?,
            ca_cert_file: config.read(&CA_CERT)?,
            bind_addr: config.read(&BIND_ADDR)?,
            shutdown_auth_token: config.read(&SHUTDOWN_TOKEN)?,
            decision_cache: std::time::Duration::from_secs(config.read(&DECISION_CACHE_SECS)?),
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

    const MINIMUM: &[(&str, &str)] = &[
        ("TLS_SERVER_CERT_CHAIN", "/tls/cert.pem"),
        ("TLS_SERVER_KEY", "/tls/key.pem"),
        ("TLS_CA_CERT", "/tls/ca.pem"),
        ("CASSINI_SHUTDOWN_TOKEN", "t0ken"),
    ];

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn the_certificate_the_key_the_ca_and_the_token_are_enough() {
        let settings = BrokerSettings::read(&config(MINIMUM)).unwrap();
        assert_eq!(settings.server_cert_file, "/tls/cert.pem");
        assert_eq!(settings.private_key_file, "/tls/key.pem");
        assert_eq!(settings.ca_cert_file, "/tls/ca.pem");
        assert_eq!(settings.bind_addr, DEFAULT_BIND_ADDR);
        assert_eq!(settings.shutdown_auth_token, "t0ken");
        assert_eq!(settings.decision_cache, std::time::Duration::from_secs(5));
    }

    #[test]
    fn what_is_set_overrides_the_defaults() {
        let mut vars = MINIMUM.to_vec();
        vars.extend([
            ("CASSINI_BIND_ADDR", "0.0.0.0:9000"),
            ("CASSINI_DECISION_CACHE_SECS", "0"),
        ]);
        let settings = BrokerSettings::read(&config(&vars)).unwrap();
        assert_eq!(settings.bind_addr, "0.0.0.0:9000");
        assert_eq!(settings.decision_cache, std::time::Duration::ZERO);
    }

    #[test]
    fn everything_missing_is_named_with_what_it_is_for() {
        let problems = config(&[]).check().join(" | ");
        for (name, what) in [
            ("TLS_SERVER_CERT_CHAIN", "presents to every client"),
            ("TLS_SERVER_KEY", "private key"),
            ("TLS_CA_CERT", "clients' certificates"),
            ("CASSINI_SHUTDOWN_TOKEN", "shut down over the wire"),
        ] {
            assert!(
                problems.contains(name) && problems.contains(what),
                "{name}: {problems}"
            );
        }
    }

    #[test]
    fn the_token_is_never_shown_in_a_report() {
        let report = config(MINIMUM).report();
        let token = report
            .iter()
            .find(|r| r.name == "CASSINI_SHUTDOWN_TOKEN")
            .unwrap();
        assert_eq!(token.value.as_deref(), Some("<hidden>"));
    }
}
