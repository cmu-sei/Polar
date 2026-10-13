//! The environment polar-init reads, each variable declared once, and judged as a whole
//! before anything else happens.
//!
//! polar-init waits on the network, so a mistake in its environment used to show up late: a
//! missing broker address looped forever at "Cassini client config not yet ready", and an
//! unset certificate issuer URL stopped it only after it had parsed its arguments. Everything
//! it reads is read here, once, into [`InitSettings`], and every problem is reported together,
//! before the first wait.
//!
//! The Cassini client's and the crypto actor's variables are declared by those crates and
//! reused, so the init container and the agent it starts read them the same way.

use cassini_client::TCPClientConfig;
use polar_crypto::environment::CryptoSettings;
use polar_env::{Config, Defaulted, Described, Env, Optional, Section};

/// Where the projected service-account token is, unless the deployment says.
pub const DEFAULT_TOKEN_PATH: &str = "/workspace/token";

/// Where the certificates come from. Only the bootstrap services (Cassini and the credentials agent with its
/// observer) ask cert-issuer directly; every other workload asks the credentials agent through its observer,
/// which is where the controls on issuance are. See docs/architecture/cert-issuer-isolation.md.
pub const ISSUER: Defaulted<String> = Defaulted::text(
    "POLAR_INIT_ISSUER",
    "where the certificates are requested from: `cert-issuer` (the bootstrap services only) or `credentials-agent` (through its observer, for every other workload)",
    ISSUER_CERT_ISSUER,
);

pub const ISSUER_CERT_ISSUER: &str = "cert-issuer";
pub const ISSUER_CREDENTIALS_AGENT: &str = "credentials-agent";

pub const CERT_ISSUER_URL: Optional<String> = Optional::text(
    "POLAR_INIT_CERT_ISSUER_URL",
    "the URL of the cert-issuer service the certificates are requested from; required when POLAR_INIT_ISSUER is `cert-issuer`",
);

pub const OBSERVER_URL: Optional<String> = Optional::text(
    "POLAR_INIT_OBSERVER_URL",
    "the URL of the credentials agent's observer the certificates are requested from; required when POLAR_INIT_ISSUER is `credentials-agent`",
);

pub const TOKEN_PATH: Defaulted<String> = Defaulted::text(
    "POLAR_INIT_TOKEN_PATH",
    "where the projected service-account token is read from",
    DEFAULT_TOKEN_PATH,
);

pub const CA_CERT: Optional<String> = Optional::text(
    "POLAR_INIT_CA_CERT",
    "path to the Polar CA certificate cert-issuer is verified against from the first request; unset means the CA is fetched once and pinned",
);

/// Skips the escrow-keypair bootstrap when set to any non-empty value. Required for the three
/// core services (the Credentials Agent, cert-issuer and Cassini), whose own polar-init would
/// otherwise wait on a service that cannot exist yet.
pub const SKIP_ESCROW_BOOTSTRAP: Optional<String> = Optional::text(
    "POLAR_INIT_SKIP_ESCROW_BOOTSTRAP",
    "set to any non-empty value to skip the escrow-keypair bootstrap, as the core services (credentials agent, cert-issuer, Cassini) must",
);

static VARIABLES: &[&dyn Described] = &[
    &ISSUER,
    &CERT_ISSUER_URL,
    &OBSERVER_URL,
    &TOKEN_PATH,
    &CA_CERT,
    &SKIP_ESCROW_BOOTSTRAP,
];

/// What polar-init reads of its own.
pub static SECTION: Section = Section {
    name: "polar-init",
    vars: VARIABLES,
    relations: issuer_is_consistent,
};

/// The issuer must be one of the two, and its address must be given.
fn issuer_is_consistent(env: &dyn Env) -> Vec<String> {
    let given = |read: Result<Option<String>, polar_env::EnvError>| {
        read.ok().flatten().is_some_and(|v| !v.trim().is_empty())
    };
    match ISSUER.read(env).as_deref() {
        Ok(ISSUER_CERT_ISSUER) if !given(CERT_ISSUER_URL.read(env)) => vec![format!(
            "{} is required when {} is `{ISSUER_CERT_ISSUER}`",
            CERT_ISSUER_URL.name(),
            ISSUER.name()
        )],
        Ok(ISSUER_CREDENTIALS_AGENT) if !given(OBSERVER_URL.read(env)) => vec![format!(
            "{} is required when {} is `{ISSUER_CREDENTIALS_AGENT}`",
            OBSERVER_URL.name(),
            ISSUER.name()
        )],
        Ok(ISSUER_CERT_ISSUER | ISSUER_CREDENTIALS_AGENT) | Err(_) => Vec::new(),
        Ok(other) => vec![format!(
            "{} `{other}` must be `{ISSUER_CERT_ISSUER}` or `{ISSUER_CREDENTIALS_AGENT}`",
            ISSUER.name()
        )],
    }
}

