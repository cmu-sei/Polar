//! The environment the crypto actor reads, each variable declared once.
//!
//! The actor derives its own identity from the client certificate the pod was issued, and
//! reads the pod's service-account token when it has to ask for an escrow keypair. The
//! certificate path is the same `TLS_CLIENT_CERT` the Cassini client reads, declared once by
//! `cassini-client`; this crate declares only what is its own.

use cassini_client::environment::TLS_CLIENT_CERT;
use polar_env::{Defaulted, Described, Env, Section};

/// Where Kubernetes mounts the pod's service-account token.
pub const KUBERNETES_SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

pub const KUBERNETES_SA_TOKEN: Defaulted<String> = Defaulted::text(
    "KUBERNETES_SA_TOKEN",
    "path to the pod's Kubernetes service-account token, re-read each time one is needed to ask for an escrow keypair",
    KUBERNETES_SA_TOKEN_PATH,
);

/// Beside the certificates, in the volume the init container and the workload already share.
pub const DEFAULT_ESCROW_KEY_HANDLE_PATH: &str = "/etc/tls/certs/escrow-key-handle";

pub const ESCROW_KEY_HANDLE: Defaulted<String> = Defaulted::text(
    "POLAR_ESCROW_KEY_HANDLE",
    "where the init container leaves the number of this agent's escrow key in the kernel keyring, for the workload to adopt; no file means the workload holds the key itself",
    DEFAULT_ESCROW_KEY_HANDLE_PATH,
);

static VARIABLES: &[&dyn Described] = &[&KUBERNETES_SA_TOKEN, &ESCROW_KEY_HANDLE];

/// What the crypto actor reads beyond the Cassini client's own variables. Applies to every
/// agent with the encryption capability, which always has the Cassini client too.
pub static SECTION: Section = Section {
    name: "crypto",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// Everything the crypto actor is configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoSettings {
    /// The certificate this agent was issued, whose identity the actor takes as its own.
    pub own_certificate_path: String,
    /// The service-account token, read afresh each time it is needed.
    pub service_account_token_path: String,
    /// Where the key number the init container left is looked for, and written.
    pub escrow_key_handle_path: String,
}

impl CryptoSettings {
    /// Reads from `env`, naming every variable that is missing or malformed.
    pub fn from_env(env: &dyn Env) -> Result<Self, String> {
        let mut problems = Vec::new();
        let mut take = |read: Result<String, polar_env::EnvError>| match read {
            Ok(value) => value,
            Err(error) => {
                problems.push(error.to_string());
                String::new()
            }
        };
        let own_certificate_path = take(TLS_CLIENT_CERT.read(env));
        let service_account_token_path = take(KUBERNETES_SA_TOKEN.read(env));
        let escrow_key_handle_path = take(ESCROW_KEY_HANDLE.read(env));
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        Ok(Self {
            own_certificate_path,
            service_account_token_path,
            escrow_key_handle_path,
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

    #[test]
    fn the_token_path_defaults_to_where_kubernetes_mounts_it() {
        let s = CryptoSettings::from_env(&env(&[("TLS_CLIENT_CERT", "/c/cert.pem")])).unwrap();
        assert_eq!(s.own_certificate_path, "/c/cert.pem");
        assert_eq!(s.service_account_token_path, KUBERNETES_SA_TOKEN_PATH);
    }

    #[test]
    fn the_key_handle_is_beside_the_certificates_unless_the_deployment_says() {
        let s = CryptoSettings::from_env(&env(&[("TLS_CLIENT_CERT", "/c/cert.pem")])).unwrap();
        assert_eq!(s.escrow_key_handle_path, DEFAULT_ESCROW_KEY_HANDLE_PATH);
        let s = CryptoSettings::from_env(&env(&[
            ("TLS_CLIENT_CERT", "/c/cert.pem"),
            ("POLAR_ESCROW_KEY_HANDLE", "/run/handle"),
        ]))
        .unwrap();
        assert_eq!(s.escrow_key_handle_path, "/run/handle");
    }

    #[test]
    fn the_deployment_may_say_where_the_token_is() {
        let s = CryptoSettings::from_env(&env(&[
            ("TLS_CLIENT_CERT", "/c/cert.pem"),
            ("KUBERNETES_SA_TOKEN", "/workspace/token"),
        ]))
        .unwrap();
        assert_eq!(s.service_account_token_path, "/workspace/token");
    }

    #[test]
    fn a_missing_certificate_path_is_named_with_what_it_is_for() {
        let error = CryptoSettings::from_env(&env(&[])).unwrap_err();
        assert!(error.contains("TLS_CLIENT_CERT"), "{error}");
        assert!(error.contains("certificate"), "{error}");
    }

    #[test]
    fn the_section_declares_only_what_is_the_crates_own() {
        let names: Vec<_> = SECTION.describe().iter().map(|v| v.name).collect();
        assert_eq!(
            names,
            ["KUBERNETES_SA_TOKEN", "POLAR_ESCROW_KEY_HANDLE"],
            "TLS_CLIENT_CERT is the client's"
        );
        assert!(
            SECTION.check(&env(&[])).is_empty(),
            "nothing in it is required"
        );
    }
}
