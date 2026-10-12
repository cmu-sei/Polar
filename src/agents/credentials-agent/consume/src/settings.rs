//! What the credentials agent consumer reads from its configuration, declared once.
//!
//! These were read with `std::env::var` where they were used, so nothing could list them, check
//! them before the agent started, or say what they were for. Declared here they are in the
//! agent's scaffold and judged with everything else. The mTLS identity (`TLS_CLIENT_CERT`,
//! `TLS_CLIENT_KEY`) is the Cassini client's own declaration, read from the same snapshot.

use crate::account::DEFAULT_BOOTSTRAP_IDENTITY;
use crate::credential::KUBERNETES_ISSUERS_DEFAULT;
use crate::escrow::CREDENTIALS_ESCROW_KEY_PATH_DEFAULT;
use cassini_client::environment::{TLS_CLIENT_CERT, TLS_CLIENT_KEY};
use polar_env::{Config, ConfigError, Defaulted, Described, Env, Optional, Required, Section};

pub const CREDENTIALS_EVENT_LOG_PATH_DEFAULT: &str = "/var/lib/credentials-agent/events.jsonl";

pub static CERT_ISSUER_URL: Required<String> = Required::text(
    "CREDENTIALS_AGENT_CERT_ISSUER_URL",
    "the address of cert-issuer, which signs the certificates this agent issues",
);

pub static CA_CERT: Optional<String> = Optional::text(
    "CREDENTIALS_AGENT_CA_CERT",
    "path to the Polar CA certificate (PEM) to verify cert-issuer against from the first request; \
     unset, the CA is fetched once from cert-issuer and pinned (the degraded self-bootstrap mode); \
     a path that is set but unreadable stops the agent",
);

pub static EVENT_LOG_PATH: Defaulted<String> = Defaulted::text(
    "CREDENTIALS_EVENT_LOG_PATH",
    "the ledger's event log, the source of truth; keep it on a persistent volume",
    CREDENTIALS_EVENT_LOG_PATH_DEFAULT,
);

pub static ESCROW_KEY_PATH: Defaulted<String> = Defaulted::text(
    "CREDENTIALS_ESCROW_KEY_PATH",
    "where the escrow keypair is kept; generated on first start, so keep it on a persistent volume",
    CREDENTIALS_ESCROW_KEY_PATH_DEFAULT,
);

pub static BOOTSTRAP_IDENTITY: Defaulted<String> = Defaulted::text(
    "CREDENTIALS_AGENT_BOOTSTRAP_IDENTITY",
    "the one Kubernetes identity (the sub claim of a service account token) allowed to bootstrap",
    DEFAULT_BOOTSTRAP_IDENTITY,
);

pub static KUBERNETES_ISSUERS: Defaulted<Vec<String>> = Defaulted::list(
    "CREDENTIALS_AGENT_KUBERNETES_ISSUERS",
    "the token issuers accepted as Kubernetes service account credentials, comma-separated",
    KUBERNETES_ISSUERS_DEFAULT,
);

pub static INTROSPECTION_ENDPOINT: Optional<String> = Optional::text(
    "CREDENTIALS_AGENT_INTROSPECTION_ENDPOINT",
    "the identity provider's token introspection endpoint; with the client id and secret, it turns on \
     checking that an identity provider token is still active",
);

pub static INTROSPECTION_CLIENT_ID: Optional<String> = Optional::text(
    "CREDENTIALS_AGENT_INTROSPECTION_CLIENT_ID",
    "this agent's client id at the identity provider, for introspection",
);

pub static INTROSPECTION_CLIENT_SECRET: Optional<String> = Optional::text(
    "CREDENTIALS_AGENT_INTROSPECTION_CLIENT_SECRET",
    "this agent's client secret at the identity provider, for introspection",
)
.secret();

static VARIABLES: &[&dyn Described] = &[
    &CERT_ISSUER_URL,
    &CA_CERT,
    &EVENT_LOG_PATH,
    &ESCROW_KEY_PATH,
    &BOOTSTRAP_IDENTITY,
    &KUBERNETES_ISSUERS,
    &INTROSPECTION_ENDPOINT,
    &INTROSPECTION_CLIENT_ID,
    &INTROSPECTION_CLIENT_SECRET,
];

/// The section the credentials agent consumer registers.
pub static SECTION: Section = Section {
    name: "credentials-agent",
    vars: VARIABLES,
    relations: introspection_is_all_or_none,
};

