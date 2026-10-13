//! The floor: who may ask cert-issuer for a certificate with nothing but a service-account token.
//!
//! Everyone else is vouched for. A caller whose verified mTLS identity is on `internal_callers` (the credentials
//! agent) has applied its own controls to the request and states the certificate type and names itself. A caller
//! with only a token is limited to the **bootstrap identities**: the few workloads that cannot be issued through
//! the credentials agent because they are what it needs to run. Each may have one certificate type and only the
//! names written down for it. Nothing else a token holder asks for is signed.
//!
//! This is the capability floor of `docs/architecture/cert-issuer-isolation.md`: what must work when the
//! credentials agent is not there. Signed policy, once cert-issuer hosts the authority, sits above it.
//!
//! The bootstrap workload is written as the issuer's own identity claim for it (for Kubernetes,
//! `system:serviceaccount:<namespace>:<name>`), so no cluster domain appears here; the names are the exact DNS
//! names a server certificate carries.

use cert_issuer_common::CertType;
use serde::{Deserialize, Serialize};

/// A workload that may be issued with only its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapIdentity {
    /// The workload as its token names it, exactly (the issuer's workload identity claim).
    pub workload: String,
    /// The one certificate type it may ask for.
    pub cert_type: CertType,
    /// The extra names its certificate may carry, exactly; empty for a client certificate that carries only
    /// its own identity.
    #[serde(default)]
    pub names: Vec<String>,
}

/// Why a token-only request is not signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The token's workload is not a bootstrap identity: it must go through the credentials agent.
    NotBootstrap,
    /// It is, but asked for a certificate type other than its own.
    WrongType { allowed: CertType },
    /// It is, but asked for a name that is not on its list (a permission or capability is never on one).
    NameNotAllowed(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotBootstrap => write!(
                f,
                "this workload must obtain its certificate through the credentials agent"
            ),
            Refusal::WrongType { allowed } => {
                write!(
                    f,
                    "this workload may ask only for a {allowed:?} certificate"
                )
            }
            Refusal::NameNotAllowed(name) => {
                write!(
                    f,
                    "this workload may not have the name `{name}` in its certificate"
                )
            }
        }
    }
}

/// Whether a request carrying only a token for `workload` may be signed.
pub fn admit_token_only(
    floor: &[BootstrapIdentity],
    workload: &str,
    cert_type: &CertType,
    extra_sans: &[String],
) -> Result<(), Refusal> {
    let Some(entry) = floor.iter().find(|b| b.workload == workload) else {
        return Err(Refusal::NotBootstrap);
    };
    if &entry.cert_type != cert_type {
        return Err(Refusal::WrongType {
            allowed: entry.cert_type.clone(),
        });
    }
    for name in extra_sans {
        if !entry.names.iter().any(|allowed| allowed == name) {
            return Err(Refusal::NameNotAllowed(name.clone()));
        }
    }
    Ok(())
}

