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
use polar_agent_builder::{
    Cause, Configured, GraphDecl, HoldReason, PolarAgent, StartError, TopicDecl,
};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;
use tracing::warn;

const JIRA_OBSERVER_SUPERVISOR: &str = "jira.supervisor.observer";

/// How often the observers poll Jira, in seconds. Not configurable yet.
const BASE_INTERVAL_SECS: u64 = 1800;

pub struct JiraObserver;

/// What the observers are started with, made from the configuration.
struct Prepared {
    jira_url: String,
    auth: JiraAuth,
    deployment: JiraDeployment,
    egress: polar::egress::Egress,
    query_date: Option<String>,
}

/// The observers' settings from `config`, or why they cannot work: an address that cannot be the site's (the
/// placeholder), or any other configuration error. Never a secret's value: polar-env never shows one.
fn prepare(config: &polar_env::Config) -> Result<Prepared, String> {
    // Declared and judged with the rest of the configuration, so a missing address or token
    // was reported before this point, with every other problem.
    let settings = JiraSettings::read(config).map_err(|e| e.to_string())?;
    // An address that cannot be the site's is a configuration error, with what to set; so are a deployment that is
    // not one, a QUERY_DATE that is not a date, and the placeholder email.
    let jira_url = settings::normalise_jira_url(&settings.url)?;
    let deployment = settings
        .deployment
        .as_deref()
        .map(settings::parse_deployment)
        .transpose()?;
    let query_date = settings
        .query_date
        .as_deref()
        .filter(|date| !date.trim().is_empty())
        .map(settings::parse_query_date)
        .transpose()?
        .map(|date| date.to_string());
    settings::refuse_placeholder_email(settings.email.as_deref())?;
    if settings::token_looks_base64(&settings.token) {
        warn!(
            "JIRA_TOKEN looks like a base64-encoded copy of a token: it decodes to printable \
             text. Jira expects the token itself; check that the Secret was not encoded twice \
             (a Secret's data is base64 already, its stringData is not)"
        );
    }
    Ok(Prepared {
        auth: JiraAuth::from_parts(settings.email, settings.token),
        deployment: JiraDeployment::resolve(deployment, &jira_url),
        jira_url,
        egress: polar::egress::Egress::read(config).map_err(|e| e.to_string())?,
        query_date,
    })
}

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
        )
        .because(&[Cause::Timer("poll")]),
        TopicDecl::publish(
            jira_common::JIRA_GROUPS_CONSUMER_TOPIC,
            "the groups it reads from Jira",
        )
        .because(&[Cause::Timer("poll")]),
        TopicDecl::publish(
            jira_common::JIRA_USERS_CONSUMER_TOPIC,
            "the users it reads from Jira",
        )
        .because(&[Cause::Timer("poll")]),
        TopicDecl::publish(
            jira_common::JIRA_ISSUES_CONSUMER_TOPIC,
            "the issues it reads from Jira",
        )
        .because(&[Cause::Timer("poll")]),
    ];
    // Where its first issue pull starts: the newest update of an issue in the graph, asked of the state agent through
    // its state actor (docs/architecture/soft-state.md, "The Jira cursor"). The read gives it the soft-state
    // capability, and with it its request and reply topics.
    const GRAPH: &'static [GraphDecl] = &[GraphDecl::reads(
        crate::issues::CURSOR_LABEL,
        "the newest updated_ms of an issue, where its first issue pull starts",
    )];
    const SECTIONS: &'static [&'static Section] = &[&settings::SECTION, &polar::egress::SECTION];
    const HELP: Option<&'static str> = Some(
        "jira-observer polls the Jira site at JIRA_URL every half hour and publishes its \
         projects, groups, users and issues for jira-processor to write into the graph. \
         JIRA_TOKEN is the credential; with JIRA_EMAIL as well it is Atlassian Cloud's basic \
         authentication, without it a bearer token. JIRA_DEPLOYMENT overrides the guess made \
         from the address. Before its first issue pull it asks the state agent for the newest update of an \
         issue in the graph and starts there; QUERY_DATE starts the first pull from a date only when the graph \
         has no issue. The egress \
         section says how the site is reached: through POLAR_EGRESS_PROXY unless the host is in \
         POLAR_EGRESS_NO_PROXY, trusting the CA certificates of POLAR_EGRESS_CA_CERT beside the \
         system's (the CA of a TLS-intercepting proxy).",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(JIRA_OBSERVER_SUPERVISOR);

        // A configuration that cannot work (the placeholder address) is not fixed by starting again: the agent stays
        // up with its infrastructure only, says why when it is asked about its health, and starts no observer
        // (docs/architecture/soft-state.md, "As built: pulled health").
        let prepared = match prepare(configured.config()) {
            Ok(prepared) => prepared,
            Err(why) => {
                return polar_agent_builder::held::stay_up(
                    configured,
                    crate::BROKER_CLIENT_NAME,
                    HoldReason::Configuration,
                    why,
                )
                .await;
            }
        };
        let args = ObserverSupervisorArgs {
            auth: prepared.auth,
            deployment: prepared.deployment,
            jira_url: prepared.jira_url,
            egress: prepared.egress,
            query_date: prepared.query_date,
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

    fn config(url: &str) -> polar_env::Config {
        let env: std::collections::HashMap<String, String> =
            [("JIRA_URL", url), ("JIRA_TOKEN", "t0ken-not-shown")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        polar_env::Config::builder(&env)
            .register(&settings::SECTION)
            .register(&polar::egress::SECTION)
            .build()
    }

    /// The placeholder address is a configuration problem the agent holds, and says, rather than a reason to end.
    #[test]
    fn the_placeholder_address_is_a_configuration_problem_with_what_to_set() {
        let why = match prepare(&config("https://example.atlassian.net")) {
            Ok(_) => panic!("the placeholder was taken"),
            Err(why) => why,
        };
        assert!(why.contains("example placeholder"), "{why}");
        assert!(why.contains("workloads.jira-observer.env"), "{why}");
        assert!(!why.contains("t0ken-not-shown"));
        let prepared = prepare(&config("https://jira.example.org")).unwrap();
        assert_eq!(prepared.jira_url, "https://jira.example.org");
    }

    /// The render writes the target's egress setting only into an agent that declares the section.
    #[test]
    fn the_scaffold_declares_the_egress_section() {
        let scaffold = polar_agent_builder::agent::scaffold_document::<JiraObserver>();
        let egress = scaffold
            .document
            .sections
            .iter()
            .find(|s| s.name == "egress")
            .expect("an egress section");
        let names: Vec<&str> = egress.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "POLAR_EGRESS_PROXY",
                "POLAR_EGRESS_NO_PROXY",
                "POLAR_EGRESS_CA_CERT"
            ]
        );
        assert!(
            scaffold
                .document
                .sections
                .iter()
                .flat_map(|s| &s.variables)
                .all(|v| v.name != "PROXY_CA_CERT"),
            "PROXY_CA_CERT is gone"
        );
    }
}
