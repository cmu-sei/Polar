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

//! What cert-issuer reads from its configuration, declared once (docs/architecture/cert-issuer-isolation.md, "1b in
//! detail"). Each scalar of the service's configuration is one variable; the issuers, the floor and the lifetime
//! overrides are each one JSON value, in the shapes [`crate::config`] reads. The section checks the whole: it builds the
//! configuration and runs [`ServiceConfig::validate`] on it, so every refusal `validate` makes is a problem found
//! before anything starts, and by `--check`.

use crate::config::{CaConfig, CaProvisioning, IssuerConfig, MtlsConfig, ServiceConfig};
use crate::floor::BootstrapIdentity;
use polar_env::{Defaulted, Described, Env, Optional, Required, Section};
use std::collections::HashMap;
use std::time::Duration;

pub static BIND_ADDR: Defaulted<String> = Defaulted::text(
    "CERT_ISSUER_BIND_ADDR",
    "the address and port cert-issuer listens on, over TLS",
    "0.0.0.0:8443",
);

pub static ISSUERS: Required<String> = Required::text(
    "CERT_ISSUER_ISSUERS",
    "every token issuer cert-issuer trusts, as a JSON list of {issuer, audience, jwks_uri, workload_identity_claim, \
     instance_binding_claim, identity_normalization, identity_scope_claim, allowed_algorithms, jwks_cache_ttl_min, \
     jwks_cache_ttl_max}",
);

pub static CA_CERT_PATH: Required<String> = Required::text(
    "CERT_ISSUER_CA_CERT_PATH",
    "path to the CA certificate (PEM)",
);

pub static CA_KEY_PATH: Required<String> = Required::text(
    "CERT_ISSUER_CA_KEY_PATH",
    "path to the CA private key (PEM)",
);

pub static CA_PROVISIONING: Defaulted<String> = Defaulted::text(
    "CERT_ISSUER_CA_PROVISIONING",
    "where the CA comes from: `provisioned` (made ahead of time by an operator and only loaded; missing is an error) \
     or `self_bootstrap` (made by cert-issuer on its first start, on its own volume)",
    "self_bootstrap",
);

pub static DEFAULT_LIFETIME_SECS: Required<u64> = Required::seconds(
    "CERT_ISSUER_DEFAULT_LIFETIME_SECS",
    "how long a workload's client certificate lives",
);

pub static SERVER_LIFETIME_SECS: Required<u64> = Required::seconds(
    "CERT_ISSUER_SERVER_LIFETIME_SECS",
    "how long a server certificate lives",
);

pub static USER_LIFETIME_SECS: Required<u64> = Required::seconds(
    "CERT_ISSUER_USER_LIFETIME_SECS",
    "how long an account's certificate lives, the one a person holds",
);

pub static SERVICE_LIFETIME_SECS: Required<u64> = Required::seconds(
    "CERT_ISSUER_SERVICE_LIFETIME_SECS",
    "how long an agent's escrow certificate lives, its durable encryption identity that outlives its pods",
);

pub static IDENTITY_LIFETIME_OVERRIDES: Optional<String> = Optional::text(
    "CERT_ISSUER_IDENTITY_LIFETIME_OVERRIDES",
    "lifetimes for particular identities, in seconds, as a JSON object {\"<identity>\": <seconds>}; they take \
     precedence over the lifetimes above",
);

pub static SERVER_IDENTITY: Required<String> = Required::text(
    "CERT_ISSUER_SERVER_IDENTITY",
    "the DNS name in cert-issuer's own TLS server certificate: the name its callers connect to",
);

pub static INTERNAL_CALLERS: Optional<Vec<String>> = Optional::list(
    "CERT_ISSUER_INTERNAL_CALLERS",
    "the identities, by their verified mTLS certificate, that may reach /internal/issue and vouch for a request: the \
     credentials agent's, and nothing else. Unset, nothing may",
);

pub static BOOTSTRAP: Optional<String> = Optional::text(
    "CERT_ISSUER_BOOTSTRAP",
    "the floor (docs/architecture/cert-issuer-isolation.md): who may be signed what on a token alone, as a JSON list of \
     {workload, cert_type, names, vouched_only, publish, subscribe, groups}. Unset, the floor is open: any holder of a \
     valid token is signed what it asks for, and cert-issuer says so at every start. Set, even to [], it is enforced",
);

pub static OWN_TOKEN: Required<String> = Required::text(
    "CERT_ISSUER_OWN_TOKEN",
    "path to cert-issuer's own service account token, with the audience its Kubernetes issuer expects: it is signed its \
     own client certificate on it, through its own floor, as every bootstrap service is",
);

