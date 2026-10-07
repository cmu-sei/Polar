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
use polar_env::{Config, Defaulted, Described, Env, Optional, Required, Section};

/// Where the projected service-account token is, unless the deployment says.
pub const DEFAULT_TOKEN_PATH: &str = "/workspace/token";

pub const CERT_ISSUER_URL: Required<String> = Required::text(
    "POLAR_INIT_CERT_ISSUER_URL",
    "the URL of the cert-issuer service the certificates are requested from",
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
    &CERT_ISSUER_URL,
    &TOKEN_PATH,
    &CA_CERT,
    &SKIP_ESCROW_BOOTSTRAP,
];

/// What polar-init reads of its own.
pub static SECTION: Section = Section {
    name: "polar-init",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// What the escrow bootstrap needs, which a core service's init does not.
#[derive(Debug, Clone)]
pub struct EscrowSettings {
    pub client: TCPClientConfig,
    pub crypto: CryptoSettings,
}

/// Everything polar-init is configured with by its environment.
#[derive(Debug, Clone)]
pub struct InitSettings {
    pub cert_issuer_url: String,
    pub token_path: String,
    pub ca_cert_path: Option<String>,
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
        let cert_issuer_url = take(CERT_ISSUER_URL.read(env));
        let token_path = take(TOKEN_PATH.read(env));

        let ca_cert_path = match CA_CERT.read(env) {
            Ok(path) => path,
            Err(error) => {
                note(error.to_string());
                None
            }
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
            cert_issuer_url,
            token_path,
            ca_cert_path,
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
        assert_eq!(s.cert_issuer_url, "https://cert-issuer:8443");
        assert_eq!(s.token_path, DEFAULT_TOKEN_PATH);
        assert_eq!(s.ca_cert_path, None);
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
        assert_eq!(s.ca_cert_path.as_deref(), Some("/ca/ca.pem"));
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
}
