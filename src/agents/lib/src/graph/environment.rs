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

//! The environment the graph controller reads, each variable declared once.
//!
//! [`GraphSettings::from_env`] is the one place these are read and the one place the rule
//! between the TLS variables lives, so the controller, the agent builder's startup check and
//! the render-time checker cannot disagree about what a sound graph environment is.

use cassini_client::ClientKey;
use cassini_client::environment::CUSTODY_SOCKET;
use polar_env::{Described, Env, EnvError, Optional, Required, Section};

pub const GRAPH_DB: Required<String> = Required::text("GRAPH_DB", "the Neo4j database name");
pub const GRAPH_USER: Required<String> = Required::text("GRAPH_USER", "the Neo4j user");
pub const GRAPH_PASSWORD: Required<String> =
    Required::text("GRAPH_PASSWORD", "the Neo4j user's password").secret();
pub const GRAPH_ENDPOINT: Required<String> = Required::text(
    "GRAPH_ENDPOINT",
    "the Neo4j address, for example bolt+s://host:7687",
);
pub const GRAPH_CLIENT_CERT: Optional<String> = Optional::text(
    "GRAPH_CLIENT_CERT",
    "path to the client certificate presented to Neo4j; set with GRAPH_CLIENT_KEY and GRAPH_CA_CERT or not at all",
);
pub const GRAPH_CLIENT_KEY: Optional<String> = Optional::text(
    "GRAPH_CLIENT_KEY",
    "path to the private key for GRAPH_CLIENT_CERT; this or POLAR_CUSTODY_SOCKET, the custody daemon holding the key, not both",
);
pub const GRAPH_CA_CERT: Optional<String> = Optional::text(
    "GRAPH_CA_CERT",
    "path to the CA certificate Neo4j's certificate is verified against",
);

static VARIABLES: &[&dyn Described] = &[
    &GRAPH_DB,
    &GRAPH_USER,
    &GRAPH_PASSWORD,
    &GRAPH_ENDPOINT,
    &GRAPH_CLIENT_CERT,
    &GRAPH_CLIENT_KEY,
    &GRAPH_CA_CERT,
];

/// How the controller secures its connection, from which of the TLS variables are set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphTls {
    /// Mutual TLS: present a client certificate, verify the server against the CA. Its key is a file, or the custody
    /// daemon's (docs/architecture/key-custody.md, "The neo4rs patch").
    Mutual {
        ca: String,
        cert: String,
        key: ClientKey,
    },
    /// Verify the server against the CA; present no client certificate.
    ServerOnly { ca: String },
    /// No TLS configuration beyond what the endpoint scheme implies.
    None,
}

/// Everything the graph controller reads.
#[derive(Debug, Clone)]
pub struct GraphSettings {
    pub database: String,
    pub user: String,
    pub password: String,
    pub endpoint: String,
    pub tls: GraphTls,
}

impl GraphSettings {
    /// Reads and judges the graph environment. Every problem is reported together, each
    /// naming the variable and what it is for.
    pub fn from_env(env: &dyn Env) -> Result<Self, String> {
        let mut problems = Vec::new();
        let mut take = |read: Result<String, EnvError>| match read {
            Ok(value) => value,
            Err(error) => {
                problems.push(error.to_string());
                String::new()
            }
        };
        let database = take(GRAPH_DB.read(env));
        let user = take(GRAPH_USER.read(env));
        let password = take(GRAPH_PASSWORD.read(env));
        let endpoint = take(GRAPH_ENDPOINT.read(env));

        let mut optional = |read: Result<Option<String>, EnvError>| match read {
            Ok(value) => value,
            Err(error) => {
                problems.push(error.to_string());
                None
            }
        };
        let cert = optional(GRAPH_CLIENT_CERT.read(env));
        let key = optional(GRAPH_CLIENT_KEY.read(env));
        let ca = optional(GRAPH_CA_CERT.read(env));
        // The pod's custody socket, declared by the Cassini client: the key of a client certificate shown to Neo4j may
        // be the daemon's. Without a client certificate it is the Cassini client's alone, and says nothing here.
        let socket = optional(CUSTODY_SOCKET.read(env));

        let tls = match (cert, key, ca, socket) {
            (Some(cert), Some(key), Some(ca), None) => GraphTls::Mutual {
                ca,
                cert,
                key: ClientKey::File(key),
            },
            (Some(cert), None, Some(ca), Some(socket)) => GraphTls::Mutual {
                ca,
                cert,
                key: ClientKey::Custody(socket),
            },
            (Some(_), Some(_), Some(_), Some(_)) => {
                problems.push(format!(
                    "{} and {} are both set: the key shown to Neo4j is a file or the custody daemon's, not both",
                    GRAPH_CLIENT_KEY.name(),
                    CUSTODY_SOCKET.name()
                ));
                GraphTls::None
            }
            (None, None, Some(ca), _) => GraphTls::ServerOnly { ca },
            (None, None, None, _) => GraphTls::None,
            // Partial TLS configuration: fail fast rather than silently connecting without
            // the full certificate chain.
            (cert, key, ca, _) => {
                problems.push(format!(
                    "the Neo4j TLS settings are incomplete (GRAPH_CLIENT_CERT is {}, \
                     GRAPH_CLIENT_KEY is {}, GRAPH_CA_CERT is {}): a client certificate needs \
                     all three, its key a file or, with POLAR_CUSTODY_SOCKET, the custody daemon's, \
                     and without one only GRAPH_CA_CERT may be set",
                    state(&cert),
                    state(&key),
                    state(&ca)
                ));
                GraphTls::None
            }
        };

        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        Ok(Self {
            database,
            user,
            password,
            endpoint,
            tls,
        })
    }
}