/// Introspection is on when its three settings are all given and off when none is. One or two of
/// them is a mistake that would otherwise switch the check off without a word, and a token the
/// identity provider has revoked would pass.
fn introspection_is_all_or_none(env: &dyn Env) -> Vec<String> {
    let names = [
        INTROSPECTION_ENDPOINT.name(),
        INTROSPECTION_CLIENT_ID.name(),
        INTROSPECTION_CLIENT_SECRET.name(),
    ];
    let set = |name: &str| env.var(name).is_some_and(|v| !v.trim().is_empty());
    let missing: Vec<&str> = names.iter().copied().filter(|n| !set(n)).collect();
    if missing.len() == names.len() || missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "introspection needs all three of {} or none; {} not set",
        names.join(", "),
        missing.join(", ")
    )]
}

/// How to ask the identity provider whether a token is still active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrospectionSettings {
    pub endpoint: String,
    pub client_id: String,
    pub client_secret: String,
}

pub struct ConsumerSettings {
    pub cert_issuer_url: String,
    pub ca_cert: Option<String>,
    pub event_log_path: String,
    pub escrow_key_path: String,
    pub bootstrap_identity: String,
    pub kubernetes_issuers: Vec<String>,
    /// Introspection is optional at the deployment level: a cluster that never has identity
    /// provider credentials does not need it. One that arrives without it is refused by the
    /// handler, never let through unchecked.
    pub introspection: Option<IntrospectionSettings>,
    /// The agent's own mTLS identity, which it presents to cert-issuer as well as to Cassini.
    pub own_cert_path: String,
    pub own_key_path: String,
}

