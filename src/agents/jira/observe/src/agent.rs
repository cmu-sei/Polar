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

//! What the Jira observer is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::settings::{self, JiraSettings};
use crate::supervisor::{ObserverSupervisor, ObserverSupervisorArgs, SHUTDOWN_BUDGET};
use crate::{JiraAuth, JiraDeployment};
use polar_agent_builder::{Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

const JIRA_OBSERVER_SUPERVISOR: &str = "jira.supervisor.observer";

/// How often the observers poll Jira, in seconds. Not configurable yet.
const BASE_INTERVAL_SECS: u64 = 1800;

pub struct JiraObserver;

impl PolarAgent for JiraObserver {
    const NAME: &'static str = crate::BROKER_CLIENT_NAME;
    const PURPOSE: &'static str =
        "Polls a Jira site for projects, groups, users and issues and publishes them to Cassini";
    const NEEDS: &'static [polar_agent_builder::Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::publish(
            jira_common::JIRA_PROJECTS_CONSUMER_TOPIC,
            "the projects it reads from Jira",
        ),
        TopicDecl::publish(
            jira_common::JIRA_GROUPS_CONSUMER_TOPIC,
            "the groups it reads from Jira",
        ),
        TopicDecl::publish(
            jira_common::JIRA_USERS_CONSUMER_TOPIC,
            "the users it reads from Jira",
        ),
        TopicDecl::publish(
            jira_common::JIRA_ISSUES_CONSUMER_TOPIC,
            "the issues it reads from Jira",
        ),
    ];
    const SECTIONS: &'static [&'static Section] = &[&settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "jira-observer polls the Jira site at JIRA_URL every half hour and publishes its \
         projects, groups, users and issues for jira-processor to write into the graph. \
         JIRA_TOKEN is the credential; with JIRA_EMAIL as well it is Atlassian Cloud's basic \
         authentication, without it a bearer token. JIRA_DEPLOYMENT overrides the guess made \
         from the address, PROXY_CA_CERT trusts the CA of a TLS-intercepting proxy, and \
         QUERY_DATE starts the first full pull from a date.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(JIRA_OBSERVER_SUPERVISOR);

        // Declared and judged with the rest of the configuration, so a missing address or token
        // was reported before this point, with every other problem.
        let settings = JiraSettings::read(configured.config())?;
        let args = ObserverSupervisorArgs {
            auth: JiraAuth::from_parts(settings.email, settings.token),
            deployment: JiraDeployment::resolve(settings.deployment.as_deref(), &settings.url),
            jira_url: settings.url,
            proxy_ca_cert_file: settings.proxy_ca_cert,
            query_date: settings.query_date,
            // TODO: read this from configuration
            base_interval: BASE_INTERVAL_SECS,
            configured,
        };

        let (_, handle) = Actor::spawn(
            Some(JIRA_OBSERVER_SUPERVISOR.to_string()),
            ObserverSupervisor,
            args,
        )
        .await
        .map_err(|e| format!("failed to start {JIRA_OBSERVER_SUPERVISOR}: {e}"))?;

        handle
            .await
            .map_err(|e| format!("{JIRA_OBSERVER_SUPERVISOR} task failed: {e}"))?;

        // The supervisor stopping is never a success: this agent runs as a Job and only a
        // non-zero exit gets it restarted. `run` exits non-zero.
        Err(format!("{JIRA_OBSERVER_SUPERVISOR} stopped").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(JiraObserver);
}