fn state(value: &Option<String>) -> &'static str {
    if value.is_some() { "set" } else { "not set" }
}

/// What must hold between the graph variables. Run only when each parses.
fn relations(env: &dyn Env) -> Vec<String> {
    match GraphSettings::from_env(env) {
        Ok(_) => Vec::new(),
        Err(problem) => vec![problem],
    }
}

/// What the graph controller reads. Applies to every agent with the graph-write capability.
pub static SECTION: Section = Section {
    name: "graph",
    vars: VARIABLES,
    relations,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const BASE: &[(&str, &str)] = &[
        ("GRAPH_DB", "neo4j"),
        ("GRAPH_USER", "neo4j"),
        ("GRAPH_PASSWORD", "hunter2"),
        ("GRAPH_ENDPOINT", "bolt+s://db:7687"),
    ];

    fn with(extra: &[(&'static str, &'static str)]) -> HashMap<String, String> {
        env(&[BASE, extra].concat())
    }

    #[test]
    fn no_tls_variables_means_no_tls_configuration() {
        let s = GraphSettings::from_env(&env(BASE)).unwrap();
        assert_eq!(s.tls, GraphTls::None);
        assert_eq!(
            (s.database.as_str(), s.endpoint.as_str()),
            ("neo4j", "bolt+s://db:7687")
        );
    }

    #[test]
    fn all_three_make_mutual_tls() {
        let s = GraphSettings::from_env(&with(&[
            ("GRAPH_CLIENT_CERT", "/c.pem"),
            ("GRAPH_CLIENT_KEY", "/k.pem"),
            ("GRAPH_CA_CERT", "/ca.pem"),
        ]))
        .unwrap();
        assert_eq!(
            s.tls,
            GraphTls::Mutual {
                ca: "/ca.pem".into(),
                cert: "/c.pem".into(),
                key: ClientKey::File("/k.pem".into())
            }
        );
    }

    // docs/architecture/key-custody.md, "The neo4rs patch": the key Neo4j is shown may be the custody daemon's, through
    // the socket the Cassini client reads too.
    #[test]
    fn the_key_may_be_the_custody_daemon_s_and_is_one_or_the_other() {
        let custody = GraphSettings::from_env(&with(&[
            ("GRAPH_CLIENT_CERT", "/c.pem"),
            ("GRAPH_CA_CERT", "/ca.pem"),
            ("POLAR_CUSTODY_SOCKET", "/custody/custody.sock"),
        ]))
        .unwrap();
        assert_eq!(
            custody.tls,
            GraphTls::Mutual {
                ca: "/ca.pem".into(),
                cert: "/c.pem".into(),
                key: ClientKey::Custody("/custody/custody.sock".into())
            }
        );
        let error = GraphSettings::from_env(&with(&[
            ("GRAPH_CLIENT_CERT", "/c.pem"),
            ("GRAPH_CLIENT_KEY", "/k.pem"),
            ("GRAPH_CA_CERT", "/ca.pem"),
            ("POLAR_CUSTODY_SOCKET", "/custody/custody.sock"),
        ]))
        .unwrap_err();
        assert!(
            error.contains("GRAPH_CLIENT_KEY") && error.contains("POLAR_CUSTODY_SOCKET"),
            "{error}"
        );
    }

    // Without a client certificate for Neo4j the socket is the Cassini client's alone, and says nothing here.
    #[test]
    fn without_a_client_certificate_the_socket_says_nothing_to_the_graph() {
        let socket = ("POLAR_CUSTODY_SOCKET", "/custody/custody.sock");
        assert_eq!(
            GraphSettings::from_env(&with(&[socket])).unwrap().tls,
            GraphTls::None
        );
        assert_eq!(
            GraphSettings::from_env(&with(&[socket, ("GRAPH_CA_CERT", "/ca.pem")]))
                .unwrap()
                .tls,
            GraphTls::ServerOnly {
                ca: "/ca.pem".into()
            }
        );
        let error =
            GraphSettings::from_env(&with(&[socket, ("GRAPH_CLIENT_CERT", "/c.pem")])).unwrap_err();
        assert!(error.contains("incomplete"), "{error}");
    }

    /// Every way the four can be set or not: the section judges each as the controller reads it.
    #[test]
    fn the_section_judges_every_combination_as_the_controller_reads_it() {
        let vars = [
            ("GRAPH_CLIENT_CERT", "/c.pem"),
            ("GRAPH_CLIENT_KEY", "/k.pem"),
            ("GRAPH_CA_CERT", "/ca.pem"),
            ("POLAR_CUSTODY_SOCKET", "/custody/custody.sock"),
        ];
        for mask in 0u8..16 {
            let given: Vec<_> = vars
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, v)| *v)
                .collect();
            let env = with(&given);
            assert_eq!(
                SECTION.check(&env).is_empty(),
                GraphSettings::from_env(&env).is_ok(),
                "{given:?}"
            );
        }
    }

    #[test]
    fn only_the_ca_means_server_verification_alone() {
        let s = GraphSettings::from_env(&with(&[("GRAPH_CA_CERT", "/ca.pem")])).unwrap();
        assert_eq!(
            s.tls,
            GraphTls::ServerOnly {
                ca: "/ca.pem".into()
            }
        );
    }

    #[test]
    fn a_partial_set_is_refused_and_says_which_are_set() {
        for extra in [
            &[("GRAPH_CLIENT_CERT", "/c.pem")][..],
            &[
                ("GRAPH_CLIENT_CERT", "/c.pem"),
                ("GRAPH_CA_CERT", "/ca.pem"),
            ][..],
            &[
                ("GRAPH_CLIENT_CERT", "/c.pem"),
                ("GRAPH_CLIENT_KEY", "/k.pem"),
            ][..],
            &[("GRAPH_CLIENT_KEY", "/k.pem")][..],
        ] {
            let error = GraphSettings::from_env(&with(extra)).unwrap_err();
            assert!(error.contains("incomplete"), "{extra:?}: {error}");
            assert!(error.contains("GRAPH_CLIENT_CERT is"), "{error}");
        }
    }

    #[test]
    fn every_missing_required_variable_is_named() {
        let error = GraphSettings::from_env(&env(&[])).unwrap_err();
        for name in ["GRAPH_DB", "GRAPH_USER", "GRAPH_PASSWORD", "GRAPH_ENDPOINT"] {
            assert!(error.contains(name), "{name}: {error}");
        }
    }

    #[test]
    fn the_password_is_never_in_a_message() {
        let error = GraphSettings::from_env(&with(&[("GRAPH_CLIENT_CERT", "/c.pem")])).unwrap_err();
        assert!(!error.contains("hunter2"), "{error}");
        let described = SECTION.describe();
        assert!(
            described
                .iter()
                .any(|v| v.name == "GRAPH_PASSWORD" && v.secret)
        );
    }

    #[test]
    fn the_section_judges_the_same_rule() {
        assert!(SECTION.check(&env(BASE)).is_empty());
        assert_eq!(
            SECTION
                .check(&with(&[
                    ("GRAPH_CA_CERT", "/ca.pem"),
                    ("GRAPH_CLIENT_KEY", "/k.pem")
                ]))
                .len(),
            1
        );
    }
}