pub static RECORD_PATH: Required<String> = Required::text(
    "CERT_ISSUER_RECORD_PATH",
    "path to the record of every certificate cert-issuer signs, one JSON line each, which the credentials agent asks for \
     (docs/architecture/cert-issuer-isolation.md, \"Step 2\"). It must be persistent storage: a certificate that cannot \
     be recorded is not returned",
);

pub static KUBERNETES_CA_CERT: Optional<String> = Optional::text(
    "KUBERNETES_CA_CERT",
    "path to the Kubernetes API server's CA certificate, trusted beside the system's when an issuer's keys are \
     fetched from it",
);

pub static KUBERNETES_SA_TOKEN: Defaulted<String> = Defaulted::text(
    "KUBERNETES_SA_TOKEN",
    "path to the token presented to the Kubernetes API server when an issuer's keys are fetched from it; read again at \
     every fetch, since Kubernetes rotates it",
    "/var/run/secrets/kubernetes.io/serviceaccount/token",
);

static VARIABLES: &[&dyn Described] = &[
    &BIND_ADDR,
    &ISSUERS,
    &CA_CERT_PATH,
    &CA_KEY_PATH,
    &CA_PROVISIONING,
    &DEFAULT_LIFETIME_SECS,
    &SERVER_LIFETIME_SECS,
    &USER_LIFETIME_SECS,
    &SERVICE_LIFETIME_SECS,
    &IDENTITY_LIFETIME_OVERRIDES,
    &SERVER_IDENTITY,
    &INTERNAL_CALLERS,
    &BOOTSTRAP,
    &OWN_TOKEN,
    &RECORD_PATH,
    &KUBERNETES_CA_CERT,
    &KUBERNETES_SA_TOKEN,
];

/// The section cert-issuer registers.
pub static SECTION: Section = Section {
    name: "cert-issuer",
    vars: VARIABLES,
    relations,
};

/// What is wrong with the configuration as a whole: what [`Settings::from_env`] refuses, which includes everything
/// [`ServiceConfig::validate`] does. A variable that does not read on its own is reported by its own check, so only the
/// problems found once every variable reads are added here.
fn relations(env: &dyn Env) -> Vec<String> {
    let each_reads = VARIABLES.iter().all(|v| v.validate(env).is_ok());
    if !each_reads {
        return Vec::new();
    }
    match Settings::from_env(env) {
        Ok(_) => Vec::new(),
        Err(problems) => problems,
    }
}

/// cert-issuer's settings: the service's configuration, already validated, and where its own token is.
#[derive(Debug, Clone)]
pub struct Settings {
    pub service: ServiceConfig,
    pub own_token: String,
    /// Where the record of every certificate it signs is kept.
    pub record_path: String,
    /// What fetching an issuer's keys from the Kubernetes API server needs.
    pub kubernetes_ca_cert: Option<String>,
    pub kubernetes_sa_token: String,
}

