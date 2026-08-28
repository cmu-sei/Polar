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

use clap::{Parser, Subcommand};
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::{CREDENTIALS_GRANTS_TOPIC, CREDENTIALS_REVOCATIONS_TOPIC};

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
        /// May be specified multiple times: --permission read --permission write
        #[arg(long = "permission", required = true)]
        permissions: Vec<String>,
        #[arg(long)]
        granted_by: String,
    },
    /// Revoke a certificate by its serial number -- uniform for
    /// human- and workload-issued certificates alike.
    Revoke {
        #[arg(long)]
        serial: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        revoked_by: String,
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
        } => (
            CredentialsEvent::Grant {
                target_identity,
                permissions,
                granted_by,
                timestamp: polar::time::now_millis(),
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
            permissions: vec!["read".to_string(), "write".to_string()],
            granted_by: "admin".to_string(),
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
                assert_eq!(permissions, vec!["read".to_string(), "write".to_string()]);
                assert_eq!(granted_by, "admin");
            }
            other => panic!("expected Grant, got {other:?}"),
        }
    }

    #[test]
    fn revoke_command_produces_the_right_event_and_topic() {
        let command = Command::Revoke {
            serial: "abc123".to_string(),
            reason: "compromised".to_string(),
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
                assert_eq!(reason, "compromised");
                assert_eq!(revoked_by, "admin");
            }
            other => panic!("expected Revoke, got {other:?}"),
        }
    }
}
