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

mod publish;

use cert_issuer_common::permission::Permission;
use clap::{Parser, Subcommand};
use credentials_agent_common::events::{CredentialsEvent, GrantReason, RevocationReason};
use credentials_agent_common::{
    CREDENTIALS_ACCOUNT_PROVISIONING_TOPIC, CREDENTIALS_GRANTS_TOPIC, CREDENTIALS_REVOCATIONS_TOPIC,
};

#[derive(Parser, Debug)]
#[command(
    name = "polar-admin-grant",
    about = "Publish grants and revocations to the Credentials Agent"
)]
struct Args {
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
        #[arg(long)]
        granted_by: String,
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
        #[arg(long)]
        revoked_by: String,
    },
    /// Provision a new Polar account -- an admin-side act, mirroring
    /// Grant's own shape, but for a durable account identity rather
    /// than a permission on an existing one. The account isn't usable
    /// until the holder submits a CSR claiming it by username and
    /// email (see the account-claim flow) -- this only reserves the
    /// account_id/permissions/field-defaults for that later claim.
    /// Single-use: a second claim attempt after the first succeeds
    /// finds nothing, by construction -- no separate check needed.
    ProvisionAccount {
        /// The account's own durable identity -- a fresh UUID,
        /// minted here, never chosen by or made known to the user in
        /// advance.
        #[arg(long)]
        account_id: String,
        /// The human-facing nickname the user will present, together
        /// with email, to claim this account. Cross-references
        /// account_id; not itself the identity.
        #[arg(long)]
        username: String,
        /// Security-sensitive, admin-authoritative -- the actual
        /// gate on the claim, never user-suppliable at claim time.
        #[arg(long)]
        email: String,
        #[arg(long = "permission", required = true, value_parser = Permission::from_san_string)]
        permissions: Vec<Permission>,
        #[arg(long)]
        provisioned_by: String,
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
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    std::process::exit(run(args).await);
}

fn event_and_topic_from_command(command: Command) -> (CredentialsEvent, &'static str) {
    match command {
        Command::Grant {
            target_identity,
            permissions,
            granted_by,
            reason,
        } => (
            CredentialsEvent::Grant {
                target_identity,
                permissions,
                granted_by,
                timestamp: polar::time::now_millis(),
                reason,
            },
            CREDENTIALS_GRANTS_TOPIC,
        ),
        Command::Revoke {
            serial,
            reason,
            revoked_by,
        } => (
            CredentialsEvent::Revoke {
                serial,
                reason,
                revoked_by,
                timestamp: polar::time::now_millis(),
            },
            CREDENTIALS_REVOCATIONS_TOPIC,
        ),
        Command::ProvisionAccount {
            account_id,
            username,
            email,
            permissions,
            provisioned_by,
            reason,
            given_name,
            surname,
            title,
            telephone_number,
        } => (
            CredentialsEvent::AccountProvisioned {
                account_id,
                username,
                email,
                permissions,
                provisioned_by,
                timestamp: polar::time::now_millis(),
                reason,
                via_bootstrap: false,
                given_name,
                surname,
                title,
                telephone_number,
            },
            CREDENTIALS_ACCOUNT_PROVISIONING_TOPIC,
        ),
    }
}

async fn run(args: Args) -> i32 {
    let (event, topic) = event_and_topic_from_command(args.command);

    match publish::publish_event(event, topic.to_string()).await {
        Ok(()) => {
            // A brief, deliberate delay: Publish is fire-and-forget at
            // the TcpClientMessage level -- this gives the underlying
            // TCP client a real chance to flush the bytes over the
            // wire before the process exits out from under it.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            eprintln!("published to {topic}");
            0
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
    fn grant_command_produces_the_right_event_and_topic() {
        let command = Command::Grant {
            target_identity: "testuser".to_string(),
            permissions: vec![
                Permission::from_san_string("p:s2s:cassini.cluster.local").unwrap(),
                Permission::from_san_string("p:u2s").unwrap(),
            ],
            granted_by: "admin".to_string(),
            reason: GrantReason::UserOnboarding,
        };

        let (event, topic) = event_and_topic_from_command(command);

        assert_eq!(topic, CREDENTIALS_GRANTS_TOPIC);
        match event {
            CredentialsEvent::Grant {
                target_identity,
                permissions,
                granted_by,
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
                assert_eq!(granted_by, "admin");
            }
            other => panic!("expected Grant, got {other:?}"),
        }
    }

    #[test]
    fn revoke_command_produces_the_right_event_and_topic() {
        let command = Command::Revoke {
            serial: "abc123".to_string(),
            reason: RevocationReason::Compromised,
            revoked_by: "admin".to_string(),
        };

        let (event, topic) = event_and_topic_from_command(command);

        assert_eq!(topic, CREDENTIALS_REVOCATIONS_TOPIC);
        match event {
            CredentialsEvent::Revoke {
                serial,
                reason,
                revoked_by,
                ..
            } => {
                assert_eq!(serial, "abc123");
                assert_eq!(reason, RevocationReason::Compromised);
                assert_eq!(revoked_by, "admin");
            }
            other => panic!("expected Revoke, got {other:?}"),
        }
    }
}