impl Settings {
    /// The settings `env` gives, or every problem with them.
    pub fn from_env(env: &dyn Env) -> Result<Self, Vec<String>> {
        let mut problems = Vec::new();
        macro_rules! take {
            ($var:expr, $fallback:expr) => {
                match $var.read(env) {
                    Ok(value) => value,
                    Err(e) => {
                        problems.push(e.to_string());
                        $fallback
                    }
                }
            };
        }
        let bind_addr = take!(BIND_ADDR, String::new());
        let issuers_text = take!(ISSUERS, String::new());
        let ca_cert_path = take!(CA_CERT_PATH, String::new());
        let ca_key_path = take!(CA_KEY_PATH, String::new());
        let provisioning_text = take!(CA_PROVISIONING, String::new());
        let default_lifetime = take!(DEFAULT_LIFETIME_SECS, 0);
        let server_lifetime = take!(SERVER_LIFETIME_SECS, 0);
        let user_lifetime = take!(USER_LIFETIME_SECS, 0);
        let service_lifetime = take!(SERVICE_LIFETIME_SECS, 0);
        let overrides_text = take!(IDENTITY_LIFETIME_OVERRIDES, None);
        let server_identity = take!(SERVER_IDENTITY, String::new());
        let internal_callers = take!(INTERNAL_CALLERS, None);
        let bootstrap_text = take!(BOOTSTRAP, None);
        let own_token = take!(OWN_TOKEN, String::new());
        let record_path = take!(RECORD_PATH, String::new());
        let kubernetes_ca_cert = take!(KUBERNETES_CA_CERT, None);
        let kubernetes_sa_token = take!(KUBERNETES_SA_TOKEN, String::new());

        let issuers: Vec<IssuerConfig> =
            json(ISSUERS.name(), &issuers_text, &mut problems).unwrap_or_default();
        let provisioning = match provisioning_text.trim() {
            "provisioned" => CaProvisioning::Provisioned,
            "self_bootstrap" => CaProvisioning::SelfBootstrap,
            other => {
                problems.push(format!(
                    "{}: `{other}` is neither `provisioned` nor `self_bootstrap`",
                    CA_PROVISIONING.name()
                ));
                CaProvisioning::default()
            }
        };
        let overrides: HashMap<String, u64> = overrides_text
            .and_then(|text| json(IDENTITY_LIFETIME_OVERRIDES.name(), &text, &mut problems))
            .unwrap_or_default();
        let bootstrap: Option<Vec<BootstrapIdentity>> =
            bootstrap_text.and_then(|text| json(BOOTSTRAP.name(), &text, &mut problems));
        if !problems.is_empty() {
            return Err(problems);
        }

        let service = ServiceConfig {
            bind_addr,
            issuers,
            ca: CaConfig {
                ca_cert_path,
                ca_key_path,
                default_lifetime: Duration::from_secs(default_lifetime),
                server_lifetime: Duration::from_secs(server_lifetime),
                user_lifetime: Duration::from_secs(user_lifetime),
                service_lifetime: Duration::from_secs(service_lifetime),
                identity_lifetime_overrides: overrides
                    .into_iter()
                    .map(|(identity, secs)| (identity, Duration::from_secs(secs)))
                    .collect(),
                provisioning,
            },
            mtls: MtlsConfig {
                server_identity,
                internal_callers: internal_callers.unwrap_or_default(),
                bootstrap,
            },
        };
        service.validate().map_err(|e| vec![e.to_string()])?;
        Ok(Self {
            service,
            own_token,
            record_path,
            kubernetes_ca_cert,
            kubernetes_sa_token,
        })
    }
}

