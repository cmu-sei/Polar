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
    /// True for a workload that is issued only through the credentials agent (Neo4j, the git server): its names
    /// are written down here so they are checked on the vouched path, but its token alone never gets it a
    /// certificate. False (the default) is a bootstrap identity.
    #[serde(default)]
    pub vouched_only: bool,
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
    let Some(entry) = floor
        .iter()
        .find(|b| b.workload == workload && !b.vouched_only)
    else {
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

/// Whether a request the credentials agent vouches for may be signed: its names are checked here too, so a
/// compromised or mistaken credentials agent cannot have a workload signed with a name that is not its own.
/// The permissions of a grant (`p:`) are the credentials agent's to state and are not names; every other extra
/// must be on the list of the workload's entry, for the type it asks for.
pub fn admit_vouched(
    floor: &[BootstrapIdentity],
    workload: &str,
    cert_type: &CertType,
    extra_sans: &[String],
) -> Result<(), Refusal> {
    let entry = floor
        .iter()
        .find(|b| b.workload == workload && &b.cert_type == cert_type);
    for name in extra_sans
        .iter()
        .filter(|n| !n.trim().to_ascii_lowercase().starts_with("p:"))
    {
        if !entry.is_some_and(|e| e.names.iter().any(|allowed| allowed == name)) {
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
                vouched_only: false,
            },
            BootstrapIdentity {
                workload: "system:serviceaccount:polar:cassini".to_string(),
                cert_type: CertType::Server,
                names: vec!["cassini-ip-svc.polar.svc.cluster.local".to_string()],
                vouched_only: false,
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

    fn floor_with_a_vouched_only_server() -> Vec<BootstrapIdentity> {
        let mut f = floor();
        f.push(BootstrapIdentity {
            workload: "system:serviceaccount:polar-graph:neo4j".to_string(),
            cert_type: CertType::Server,
            names: vec!["polar-db-svc.polar-graph.svc.cluster.local".to_string()],
            vouched_only: true,
        });
        f
    }

    #[test]
    fn a_vouched_only_workload_is_never_signed_on_its_token_alone() {
        assert_eq!(
            admit_token_only(
                &floor_with_a_vouched_only_server(),
                "system:serviceaccount:polar-graph:neo4j",
                &CertType::Server,
                &names(&["polar-db-svc.polar-graph.svc.cluster.local"])
            ),
            Err(Refusal::NotBootstrap)
        );
    }

    #[test]
    fn a_vouched_request_may_carry_the_names_written_down_for_its_workload() {
        assert_eq!(
            admit_vouched(
                &floor_with_a_vouched_only_server(),
                "system:serviceaccount:polar-graph:neo4j",
                &CertType::Server,
                &names(&["polar-db-svc.polar-graph.svc.cluster.local"])
            ),
            Ok(())
        );
        // A bootstrap identity's names hold on this path too.
        assert_eq!(
            admit_vouched(
                &floor(),
                "system:serviceaccount:polar:cassini",
                &CertType::Server,
                &names(&["cassini-ip-svc.polar.svc.cluster.local"])
            ),
            Ok(())
        );
    }

    #[test]
    fn a_vouched_request_cannot_carry_a_name_that_is_not_written_down_for_its_workload() {
        let f = floor_with_a_vouched_only_server();
        for (workload, extra) in [
            // Someone else's name, a wildcard, a capability.
            (
                "system:serviceaccount:polar-graph:neo4j",
                "cassini-ip-svc.polar.svc.cluster.local",
            ),
            ("system:serviceaccount:polar-graph:neo4j", "*.polar.svc"),
            ("system:serviceaccount:polar-graph:neo4j", "c:grant"),
            // A workload with no entry at all has no names it may carry.
            (
                "system:serviceaccount:polar:git-observer",
                "polar-db-svc.polar-graph.svc.cluster.local",
            ),
        ] {
            assert_eq!(
                admit_vouched(&f, workload, &CertType::Server, &names(&[extra])),
                Err(Refusal::NameNotAllowed(extra.to_string())),
                "{workload} {extra}"
            );
        }
    }

    #[test]
    fn a_vouched_request_takes_permissions_from_the_credentials_agent_and_names_only_from_the_list()
    {
        // The permissions of a grant are the credentials agent's to state; they are not names.
        assert_eq!(
            admit_vouched(
                &floor(),
                "system:serviceaccount:polar:git-observer",
                &CertType::Client,
                &names(&["p:u2s:polar-db-svc.polar-graph.svc.cluster.local"])
            ),
            Ok(())
        );
        assert_eq!(
            admit_vouched(
                &floor(),
                "system:serviceaccount:polar:git-observer",
                &CertType::Client,
                &[]
            ),
            Ok(())
        );
    }

    #[test]
    fn a_vouched_request_cannot_change_its_type_to_get_at_another_workloads_names() {
        assert_eq!(
            admit_vouched(
                &floor_with_a_vouched_only_server(),
                "system:serviceaccount:polar-graph:neo4j",
                &CertType::Client,
                &names(&["polar-db-svc.polar-graph.svc.cluster.local"])
            ),
            Err(Refusal::NameNotAllowed(
                "polar-db-svc.polar-graph.svc.cluster.local".to_string()
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
                vouched_only: false,
            },
            BootstrapIdentity {
                workload: "a".into(),
                cert_type: CertType::Server,
                names: names(&["p:s2s:x", "c:grant", "*.x", ""]),
                vouched_only: false,
            },
            BootstrapIdentity {
                workload: "a".into(),
                cert_type: CertType::Client,
                names: vec![],
                vouched_only: false,
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
