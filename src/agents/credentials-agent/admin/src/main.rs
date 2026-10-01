/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

mod bootstrap;
mod bootstrap_ca;
mod publish;

use cert_issuer_common::capability::Capability;
use cert_issuer_common::permission::Permission;
use clap::{Parser, Subcommand};
use credentials_agent_common::admin_action::{
    AdminAction, AdminActionBody, AdminActionReply,
};
use credentials_agent_common::admin_signing;
use credentials_agent_common::events::{GrantReason, RevocationReason};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "polar-admin-grant",
    about = "Sign administrative actions and submit them to the Credentials Agent"
)]
struct Args {
    /// The admin's own certificate (PEM). Its key signs every action; the
    /// credentials agent looks the author up by this certificate's serial.
    /// Falls back to $TLS_CLIENT_CERT when absent or empty.
    #[arg(long, global = true)]
    cert: Option<PathBuf>,
    /// The private key (PEM) matching --cert. Falls back to $TLS_CLIENT_KEY
    /// when absent or empty.
    #[arg(long, global = true)]
    key: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Grant permissions to a target identity. target_identity must
    /// be the exact claim value the relevant IssuerConfig's
    /// workload_identity_claim will extract from the user's real
    /// token -- confirm this against the IdP directly before
    /// publishing, or the grant will silently never match.
    Grant {
        #[arg(long)]
        target_identity: String,
        /// Terse scope[:target] form -- may be specified multiple
        /// times: --permission p:s2s:cassini.cluster.local
        /// --permission p:u2s. Scopes: s2s (service-to-service), s2u
        /// (service-to-user), u2s (user-to-service), u2u
        /// (user-to-user). Target is an optional FQDN, validated the
        /// same way it will be when the certificate itself is parsed.
        #[arg(long = "permission", required = true, value_parser = Permission::from_san_string)]
        permissions: Vec<Permission>,
        #[arg(long, value_enum)]
        reason: GrantReason,
    },
    /// Revoke a certificate by its serial number -- uniform for
    /// human- and workload-issued certificates alike.
    Revoke {
        #[arg(long)]
        serial: String,
        #[arg(long, value_enum)]
        reason: RevocationReason,
    },
    /// Provision a new Polar account. The account isn't usable until the
    /// holder submits a CSR claiming it by username and email -- this only
    /// reserves the permissions, capabilities and field defaults for that
    /// later claim. The credentials agent mints the account's UUID; it is
    /// never supplied here.
    ProvisionAccount {
        /// The human-facing nickname the user will present, together
        /// with email, to claim this account. Cross-references the
        /// account's UUID; not itself the identity.
        #[arg(long)]
        username: String,
        /// Security-sensitive, admin-authoritative -- the actual
        /// gate on the claim, never user-suppliable at claim time.
        #[arg(long)]
        email: String,
        #[arg(long = "permission", required = true, value_parser = Permission::from_san_string)]
        permissions: Vec<Permission>,
        /// Administrative capability the account holds, e.g.
        /// `--capability c:grant`. May be repeated. Omit for none.
        #[arg(long = "capability", value_parser = Capability::from_san_string)]
        capabilities: Vec<Capability>,
        #[arg(long, value_enum)]
        reason: GrantReason,
        /// Admin-settable defaults for fields the user's own CSR may
        /// still override -- never used for anything
        /// security-sensitive.
        #[arg(long)]
        given_name: Option<String>,
        #[arg(long)]
        surname: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        telephone_number: Option<String>,
    },
    /// Generate the cluster's CA and provision it into Kubernetes: a
    /// Secret holding the certificate and key for cert-issuer, and a
    /// ConfigMap holding only the certificate for every pod that has to
    /// verify it. Run once per cluster, with the cluster admin's own
    /// kubeconfig, after the namespaces exist and before cert-issuer
    /// starts in provisioned mode. Unlike every other subcommand it does
    /// not go through Cassini -- the CA has to exist before Cassini
    /// can. It creates and never overwrites.
    BootstrapCa {
        /// Common name embedded in the CA certificate.
        #[arg(long, default_value = "Polar Internal CA")]
        common_name: String,
        /// Namespace for the Secret holding the CA key. Only
        /// cert-issuer should ever mount it.
        #[arg(long, default_value = "polar")]
        secret_namespace: String,
        /// Namespaces to receive the public-certificate ConfigMap. May be
        /// repeated. Every namespace with a pod that verifies cert-issuer
        /// needs one, because ConfigMaps can't be shared across
        /// namespaces.
        #[arg(long = "configmap-namespace", default_values = ["polar", "polar-graph"])]
        configmap_namespaces: Vec<String>,
    },
    /// Claim the bootstrap admin account, once per cluster. Mints a
    /// short-lived token for the bootstrap ServiceAccount with your own
    /// kubeconfig, proves it to the credentials agent through the observer,
    /// and writes the resulting admin certificate and key to disk. Run it
    /// after the observer is reachable, e.g. behind
    /// `kubectl -n polar port-forward svc/credentials-agent-observer 8080`.
    /// The claim succeeds exactly once: back the files up, because losing
    /// them means rebuilding the cluster. Refuses to overwrite existing files.
    Bootstrap {
        #[arg(long, default_value = "polar-bootstrap")]
        namespace: String,
        #[arg(long, default_value = "polar-admin-bootstrap")]
        service_account: String,
        /// Audience the token is minted for; must be the one cert-issuer's
        /// token validation expects.
        #[arg(long, default_value = "polar-cert-issuer.local")]
        audience: String,
        #[arg(long, default_value = "http://localhost:8080")]
        observer_url: String,
        #[arg(long, default_value = "bootstrap-admin.crt")]
        cert_out: PathBuf,
        #[arg(long, default_value = "bootstrap-admin.key")]
        key_out: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    std::process::exit(run(args).await);
}

/// The admin action a subcommand asks for. BootstrapCa is handled by run()
/// before this is ever called: it is not an admin action.
fn action_from_command(command: Command) -> AdminAction {
    match command {
        Command::Grant {
            target_identity,
            permissions,
            reason,
        } => AdminAction::Grant {
            target_identity,
            permissions,
            reason,
        },
        Command::Revoke { serial, reason } => AdminAction::Revoke { serial, reason },
        Command::ProvisionAccount {
            username,
            email,
            permissions,
            capabilities,
            reason,
            given_name,
            surname,
            title,
            telephone_number,
        } => AdminAction::ProvisionAccount {
            username,
            email,
            permissions,
            capabilities,
            reason,
            given_name,
            surname,
            title,
            telephone_number,
        },
        Command::BootstrapCa { .. } | Command::Bootstrap { .. } => {
            unreachable!("bootstrap commands are not admin actions; run() dispatches them first")
        }
    }
}

/// A path from the environment, treating unset and empty alike. Read only by
/// the commands that sign, so a stray empty variable in the shell cannot
/// break the ones that never need a certificate.
fn path_from_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

async fn run(args: Args) -> i32 {
    let Args { cert, key, command } = args;

    if let Command::Bootstrap {
        namespace,
        service_account,
        audience,
        observer_url,
        cert_out,
        key_out,
    } = command
    {
        return match bootstrap::run(bootstrap::BootstrapArgs {
            namespace,
            service_account,
            audience,
            observer_url,
            cert_out,
            key_out,
        })
        .await
        {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }

    if let Command::BootstrapCa {
        common_name,
        secret_namespace,
        configmap_namespaces,
    } = command
    {
        return match bootstrap_ca::run(bootstrap_ca::BootstrapCaArgs {
            common_name,
            secret_namespace,
            configmap_namespaces,
        })
        .await
        {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }

    let cert = cert.or_else(|| path_from_env("TLS_CLIENT_CERT"));
    let key = key.or_else(|| path_from_env("TLS_CLIENT_KEY"));
    let (Some(cert), Some(key)) = (cert, key) else {
        eprintln!("error: --cert and --key (or TLS_CLIENT_CERT and TLS_CLIENT_KEY) are required");
        return 1;
    };

    let cert_pem = match std::fs::read_to_string(&cert) {
        Ok(pem) => pem,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", cert.display());
            return 1;
        }
    };
    let key_pem = match std::fs::read_to_string(&key) {
        Ok(pem) => pem,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", key.display());
            return 1;
        }
    };

    let body = AdminActionBody {
        nonce: uuid::Uuid::new_v4().to_string(),
        action: action_from_command(command),
    };
    let signed = match admin_signing::sign(&body, &cert_pem, &key_pem) {
        Ok(signed) => signed,
        Err(e) => {
            eprintln!("error: cannot sign: {e}");
            return 1;
        }
    };

    match publish::request_admin_action(signed, Duration::from_secs(30)).await {
        Ok(AdminActionReply::Recorded) => {
            eprintln!("recorded");
            0
        }
        Ok(AdminActionReply::Refused { reason }) => {
            eprintln!("refused: {reason:?}");
            1
        }
        Ok(AdminActionReply::Failed { detail }) => {
            eprintln!("failed: {detail}");
            1
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_command_becomes_a_grant_action() {
        let action = action_from_command(Command::Grant {
            target_identity: "testuser".to_string(),
            permissions: vec![
                Permission::from_san_string("p:s2s:cassini.cluster.local").unwrap(),
                Permission::from_san_string("p:u2s").unwrap(),
            ],
            reason: GrantReason::UserOnboarding,
        });

        match action {
            AdminAction::Grant {
                target_identity,
                permissions,
                ..
            } => {
                assert_eq!(target_identity, "testuser");
                assert_eq!(
                    permissions,
                    vec![
                        Permission::from_san_string("p:s2s:cassini.cluster.local").unwrap(),
                        Permission::from_san_string("p:u2s").unwrap(),
                    ]
                );
            }
            other => panic!("expected Grant, got {other:?}"),
        }
    }

    #[test]
    fn revoke_command_becomes_a_revoke_action() {
        let action = action_from_command(Command::Revoke {
            serial: "abc123".to_string(),
            reason: RevocationReason::Compromised,
        });

        match action {
            AdminAction::Revoke { serial, reason } => {
                assert_eq!(serial, "abc123");
                assert_eq!(reason, RevocationReason::Compromised);
            }
            other => panic!("expected Revoke, got {other:?}"),
        }
    }

    #[test]
    fn provision_command_carries_capabilities_and_no_account_id() {
        let action = action_from_command(Command::ProvisionAccount {
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            permissions: vec![Permission::from_san_string("p:u2s").unwrap()],
            capabilities: vec![Capability::ProvisionAccount],
            reason: GrantReason::UserOnboarding,
            given_name: None,
            surname: None,
            title: None,
            telephone_number: None,
        });

        match action {
            AdminAction::ProvisionAccount {
                username,
                capabilities,
                ..
            } => {
                assert_eq!(username, "alice");
                assert_eq!(capabilities, vec![Capability::ProvisionAccount]);
            }
            other => panic!("expected ProvisionAccount, got {other:?}"),
        }
    }
}
