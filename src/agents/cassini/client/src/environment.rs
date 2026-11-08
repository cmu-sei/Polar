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

//! The environment a Cassini client reads, each variable declared once.
//!
//! The client needs to know where the broker is and which certificates to present. Those
//! were read with `env::var("X")?`, whose error says only that a variable was not found and
//! never which. They are declared here instead, so a missing one is named with what it is
//! for, and [`SECTION`] lets anything that judges an environment (the agent builder at
//! startup, the render-time checker for the pod a workload will get) judge these too.

use crate::ClientKey;
use polar_env::{Described, Env, EnvError, Optional, Required, Section};

pub const TLS_CLIENT_CERT: Required<String> = Required::text(
    "TLS_CLIENT_CERT",
    "path to this agent's client certificate, which it presents to the broker",
);

const TLS_CLIENT_KEY_DOC: &str = "path to the private key for this agent's client certificate; this or POLAR_CUSTODY_SOCKET, not both";

pub const TLS_CLIENT_KEY: Optional<String> = Optional::text("TLS_CLIENT_KEY", TLS_CLIENT_KEY_DOC);

/// The custody daemon's socket (docs/architecture/key-custody.md), declared once in `polar-custody` for every component
/// that may be given the daemon's key.
pub use polar_custody::environment::CUSTODY_SOCKET;

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
    &CUSTODY_SOCKET,
    &TLS_CA_CERT,
    &BROKER_ADDR,
    &CASSINI_SERVER_NAME,
    &CASSINI_QUEUE_PATH,
];

/// What a Cassini client reads. Applies to every agent that connects to the broker.
pub static SECTION: Section = Section {
    name: "cassini-client",
    vars: VARIABLES,
    relations: |env| {
        read_client_key(env)
            .err()
            .map(|e| e.to_string())
            .into_iter()
            .collect()
    },
};

/// Where the client key is: a file, or the custody daemon. Exactly one of the two is given.
pub fn read_client_key(env: &dyn Env) -> Result<ClientKey, EnvError> {
    use polar_custody::environment::{KeyGiven, one_key};
    Ok(match one_key(env, &TLS_CLIENT_KEY, TLS_CLIENT_KEY_DOC)? {
        KeyGiven::File(path) => ClientKey::File(path),
        KeyGiven::Socket(socket) => ClientKey::Custody(socket),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientKey, TCPClientConfig};
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
        assert_eq!(config.client_key, ClientKey::File("/c/key.pem".into()));
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
        // The four required; the key is judged with the socket once they are given, as a relation.
        let problems = SECTION.check(&env(&[]));
        assert_eq!(problems.len(), 4, "{problems:?}");
        let mut pairs = ALL.to_vec();
        pairs.retain(|(k, _)| *k != "TLS_CLIENT_KEY");
        let problems = SECTION.check(&env(&pairs));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("TLS_CLIENT_KEY"), "{problems:?}");
        assert!(problems[0].contains("POLAR_CUSTODY_SOCKET"), "{problems:?}");
    }

    /// Every way the two can be given (unset, blank, a path): exactly one is, and that one is the key; the section
    /// judges each as the client reads it.
    #[test]
    fn exactly_one_of_a_key_file_and_the_custody_socket_is_given() {
        let states = [None, Some("  "), Some("/given")];
        let given = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
        for key in states {
            for socket in states {
                let mut pairs = ALL.to_vec();
                pairs.retain(|(k, _)| *k != "TLS_CLIENT_KEY");
                pairs.extend(key.map(|v| ("TLS_CLIENT_KEY", v)));
                pairs.extend(socket.map(|v| ("POLAR_CUSTODY_SOCKET", v)));
                let read = TCPClientConfig::from_env(&env(&pairs));
                let judged = SECTION.check(&env(&pairs));
                assert_eq!(judged.is_empty(), read.is_ok(), "{pairs:?}: {judged:?}");
                match (given(key), given(socket), read) {
                    (true, false, Ok(config)) => {
                        assert_eq!(config.client_key, ClientKey::File("/given".into()))
                    }
                    (false, true, Ok(config)) => {
                        assert_eq!(config.client_key, ClientKey::Custody("/given".into()))
                    }
                    (true, true, Err(error)) | (false, false, Err(error)) => {
                        assert!(error.contains("TLS_CLIENT_KEY"), "{error}");
                        assert!(error.contains("POLAR_CUSTODY_SOCKET"), "{error}");
                    }
                    (_, _, read) => panic!("{pairs:?}: {read:?}"),
                }
            }
        }
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
