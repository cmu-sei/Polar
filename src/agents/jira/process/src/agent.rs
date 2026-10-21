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

//! What jira-processor is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ConsumerSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError, TopicDecl};
use ractor::Actor;
use std::time::Duration;

const JIRA_PROCESSOR_SUPERVISOR: &str = "jira.supervisor.processor";

pub struct JiraProcessor;

impl PolarAgent for JiraProcessor {
    const NAME: &'static str = crate::BROKER_CLIENT_NAME;
    const PURPOSE: &'static str =
        "Writes the Jira projects, groups, users and issues the observer publishes into the graph";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            jira_common::JIRA_PROJECTS_CONSUMER_TOPIC,
            "the observer's projects, written to the graph",
        ),
        TopicDecl::subscribe(
            jira_common::JIRA_GROUPS_CONSUMER_TOPIC,
            "the observer's groups, written to the graph",
        ),
        TopicDecl::subscribe(
            jira_common::JIRA_USERS_CONSUMER_TOPIC,
            "the observer's users, written to the graph",
        ),
        TopicDecl::subscribe(
            jira_common::JIRA_ISSUES_CONSUMER_TOPIC,
            "the observer's issues, written to the graph",
        ),
    ];
    const HELP: Option<&'static str> = Some(
        "jira-processor subscribes to the Jira observer's four topics on Cassini (projects, \
         groups, users, issues) and writes what it receives into the graph. It reads no \
         variables of its own: the graph's and the Cassini client's are declared by those \
         components.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(JIRA_PROCESSOR_SUPERVISOR);

        let (_, handle) = Actor::spawn(
            Some(JIRA_PROCESSOR_SUPERVISOR.to_string()),
            ConsumerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("failed to start {JIRA_PROCESSOR_SUPERVISOR}: {e}"))?;

        handle
            .await
            .map_err(|e| format!("{JIRA_PROCESSOR_SUPERVISOR} task failed: {e}"))?;

        // The supervisor stopping is never a success: a processor that no longer processes must
        // be restarted. `run` exits non-zero so the Job does.
        Err(format!("{JIRA_PROCESSOR_SUPERVISOR} stopped").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(JiraProcessor);

    #[test]
    fn every_topic_it_subscribes_to_is_in_its_declared_set() {
        let set = polar_agent_builder::agent::topic_set::<JiraProcessor>();
        for topic in crate::supervisor::JIRA_TOPICS {
            assert!(
                set.allows(topic, polar_agent_builder::Access::Subscribe),
                "{topic} is not declared"
            );
        }
    }
}
