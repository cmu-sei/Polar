//! The environment the graph controller reads, each variable declared once.
//!
//! [`GraphSettings::from_env`] is the one place these are read and the one place the rule
//! between the TLS variables lives, so the controller, the agent builder's startup check and
//! the render-time checker cannot disagree about what a sound graph environment is.

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
    "path to the private key for GRAPH_CLIENT_CERT",
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
    /// Mutual TLS: present a client certificate, verify the server against the CA.
    Mutual {
        ca: String,
        cert: String,
        key: String,
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

        let tls = match (cert, key, ca) {
            (Some(cert), Some(key), Some(ca)) => GraphTls::Mutual { ca, cert, key },
            (None, None, Some(ca)) => GraphTls::ServerOnly { ca },
            (None, None, None) => GraphTls::None,
            // Partial TLS configuration: fail fast rather than silently connecting without
            // the full certificate chain.
            (cert, key, ca) => {
                problems.push(format!(
                    "the Neo4j TLS settings are incomplete (GRAPH_CLIENT_CERT is {}, \
                     GRAPH_CLIENT_KEY is {}, GRAPH_CA_CERT is {}): a client certificate needs \
                     all three, and without one only GRAPH_CA_CERT may be set",
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
                key: "/k.pem".into()
            }
        );
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
