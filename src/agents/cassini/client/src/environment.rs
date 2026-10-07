//! The environment a Cassini client reads, each variable declared once.
//!
//! The client needs to know where the broker is and which certificates to present. Those
//! were read with `env::var("X")?`, whose error says only that a variable was not found and
//! never which. They are declared here instead, so a missing one is named with what it is
//! for, and [`SECTION`] lets anything that judges an environment (the agent builder at
//! startup, the render-time checker for the pod a workload will get) judge these too.

use polar_env::{Described, Optional, Required, Section};

pub const TLS_CLIENT_CERT: Required<String> = Required::text(
    "TLS_CLIENT_CERT",
    "path to this agent's client certificate, which it presents to the broker",
);

pub const TLS_CLIENT_KEY: Required<String> = Required::text(
    "TLS_CLIENT_KEY",
    "path to the private key for this agent's client certificate",
);

pub const TLS_CA_CERT: Required<String> = Required::text(
    "TLS_CA_CERT",
    "path to the CA certificate the broker's certificate is verified against",
);

pub const BROKER_ADDR: Required<String> =
    Required::text("BROKER_ADDR", "the broker's address, host:port");

pub const CASSINI_SERVER_NAME: Required<String> = Required::text(
    "CASSINI_SERVER_NAME",
    "the name on the broker's certificate, which the connection is verified against",
);

pub const CASSINI_QUEUE_PATH: Optional<String> = Optional::text(
    "CASSINI_QUEUE_PATH",
    "where the client keeps a local queue of messages it could not deliver (none when unset)",
);

static VARIABLES: &[&dyn Described] = &[
    &TLS_CLIENT_CERT,
    &TLS_CLIENT_KEY,
    &TLS_CA_CERT,
    &BROKER_ADDR,
    &CASSINI_SERVER_NAME,
    &CASSINI_QUEUE_PATH,
];

/// What a Cassini client reads. Applies to every agent that connects to the broker.
pub static SECTION: Section = Section {
    name: "cassini-client",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TCPClientConfig;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const ALL: &[(&str, &str)] = &[
        ("TLS_CLIENT_CERT", "/c/cert.pem"),
        ("TLS_CLIENT_KEY", "/c/key.pem"),
        ("TLS_CA_CERT", "/c/ca.pem"),
        ("BROKER_ADDR", "cassini:8080"),
        ("CASSINI_SERVER_NAME", "cassini.local"),
    ];

    #[test]
    fn a_complete_environment_gives_the_client_its_coordinates() {
        let config = TCPClientConfig::from_env(&env(ALL)).unwrap();
        assert_eq!(config.client_certificate_path, "/c/cert.pem");
        assert_eq!(config.client_key_path, "/c/key.pem");
        assert_eq!(config.ca_certificate_path, "/c/ca.pem");
        assert_eq!(config.broker_endpoint, "cassini:8080");
        assert_eq!(config.server_name, "cassini.local");
    }

    #[test]
    fn every_missing_variable_is_named_with_what_it_is_for() {
        let error =
            TCPClientConfig::from_env(&env(&[("BROKER_ADDR", "cassini:8080")])).unwrap_err();
        for name in [
            "TLS_CLIENT_CERT",
            "TLS_CLIENT_KEY",
            "TLS_CA_CERT",
            "CASSINI_SERVER_NAME",
        ] {
            assert!(error.contains(name), "{name} missing from: {error}");
        }
        assert!(!error.contains("BROKER_ADDR"), "{error}");
        assert!(error.contains("broker"), "says what they are for: {error}");
    }

    #[test]
    fn a_blank_variable_counts_as_missing() {
        let mut pairs = ALL.to_vec();
        pairs.retain(|(k, _)| *k != "BROKER_ADDR");
        pairs.push(("BROKER_ADDR", "  "));
        let error = TCPClientConfig::from_env(&env(&pairs)).unwrap_err();
        assert!(error.contains("BROKER_ADDR"), "{error}");
    }

    #[test]
    fn the_section_checks_the_same_things_the_client_needs() {
        assert!(SECTION.check(&env(ALL)).is_empty());
        let problems = SECTION.check(&env(&[]));
        assert_eq!(problems.len(), 5, "{problems:?}");
    }

    #[test]
    fn the_queue_path_is_optional() {
        assert!(
            SECTION
                .describe()
                .iter()
                .any(|v| v.name == "CASSINI_QUEUE_PATH")
        );
        assert!(SECTION.check(&env(ALL)).is_empty());
    }
}