/// Whether a bootstrap entry is well formed: a name that is a permission, a capability, a wildcard or empty
/// is a mistake in the configuration, caught when it loads.
pub fn problems(floor: &[BootstrapIdentity]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in floor {
        if entry.workload.trim().is_empty() {
            problems.push("a bootstrap identity has an empty workload".to_string());
        } else if !seen.insert(entry.workload.as_str()) {
            problems.push(format!(
                "bootstrap identity `{}` is listed more than once",
                entry.workload
            ));
        }
        for name in &entry.names {
            let lower = name.trim().to_ascii_lowercase();
            if name.trim().is_empty() {
                problems.push(format!("`{}` has an empty name", entry.workload));
            } else if lower.starts_with("p:") || lower.starts_with("c:") {
                problems.push(format!(
                    "`{}` lists `{name}`: a permission or capability is never a bootstrap name",
                    entry.workload
                ));
            } else if name.contains('*') {
                problems.push(format!("`{}` lists the wildcard `{name}`", entry.workload));
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor() -> Vec<BootstrapIdentity> {
        vec![
            BootstrapIdentity {
                workload: "system:serviceaccount:polar:credentials-agent".to_string(),
                cert_type: CertType::Client,
                names: vec![],
            },
            BootstrapIdentity {
                workload: "system:serviceaccount:polar:cassini".to_string(),
                cert_type: CertType::Server,
                names: vec!["cassini-ip-svc.polar.svc.cluster.local".to_string()],
            },
        ]
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_bootstrap_identity_gets_what_is_written_down_for_it() {
        let f = floor();
        assert_eq!(
            admit_token_only(
                &f,
                "system:serviceaccount:polar:credentials-agent",
                &CertType::Client,
                &[]
            ),
            Ok(())
        );
        assert_eq!(
            admit_token_only(
                &f,
                "system:serviceaccount:polar:cassini",
                &CertType::Server,
                &names(&["cassini-ip-svc.polar.svc.cluster.local"])
            ),
            Ok(())
        );
    }

    #[test]
    fn any_other_workload_must_go_through_the_credentials_agent() {
        assert_eq!(
            admit_token_only(
                &floor(),
                "system:serviceaccount:polar:git-observer",
                &CertType::Client,
                &[]
            ),
            Err(Refusal::NotBootstrap)
        );
    }

    #[test]
    fn the_workload_is_matched_exactly_and_not_by_prefix_or_case() {
        let f = floor();
        for workload in [
            "system:serviceaccount:polar:cassini-evil",
            "system:serviceaccount:other:cassini",
            "System:ServiceAccount:polar:cassini",
            "cassini",
            "",
        ] {
            assert_eq!(
                admit_token_only(
                    &f,
                    workload,
                    &CertType::Server,
                    &names(&["cassini-ip-svc.polar.svc.cluster.local"])
                ),
                Err(Refusal::NotBootstrap),
                "{workload}"
            );
        }
    }

    #[test]
    fn a_bootstrap_identity_may_not_change_its_type() {
        let f = floor();
        assert_eq!(
            admit_token_only(
                &f,
                "system:serviceaccount:polar:cassini",
                &CertType::Client,
                &[]
            ),
            Err(Refusal::WrongType {
                allowed: CertType::Server
            })
        );
        assert_eq!(
            admit_token_only(
                &f,
                "system:serviceaccount:polar:credentials-agent",
                &CertType::Server,
                &[]
            ),
            Err(Refusal::WrongType {
                allowed: CertType::Client
            })
        );
        assert_eq!(
            admit_token_only(
                &f,
                "system:serviceaccount:polar:credentials-agent",
                &CertType::Service,
                &[]
            ),
            Err(Refusal::WrongType {
                allowed: CertType::Client
            })
        );
    }

    #[test]
    fn a_bootstrap_identity_may_not_add_a_name_that_is_not_its_own() {
        let f = floor();
        // Someone else's name, a permission, a capability, a wildcard: none of them is on the list.
        for extra in [
            "cert-issuer.polar.svc.cluster.local",
            "p:u2s:polar-db-svc.polar-graph.svc.cluster.local",
            "c:provision-account",
            "*.polar.svc.cluster.local",
            "CASSINI-IP-SVC.POLAR.SVC.CLUSTER.LOCAL",
        ] {
            assert_eq!(
                admit_token_only(
                    &f,
                    "system:serviceaccount:polar:cassini",
                    &CertType::Server,
                    &names(&[extra])
                ),
                Err(Refusal::NameNotAllowed(extra.to_string())),
                "{extra}"
            );
        }
        // And a client certificate with nothing listed takes no extras at all.
        assert_eq!(
            admit_token_only(
                &f,
                "system:serviceaccount:polar:credentials-agent",
                &CertType::Client,
                &names(&["cassini-ip-svc.polar.svc.cluster.local"])
            ),
            Err(Refusal::NameNotAllowed(
                "cassini-ip-svc.polar.svc.cluster.local".to_string()
            ))
        );
    }

    #[test]
    fn an_empty_floor_admits_nobody() {
        assert_eq!(
            admit_token_only(
                &[],
                "system:serviceaccount:polar:cassini",
                &CertType::Server,
                &[]
            ),
            Err(Refusal::NotBootstrap)
        );
    }

    #[test]
    fn a_floor_that_is_wrong_is_said_so_when_it_loads() {
        assert!(problems(&floor()).is_empty());
        let bad = vec![
            BootstrapIdentity {
                workload: " ".into(),
                cert_type: CertType::Client,
                names: vec![],
            },
            BootstrapIdentity {
                workload: "a".into(),
                cert_type: CertType::Server,
                names: names(&["p:s2s:x", "c:grant", "*.x", ""]),
            },
            BootstrapIdentity {
                workload: "a".into(),
                cert_type: CertType::Client,
                names: vec![],
            },
        ];
        let p = problems(&bad);
        assert!(p.iter().any(|m| m.contains("empty workload")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("more than once")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("`p:s2s:x`")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("`c:grant`")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("wildcard")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("empty name")), "{p:?}");
    }

    #[test]
    fn the_configuration_form_reads_and_refuses_what_it_does_not_know() {
        let ok: BootstrapIdentity = serde_json::from_str(
            r#"{"workload": "system:serviceaccount:polar:cassini", "cert_type": "SERVER", "names": ["a.b"]}"#,
        )
        .unwrap();
        assert_eq!(ok.cert_type, CertType::Server);
        let no_names: BootstrapIdentity =
            serde_json::from_str(r#"{"workload": "w", "cert_type": "CLIENT"}"#).unwrap();
        assert!(no_names.names.is_empty());
        assert!(
            serde_json::from_str::<BootstrapIdentity>(
                r#"{"workload": "w", "cert_type": "CLIENT", "extra": 1}"#
            )
            .is_err()
        );
    }
}
