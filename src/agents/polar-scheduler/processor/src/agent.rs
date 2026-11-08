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

//! What the scheduler processor is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::supervisor::{SERVICE_NAME, SHUTDOWN_BUDGET, SchedulerSupervisor};
use polar_agent_builder::{
    Capability, Cause, Configured, PolarAgent, StartError, TopicDecl, topics::FamilyDecl,
};
use ractor::Actor;
use std::time::Duration;

pub struct SchedulerProcessor;

impl PolarAgent for SchedulerProcessor {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str =
        "Records schedule changes in the graph and notifies the agents they concern";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            polar_scheduler_common::SCHEDULER_IN_TOPIC,
            "the observer's schedule changes",
        ),
        TopicDecl::subscribe(
            polar_scheduler_common::CONFIGURATION_OUTCOME_TOPIC,
            "each agent's outcome for a configuration document, recorded in the graph",
        ),
    ];
    const TOPIC_FAMILIES: &'static [FamilyDecl] = &[FamilyDecl::publish(
        "agent.{workload}.configuration",
        "a signed document for the workload, forwarded untouched for it to verify",
    )
    .because(&[Cause::Topic(polar_scheduler_common::SCHEDULER_IN_TOPIC)])];
    const HELP: Option<&'static str> = Some(
        "The processor subscribes to the changes the scheduler observer publishes from the \
         schedules repository (scheduler.in). It writes each signed document to the graph and, \
         when its version is new, forwards it untouched to the agent it is addressed to; a file \
         that is not a signed document is ignored. It also records each agent's outcome for a \
         document. It needs the graph (GRAPH_*).",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            SchedulerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the scheduler supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the scheduler supervisor task failed: {e}"))?;
        // A processor that is no longer processing must be restarted.
        Err("the scheduler supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(SchedulerProcessor);

    #[test]
    fn its_family_is_the_topic_the_code_publishes_each_workloads_configuration_on() {
        for workload in ["kube-observer", "credentials-agent", "polar-scheduler"] {
            for family in SchedulerProcessor::TOPIC_FAMILIES {
                assert_eq!(
                    family.topic_for(workload),
                    polar_scheduler_common::configuration_topic(workload)
                );
            }
        }
        assert_eq!(SchedulerProcessor::TOPIC_FAMILIES.len(), 1);
    }
}