/// What the escrow bootstrap needs, which a core service's init does not.
#[derive(Debug, Clone)]
pub struct EscrowSettings {
    pub client: TCPClientConfig,
    pub crypto: CryptoSettings,
}

/// Where the certificates are requested from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issuance {
    /// Straight from cert-issuer, with the pod's token: the bootstrap services only.
    CertIssuer {
        url: String,
        ca_cert_path: Option<String>,
    },
    /// From the credentials agent through its observer, with the token sealed to an ephemeral session.
    Observer { url: String },
}

/// Everything polar-init is configured with by its environment.
#[derive(Debug, Clone)]
pub struct InitSettings {
    pub issuance: Issuance,
    pub token_path: String,
    /// `None` when the bootstrap is skipped, in which case nothing of the client's or the
    /// crypto actor's is read or required.
    pub escrow: Option<EscrowSettings>,
}

/// The one snapshot of the process polar-init runs on: its own variables, the Cassini
/// client's and the crypto actor's, and nothing else.
pub fn snapshot(env: &dyn Env) -> Config {
    Config::builder(env)
        .register(&SECTION)
        .register(&cassini_client::environment::SECTION)
        .register(&polar_crypto::environment::SECTION)
        .build()
}

impl InitSettings {
    /// Reads and judges `env`, naming every problem together.
    pub fn from_env(env: &dyn Env) -> Result<Self, String> {
        let mut problems: Vec<String> = Vec::new();
        // Components report their problems joined with "; ". They share TLS_CLIENT_CERT, so
        // the same problem can come from two of them; it is said once.
        let mut note = |joined: String| {
            for problem in joined.split("; ") {
                if !problems.iter().any(|p| p == problem) {
                    problems.push(problem.to_string());
                }
            }
        };

        let mut take = |read: Result<String, polar_env::EnvError>| match read {
            Ok(value) => value,
            Err(error) => {
                note(error.to_string());
                String::new()
            }
        };
        let token_path = take(TOKEN_PATH.read(env));
        let mode = take(ISSUER.read(env));
        let optional = |read: Result<Option<String>, polar_env::EnvError>,
                        note: &mut dyn FnMut(String)| match read {
            Ok(value) => value.filter(|v| !v.trim().is_empty()),
            Err(error) => {
                note(error.to_string());
                None
            }
        };
        let ca_cert_path = optional(CA_CERT.read(env), &mut note);
        let cert_issuer_url = optional(CERT_ISSUER_URL.read(env), &mut note);
        let observer_url = optional(OBSERVER_URL.read(env), &mut note);
        // Said once, whatever else is wrong: which address a mode needs.
        for problem in issuer_is_consistent(env) {
            note(problem);
        }
        let issuance = match mode.as_str() {
            ISSUER_CREDENTIALS_AGENT => Issuance::Observer {
                url: observer_url.unwrap_or_default(),
            },
            _ => Issuance::CertIssuer {
                url: cert_issuer_url.unwrap_or_default(),
                ca_cert_path,
            },
        };
        // Any non-empty value skips, as it always did (the value itself does not matter).
        let skip = match SKIP_ESCROW_BOOTSTRAP.read(env) {
            Ok(value) => value.is_some(),
            Err(error) => {
                note(error.to_string());
                false
            }
        };

        let escrow = if skip {
            None
        } else {
            let client = TCPClientConfig::from_env(env);
            let crypto = CryptoSettings::from_env(env);
            match (client, crypto) {
                (Ok(client), Ok(crypto)) => Some(EscrowSettings { client, crypto }),
                (client, crypto) => {
                    if let Err(e) = client {
                        note(e);
                    }
                    if let Err(e) = crypto {
                        note(e);
                    }
                    None
                }
            }
        };

        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        Ok(Self {
            issuance,
            token_path,
            escrow,
        })
    }
}

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

    const OWN: &[(&str, &str)] = &[("POLAR_INIT_CERT_ISSUER_URL", "https://cert-issuer:8443")];

    const BOOTSTRAP: &[(&str, &str)] = &[
        ("TLS_CLIENT_CERT", "/c/cert.pem"),
        ("TLS_CLIENT_KEY", "/c/key.pem"),
        ("TLS_CA_CERT", "/c/ca.pem"),
        ("BROKER_ADDR", "cassini:8080"),
        ("CASSINI_SERVER_NAME", "cassini.local"),
    ];

    fn with(parts: &[&[(&str, &str)]]) -> HashMap<String, String> {
        env(&parts.concat())
    }

    #[test]
    fn a_core_service_needs_only_the_issuer_url() {
        let s = InitSettings::from_env(&with(&[OWN, &[("POLAR_INIT_SKIP_ESCROW_BOOTSTRAP", "1")]]))
            .unwrap();
        assert_eq!(
            s.issuance,
            Issuance::CertIssuer {
                url: "https://cert-issuer:8443".to_string(),
                ca_cert_path: None
            }
        );
        assert_eq!(s.token_path, DEFAULT_TOKEN_PATH);
        assert!(s.escrow.is_none(), "skipped: nothing of the client is read");
    }

    #[test]
    fn any_non_empty_value_skips_and_an_empty_one_does_not() {
        for skipping in ["1", "true", "false", "no"] {
            let s = InitSettings::from_env(&with(&[
                OWN,
                &[("POLAR_INIT_SKIP_ESCROW_BOOTSTRAP", skipping)],
            ]))
            .unwrap();
            assert!(s.escrow.is_none(), "{skipping:?} skips, as it always did");
        }
        // Not skipped, so the bootstrap's settings are required.
        let error =
            InitSettings::from_env(&with(&[OWN, &[("POLAR_INIT_SKIP_ESCROW_BOOTSTRAP", "")]]))
                .unwrap_err();
        assert!(error.contains("BROKER_ADDR"), "{error}");
    }

    #[test]
    fn an_agent_that_bootstraps_has_the_client_and_crypto_settings() {
        let s = InitSettings::from_env(&with(&[
            OWN,
            BOOTSTRAP,
            &[
                ("KUBERNETES_SA_TOKEN", "/workspace/token"),
                ("POLAR_INIT_CA_CERT", "/ca/ca.pem"),
                ("POLAR_INIT_TOKEN_PATH", "/t"),
            ],
        ]))
        .unwrap();
        let escrow = s.escrow.expect("bootstrap not skipped");
        assert_eq!(escrow.client.broker_endpoint, "cassini:8080");
        assert_eq!(escrow.crypto.own_certificate_path, "/c/cert.pem");
        assert_eq!(escrow.crypto.service_account_token_path, "/workspace/token");
        assert_eq!(
            s.issuance,
            Issuance::CertIssuer {
                url: "https://cert-issuer:8443".to_string(),
                ca_cert_path: Some("/ca/ca.pem".to_string())
            }
        );
        assert_eq!(s.token_path, "/t");
    }

    #[test]
    fn every_problem_is_reported_together_before_anything_waits() {
        let error = InitSettings::from_env(&env(&[])).unwrap_err();
        for name in [
            "POLAR_INIT_CERT_ISSUER_URL",
            "TLS_CLIENT_CERT",
            "TLS_CLIENT_KEY",
            "TLS_CA_CERT",
            "BROKER_ADDR",
            "CASSINI_SERVER_NAME",
        ] {
            assert!(error.contains(name), "{name}: {error}");
        }
    }

    #[test]
    fn a_blank_issuer_url_is_missing() {
        let error = InitSettings::from_env(&with(&[
            &[("POLAR_INIT_CERT_ISSUER_URL", "  ")],
            &[("POLAR_INIT_SKIP_ESCROW_BOOTSTRAP", "1")],
        ]))
        .unwrap_err();
        assert!(error.contains("POLAR_INIT_CERT_ISSUER_URL"), "{error}");
    }

    #[test]
    fn the_snapshot_holds_only_what_the_three_sections_declare() {
        let config = snapshot(&with(&[
            OWN,
            BOOTSTRAP,
            &[("AN_UNRELATED_SECRET", "leak-me"), ("GRAPH_PASSWORD", "no")],
        ]));
        assert_eq!(config.var("AN_UNRELATED_SECRET"), None);
        assert_eq!(config.var("GRAPH_PASSWORD"), None);
        assert_eq!(config.var("BROKER_ADDR").as_deref(), Some("cassini:8080"));
        assert!(InitSettings::from_env(&config).is_ok());
    }

    // ---- where the certificates are requested from ---------------------------------------------------------

    const SKIP: &[(&str, &str)] = &[("POLAR_INIT_SKIP_ESCROW_BOOTSTRAP", "1")];

    #[test]
    fn with_no_issuer_named_the_certificates_come_from_cert_issuer_as_they_always_did() {
        let s = InitSettings::from_env(&with(&[OWN, SKIP])).unwrap();
        assert!(matches!(s.issuance, Issuance::CertIssuer { .. }));
    }

    #[test]
    fn a_workload_asks_the_credentials_agent_through_its_observer_and_needs_no_cert_issuer() {
        let s = InitSettings::from_env(&with(&[
            &[
                ("POLAR_INIT_ISSUER", "credentials-agent"),
                (
                    "POLAR_INIT_OBSERVER_URL",
                    "http://credentials-agent-observer:8080",
                ),
            ],
            SKIP,
        ]))
        .unwrap();
        assert_eq!(
            s.issuance,
            Issuance::Observer {
                url: "http://credentials-agent-observer:8080".to_string()
            }
        );
    }

    #[test]
    fn the_rendered_observer_mode_has_a_blank_cert_issuer_address_and_that_is_fine() {
        // The catalog renders POLAR_INIT_CERT_ISSUER_URL as an empty string for a workload on the credentials
        // agent, so that nothing in its pod is told where cert-issuer is.
        let s = InitSettings::from_env(&with(&[
            &[
                ("POLAR_INIT_CERT_ISSUER_URL", ""),
                ("POLAR_INIT_ISSUER", "credentials-agent"),
                ("POLAR_INIT_OBSERVER_URL", "http://obs:8080"),
            ],
            SKIP,
        ]))
        .unwrap();
        assert_eq!(
            s.issuance,
            Issuance::Observer {
                url: "http://obs:8080".to_string()
            }
        );
    }

    #[test]
    fn each_mode_names_the_address_it_is_missing_and_not_the_other() {
        let error = InitSettings::from_env(&with(&[
            &[("POLAR_INIT_ISSUER", "credentials-agent")],
            SKIP,
        ]))
        .unwrap_err();
        assert!(error.contains("POLAR_INIT_OBSERVER_URL"), "{error}");
        assert!(!error.contains("POLAR_INIT_CERT_ISSUER_URL"), "{error}");

        let error = InitSettings::from_env(&with(&[&[("POLAR_INIT_ISSUER", "cert-issuer")], SKIP]))
            .unwrap_err();
        assert!(error.contains("POLAR_INIT_CERT_ISSUER_URL"), "{error}");
        assert!(!error.contains("POLAR_INIT_OBSERVER_URL"), "{error}");
    }

    #[test]
    fn a_blank_observer_address_is_missing_and_an_unknown_issuer_is_named() {
        let error = InitSettings::from_env(&with(&[
            &[
                ("POLAR_INIT_ISSUER", "credentials-agent"),
                ("POLAR_INIT_OBSERVER_URL", "   "),
            ],
            SKIP,
        ]))
        .unwrap_err();
        assert!(error.contains("POLAR_INIT_OBSERVER_URL"), "{error}");

        let error =
            InitSettings::from_env(&with(&[OWN, &[("POLAR_INIT_ISSUER", "the-moon")], SKIP]))
                .unwrap_err();
        assert!(
            error.contains("the-moon") && error.contains("credentials-agent"),
            "{error}"
        );
    }

    #[test]
    fn the_issuer_variables_are_in_the_snapshot_and_the_section_is_sound() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
        let config = snapshot(&with(&[
            &[
                ("POLAR_INIT_ISSUER", "credentials-agent"),
                ("POLAR_INIT_OBSERVER_URL", "http://o:8080"),
            ],
            SKIP,
        ]));
        assert_eq!(
            config.var("POLAR_INIT_ISSUER").as_deref(),
            Some("credentials-agent")
        );
        assert_eq!(
            config.var("POLAR_INIT_OBSERVER_URL").as_deref(),
            Some("http://o:8080")
        );
    }
}