impl ConsumerSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        let introspection = match (
            config.read(&INTROSPECTION_ENDPOINT)?,
            config.read(&INTROSPECTION_CLIENT_ID)?,
            config.read(&INTROSPECTION_CLIENT_SECRET)?,
        ) {
            (Some(endpoint), Some(client_id), Some(client_secret)) => Some(IntrospectionSettings {
                endpoint,
                client_id,
                client_secret,
            }),
            _ => None,
        };
        Ok(Self {
            cert_issuer_url: config.read(&CERT_ISSUER_URL)?,
            ca_cert: config.read(&CA_CERT)?,
            event_log_path: config.read(&EVENT_LOG_PATH)?,
            escrow_key_path: config.read(&ESCROW_KEY_PATH)?,
            bootstrap_identity: config.read(&BOOTSTRAP_IDENTITY)?,
            kubernetes_issuers: config.read(&KUBERNETES_ISSUERS)?,
            introspection,
            own_cert_path: config.read(&TLS_CLIENT_CERT)?,
            own_key_path: config.read(&TLS_CLIENT_KEY)?,
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
        Config::builder(&env)
            .register(&cassini_client::environment::SECTION)
            .register(&SECTION)
            .build()
    }

    const MINIMUM: &[(&str, &str)] = &[
        ("CREDENTIALS_AGENT_CERT_ISSUER_URL", "https://cert-issuer"),
        ("TLS_CLIENT_CERT", "/c/cert.pem"),
        ("TLS_CLIENT_KEY", "/c/key.pem"),
        ("TLS_CA_CERT", "/c/ca.pem"),
        ("BROKER_ADDR", "cassini:8080"),
        ("CASSINI_SERVER_NAME", "cassini"),
    ];

    fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        MINIMUM.iter().chain(extra).copied().collect()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn the_cert_issuer_address_is_the_only_thing_it_must_be_given() {
        let settings = ConsumerSettings::read(&config(MINIMUM)).unwrap();
        assert_eq!(settings.cert_issuer_url, "https://cert-issuer");
        assert_eq!(settings.ca_cert, None);
        assert_eq!(settings.event_log_path, CREDENTIALS_EVENT_LOG_PATH_DEFAULT);
        assert_eq!(
            settings.escrow_key_path,
            CREDENTIALS_ESCROW_KEY_PATH_DEFAULT
        );
        assert_eq!(settings.bootstrap_identity, DEFAULT_BOOTSTRAP_IDENTITY);
        assert_eq!(
            settings.kubernetes_issuers,
            vec![KUBERNETES_ISSUERS_DEFAULT]
        );
        assert_eq!(settings.introspection, None);
        assert_eq!(settings.own_cert_path, "/c/cert.pem");
        assert_eq!(settings.own_key_path, "/c/key.pem");
    }

    #[test]
    fn a_missing_cert_issuer_address_is_named_with_what_it_is_for() {
        let error = config(&[]).check().join(" | ");
        assert!(
            error.contains("CREDENTIALS_AGENT_CERT_ISSUER_URL") && error.contains("cert-issuer"),
            "{error}"
        );
    }

    #[test]
    fn a_blank_value_is_no_value() {
        let settings = ConsumerSettings::read(&config(&with(&[
            ("CREDENTIALS_AGENT_CA_CERT", ""),
            ("CREDENTIALS_AGENT_BOOTSTRAP_IDENTITY", "  "),
        ])))
        .unwrap();
        assert_eq!(settings.ca_cert, None);
        assert_eq!(settings.bootstrap_identity, DEFAULT_BOOTSTRAP_IDENTITY);
    }

    #[test]
    fn what_is_set_overrides_the_defaults() {
        let settings = ConsumerSettings::read(&config(&with(&[
            ("CREDENTIALS_AGENT_CA_CERT", "/etc/polar-ca/ca.pem"),
            ("CREDENTIALS_EVENT_LOG_PATH", "/data/events.jsonl"),
            ("CREDENTIALS_ESCROW_KEY_PATH", "/data/escrow.pem"),
            (
                "CREDENTIALS_AGENT_BOOTSTRAP_IDENTITY",
                "system:serviceaccount:x:y",
            ),
            (
                "CREDENTIALS_AGENT_KUBERNETES_ISSUERS",
                "https://a.example.com, https://b.example.com",
            ),
        ])))
        .unwrap();
        assert_eq!(settings.ca_cert.as_deref(), Some("/etc/polar-ca/ca.pem"));
        assert_eq!(settings.event_log_path, "/data/events.jsonl");
        assert_eq!(settings.escrow_key_path, "/data/escrow.pem");
        assert_eq!(settings.bootstrap_identity, "system:serviceaccount:x:y");
        assert_eq!(
            settings.kubernetes_issuers,
            vec!["https://a.example.com", "https://b.example.com"]
        );
    }

    #[test]
    fn introspection_is_off_without_its_settings() {
        let all = ConsumerSettings::read(&config(&with(&[
            (
                "CREDENTIALS_AGENT_INTROSPECTION_ENDPOINT",
                "https://idp/introspect",
            ),
            ("CREDENTIALS_AGENT_INTROSPECTION_CLIENT_ID", "agent"),
            ("CREDENTIALS_AGENT_INTROSPECTION_CLIENT_SECRET", "s3cret"),
        ])))
        .unwrap();
        assert_eq!(
            all.introspection,
            Some(IntrospectionSettings {
                endpoint: "https://idp/introspect".into(),
                client_id: "agent".into(),
                client_secret: "s3cret".into(),
            })
        );
    }

    #[test]
    fn a_partial_introspection_setting_is_a_problem_that_names_what_is_missing() {
        let problems = config(&with(&[
            (
                "CREDENTIALS_AGENT_INTROSPECTION_ENDPOINT",
                "https://idp/introspect",
            ),
            ("CREDENTIALS_AGENT_INTROSPECTION_CLIENT_ID", "agent"),
        ]))
        .check();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("CREDENTIALS_AGENT_INTROSPECTION_CLIENT_SECRET")
                && problems[0].contains("all three"),
            "{problems:?}"
        );
        // none, or all three, are fine
        assert!(config(MINIMUM).check().is_empty());
        let all = with(&[
            ("CREDENTIALS_AGENT_INTROSPECTION_ENDPOINT", "e"),
            ("CREDENTIALS_AGENT_INTROSPECTION_CLIENT_ID", "i"),
            ("CREDENTIALS_AGENT_INTROSPECTION_CLIENT_SECRET", "s"),
        ]);
        assert!(config(&all).check().is_empty());
    }

    #[test]
    fn the_introspection_secret_is_never_shown_in_a_report() {
        let c = config(&with(&[(
            "CREDENTIALS_AGENT_INTROSPECTION_CLIENT_SECRET",
            "s3cret",
        )]));
        let report = c.report();
        let secret = report
            .iter()
            .find(|r| r.name == "CREDENTIALS_AGENT_INTROSPECTION_CLIENT_SECRET")
            .unwrap();
        assert_eq!(secret.value.as_deref(), Some("<hidden>"));
    }
}