/// `text` read as JSON, or `None` with the problem, named for `variable`, added to `problems`.
fn json<T: serde::de::DeserializeOwned>(
    variable: &str,
    text: &str,
    problems: &mut Vec<String>,
) -> Option<T> {
    match serde_json::from_str(text) {
        Ok(value) => Some(value),
        Err(e) => {
            problems.push(format!("{variable}: not the JSON it should be: {e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::CertType;
    use proptest::prelude::*;

    const ISSUER: &str = r#"[{"issuer": "https://kubernetes.default.svc", "audience": ["cert-issuer"],
        "jwks_uri": null, "workload_identity_claim": "sub", "instance_binding_claim": "kubernetes.io/pod/uid",
        "identity_normalization": {"strategy": "kubernetes_service_account", "cluster_domain": "cluster.local"},
        "identity_scope_claim": null, "allowed_algorithms": ["RS256"],
        "jwks_cache_ttl_min": {"secs": 60, "nanos": 0}, "jwks_cache_ttl_max": {"secs": 3600, "nanos": 0}}]"#;

    /// Every required variable, with sound values.
    fn whole() -> HashMap<String, String> {
        [
            ("CERT_ISSUER_ISSUERS", ISSUER),
            ("CERT_ISSUER_CA_CERT_PATH", "/home/polar/ca/ca.crt"),
            ("CERT_ISSUER_CA_KEY_PATH", "/home/polar/ca/ca.key"),
            ("CERT_ISSUER_DEFAULT_LIFETIME_SECS", "3600"),
            ("CERT_ISSUER_SERVER_LIFETIME_SECS", "7200"),
            ("CERT_ISSUER_USER_LIFETIME_SECS", "31536000"),
            ("CERT_ISSUER_SERVICE_LIFETIME_SECS", "15768000"),
            (
                "CERT_ISSUER_SERVER_IDENTITY",
                "cert-issuer.polar.svc.cluster.local",
            ),
            ("CERT_ISSUER_OWN_TOKEN", "/var/run/secrets/polar/token"),
            (
                "CERT_ISSUER_RECORD_PATH",
                "/var/lib/cert-issuer/issued.jsonl",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn with(changes: &[(&str, &str)]) -> HashMap<String, String> {
        let mut env = whole();
        for (k, v) in changes {
            env.insert(k.to_string(), v.to_string());
        }
        env
    }

    /// What the section reports, as `--check` and the start do.
    fn checked(env: &HashMap<String, String>) -> Vec<String> {
        polar_env::Config::builder(env)
            .register(&SECTION)
            .build()
            .check()
    }

    #[test]
    fn a_whole_configuration_reads_as_the_service_configuration_it_describes() {
        let s = Settings::from_env(&with(&[
            ("CERT_ISSUER_BIND_ADDR", "0.0.0.0:9443"),
            ("CERT_ISSUER_CA_PROVISIONING", "provisioned"),
            (
                "CERT_ISSUER_IDENTITY_LIFETIME_OVERRIDES",
                r#"{"cassini.polar.svc.cluster.local": 86400}"#,
            ),
            (
                "CERT_ISSUER_INTERNAL_CALLERS",
                "credentials-agent.polar.serviceaccount.cluster.local",
            ),
        ]))
        .unwrap();
        let c = &s.service;
        assert_eq!(c.bind_addr, "0.0.0.0:9443");
        assert_eq!(c.issuers.len(), 1);
        assert_eq!(c.issuers[0].issuer, "https://kubernetes.default.svc");
        assert_eq!(c.ca.ca_cert_path, "/home/polar/ca/ca.crt");
        assert_eq!(c.ca.ca_key_path, "/home/polar/ca/ca.key");
        assert_eq!(c.ca.provisioning, CaProvisioning::Provisioned);
        assert_eq!(c.ca.default_lifetime, Duration::from_secs(3600));
        assert_eq!(c.ca.server_lifetime, Duration::from_secs(7200));
        assert_eq!(c.ca.user_lifetime, Duration::from_secs(31_536_000));
        assert_eq!(c.ca.service_lifetime, Duration::from_secs(15_768_000));
        assert_eq!(
            c.ca.identity_lifetime_overrides,
            HashMap::from([(
                "cassini.polar.svc.cluster.local".to_string(),
                Duration::from_secs(86400)
            )])
        );
        assert_eq!(
            c.mtls.server_identity,
            "cert-issuer.polar.svc.cluster.local"
        );
        assert_eq!(
            c.mtls.internal_callers,
            vec!["credentials-agent.polar.serviceaccount.cluster.local".to_string()]
        );
        assert_eq!(s.own_token, "/var/run/secrets/polar/token");
        assert_eq!(s.record_path, "/var/lib/cert-issuer/issued.jsonl");
        assert!(checked(&whole()).is_empty(), "{:?}", checked(&whole()));
    }

    #[test]
    fn what_fetching_the_kubernetes_keys_needs_is_read_and_has_its_standard_defaults() {
        let s = Settings::from_env(&whole()).unwrap();
        assert_eq!(s.kubernetes_ca_cert, None);
        assert_eq!(
            s.kubernetes_sa_token,
            "/var/run/secrets/kubernetes.io/serviceaccount/token"
        );
        let s = Settings::from_env(&with(&[
            ("KUBERNETES_CA_CERT", "/k/ca.crt"),
            ("KUBERNETES_SA_TOKEN", "/k/token"),
        ]))
        .unwrap();
        assert_eq!(s.kubernetes_ca_cert.as_deref(), Some("/k/ca.crt"));
        assert_eq!(s.kubernetes_sa_token, "/k/token");
    }

    #[test]
    fn the_defaults_are_the_open_listener_address_a_self_made_ca_and_no_one_vouching() {
        let c = Settings::from_env(&whole()).unwrap().service;
        assert_eq!(c.bind_addr, "0.0.0.0:8443");
        assert_eq!(c.ca.provisioning, CaProvisioning::SelfBootstrap);
        assert!(c.ca.identity_lifetime_overrides.is_empty());
        assert!(c.mtls.internal_callers.is_empty());
    }

    #[test]
    fn the_floor_is_open_when_unset_and_enforced_when_set_even_to_nothing() {
        assert_eq!(
            Settings::from_env(&whole()).unwrap().service.mtls.bootstrap,
            None
        );
        assert_eq!(
            Settings::from_env(&with(&[("CERT_ISSUER_BOOTSTRAP", "[]")]))
                .unwrap()
                .service
                .mtls
                .bootstrap,
            Some(Vec::new())
        );
        let floor = r#"[{"workload": "system:serviceaccount:polar:cert-issuer", "cert_type": "CLIENT",
            "names": [], "groups": ["cassini-client"]}]"#;
        let read = Settings::from_env(&with(&[("CERT_ISSUER_BOOTSTRAP", floor)]))
            .unwrap()
            .service
            .mtls
            .bootstrap
            .unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].workload, "system:serviceaccount:polar:cert-issuer");
        assert_eq!(read[0].cert_type, CertType::Client);
        assert_eq!(read[0].groups, vec!["cassini-client".to_string()]);
    }

    #[test]
    fn a_json_value_that_does_not_read_is_a_problem_naming_its_variable() {
        for name in [
            "CERT_ISSUER_ISSUERS",
            "CERT_ISSUER_BOOTSTRAP",
            "CERT_ISSUER_IDENTITY_LIFETIME_OVERRIDES",
        ] {
            let env = with(&[(name, "{not json")]);
            let problems = Settings::from_env(&env).unwrap_err();
            assert!(
                problems.iter().any(|p| p.starts_with(name)),
                "{name}: {problems:?}"
            );
            assert!(checked(&env).iter().any(|p| p.starts_with(name)), "{name}");
        }
    }

    #[test]
    fn a_provisioning_that_is_neither_is_a_problem() {
        let env = with(&[("CERT_ISSUER_CA_PROVISIONING", "made-up")]);
        let problems = Settings::from_env(&env).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("CERT_ISSUER_CA_PROVISIONING")),
            "{problems:?}"
        );
        assert!(!checked(&env).is_empty());
    }

    #[test]
    fn what_validate_refuses_is_a_problem_found_before_anything_starts() {
        for (label, change) in [
            ("no issuers", ("CERT_ISSUER_ISSUERS", "[]")),
            ("no server identity", ("CERT_ISSUER_SERVER_IDENTITY", "")),
            (
                "a floor entry naming a permission",
                (
                    "CERT_ISSUER_BOOTSTRAP",
                    r#"[{"workload": "system:serviceaccount:polar:x", "cert_type": "CLIENT", "names": ["p:s2s:y"]}]"#,
                ),
            ),
        ] {
            let env = with(&[change]);
            assert!(Settings::from_env(&env).is_err(), "{label}");
            assert!(
                !checked(&env).is_empty(),
                "{label}: the section finds it too"
            );
        }
    }

    #[test]
    fn a_missing_required_variable_is_reported_once_by_its_own_check() {
        let mut env = whole();
        env.remove("CERT_ISSUER_OWN_TOKEN");
        let problems = checked(&env);
        assert_eq!(
            problems
                .iter()
                .filter(|p| p.contains("CERT_ISSUER_OWN_TOKEN"))
                .count(),
            1,
            "{problems:?}"
        );
        assert!(Settings::from_env(&env).is_err());
    }

    fn entry() -> impl Strategy<Value = BootstrapIdentity> {
        (
            "[a-z]{1,8}",
            prop_oneof![Just(CertType::Client), Just(CertType::Server)],
            proptest::collection::vec("[a-z]{1,6}\\.polar\\.svc", 0..3),
            any::<bool>(),
            proptest::collection::vec("[a-z]{1,6}", 0..3),
            proptest::collection::vec("[a-z]{1,6}", 0..3),
            proptest::collection::vec("[a-z]{1,6}", 0..2),
        )
            .prop_map(
                |(sa, cert_type, names, vouched_only, publish, subscribe, groups)| {
                    BootstrapIdentity {
                        workload: format!("system:serviceaccount:polar:{sa}"),
                        cert_type,
                        names,
                        vouched_only,
                        publish,
                        subscribe,
                        groups,
                    }
                },
            )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// A floor, the lifetime overrides and the internal callers, written as the variables the render writes, read
        /// back as themselves.
        #[test]
        fn the_structured_settings_read_back_as_they_were_written(
            floor in proptest::collection::vec(entry(), 0..4),
            overrides in proptest::collection::hash_map("[a-z]{1,8}\\.polar\\.svc", 1u64..100_000, 0..4),
            callers in proptest::collection::vec("[a-z]{1,8}\\.polar\\.serviceaccount", 1..3),
        ) {
            // A workload is listed once.
            let mut seen = std::collections::HashSet::new();
            let floor: Vec<BootstrapIdentity> =
                floor.into_iter().filter(|e| seen.insert(e.workload.clone())).collect();
            let env = with(&[
                ("CERT_ISSUER_BOOTSTRAP", &serde_json::to_string(&floor).unwrap()),
                ("CERT_ISSUER_IDENTITY_LIFETIME_OVERRIDES", &serde_json::to_string(&overrides).unwrap()),
                ("CERT_ISSUER_INTERNAL_CALLERS", &callers.join(",")),
            ]);
            let c = Settings::from_env(&env).unwrap().service;
            prop_assert_eq!(c.mtls.bootstrap, Some(floor));
            let read: HashMap<String, u64> = c
                .ca
                .identity_lifetime_overrides
                .iter()
                .map(|(k, v)| (k.clone(), v.as_secs()))
                .collect();
            prop_assert_eq!(read, overrides);
            prop_assert_eq!(c.mtls.internal_callers, callers);
        }
    }
}
