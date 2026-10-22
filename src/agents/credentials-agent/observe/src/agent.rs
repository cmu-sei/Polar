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

//! What the credentials agent observer is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ObserverSupervisor, SERVICE_NAME, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Cause, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct CredentialsObserver;

impl PolarAgent for CredentialsObserver {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "The HTTP front of the credentials agent: relays certificate, account and admin requests to the consumer over Cassini";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
            "a certificate request from HTTP (POST /issue)",
        )
        .because(&[Cause::Http(credentials_agent_common::routes::ISSUE)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
            "polar-init's workload session and issuance (/workload/session, /workload/issue)",
        )
        .because(&[
            Cause::Http(credentials_agent_common::routes::WORKLOAD_SESSION),
            Cause::Http(credentials_agent_common::routes::WORKLOAD_ISSUE),
        ]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC,
            "an identity to normalize, from HTTP (/identity)",
        )
        .because(&[Cause::Http(credentials_agent_common::routes::IDENTITY)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
            "an agent's escrow public key, asked for over HTTP (/agent-escrow-key)",
        )
        .because(&[Cause::Http(
            credentials_agent_common::routes::AGENT_ESCROW_KEY,
        )]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_ACCOUNT_REQUESTS_TOPIC,
            "an account request from HTTP (/account)",
        )
        .because(&[Cause::Http(credentials_agent_common::routes::ACCOUNT)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC,
            "a signed administrator action from HTTP (/admin-action)",
        )
        .because(&[Cause::Http(credentials_agent_common::routes::ADMIN_ACTION)]),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
            "the answer to an issuance it relayed",
        )
        .answers(&[Cause::Http(credentials_agent_common::routes::ISSUE)]),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC,
            "the answer to a workload's request it relayed",
        )
        .answers(&[
            Cause::Http(credentials_agent_common::routes::WORKLOAD_SESSION),
            Cause::Http(credentials_agent_common::routes::WORKLOAD_ISSUE),
        ]),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC,
            "the answer to a normalization it relayed",
        )
        .answers(&[Cause::Http(credentials_agent_common::routes::IDENTITY)]),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC,
            "the answer to an escrow key lookup it relayed",
        )
        .answers(&[Cause::Http(
            credentials_agent_common::routes::AGENT_ESCROW_KEY,
        )]),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_ACCOUNT_REPLIES_TOPIC,
            "the answer to an account request it relayed",
        )
        .answers(&[Cause::Http(credentials_agent_common::routes::ACCOUNT)]),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC,
            "the answer to an administrator action it relayed",
        )
        .answers(&[Cause::Http(credentials_agent_common::routes::ADMIN_ACTION)]),
    ];
    const SECTIONS: &'static [&'static Section] = &[&crate::settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "The observer takes the credentials agent's requests over HTTP, publishes each on Cassini \
         and waits for the consumer's reply. It makes no trust decision: it checks that a request \
         is well formed and relays it.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            ObserverSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the observer supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the observer supervisor task failed: {e}"))?;
        // The supervisor is meant to live as long as the pod; `run` exits non-zero, so the Job
        // starts a fresh pod.
        Err("the observer supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(CredentialsObserver);
}
