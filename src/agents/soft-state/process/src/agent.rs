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

//! What the state agent's writer is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::SERVICE_NAME;
use crate::supervisor::{SHUTDOWN_BUDGET, WriterSupervisor};
use polar_agent_builder::topics::FamilyDecl;
use polar_agent_builder::{
    Capability, Cause, Configured, GraphDecl, PolarAgent, StartError, TopicDecl,
};
use polar_env::Section;
use polar_health::report::{HEALTH_QUERY_FAMILY, HEALTH_REPLIES_TOPIC};
use ractor::Actor;
use soft_state_common::PROCESSOR_NAME;
use std::time::Duration;

pub struct SoftStateProcessor;

impl PolarAgent for SoftStateProcessor {
    const NAME: &'static str = PROCESSOR_NAME;
    const PURPOSE: &'static str =
        "Asks every agent about its health and writes what it hears into the graph";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[TopicDecl::subscribe(
        HEALTH_REPLIES_TOPIC,
        "every agent's answer about its health",
    )];
    // A question topic for each workload it serves, as the scheduler's family (docs/architecture/soft-state.md, "Revised
    // by the maintainer's review"); every agent subscribes to its own by the `agent` rule.
    const TOPIC_FAMILIES: &'static [FamilyDecl] = &[FamilyDecl::publish(
        HEALTH_QUERY_FAMILY,
        "the question about the workload's health, on its own topic",
    )
    .because(&[Cause::Timer("health round")])];
    const GRAPH: &'static [GraphDecl] = &[GraphDecl::owns(
        crate::graph::LABEL,
        "each workload's latest health report and its last transitions",
    )];
    const SECTIONS: &'static [&'static Section] = &[&crate::settings::SECTION];
    const RUNTIME: &'static [&'static polar_agent_builder::RuntimeSection] =
        &[&crate::expected::AGENTS];
    const HELP: Option<&'static str> = Some(
        "soft-state-processor is the state agent's writer. Every POLAR_HEALTH_ASK_INTERVAL_SECS it asks each \
         workload its signed runtime section `agents` names about its health, on the workload's own topic \
         agent.<workload>.health, and waits POLAR_HEALTH_ANSWER_TIMEOUT_SECS for the answers, which every agent \
         gives on polar-health-replies from its health actor's report. Each workload is then answering (with \
         its status), unresponsive (no answer stamped with its identity in time), or unreachable (named by the \
         section but outside the family it was deployed with: drift). It writes each workload's (:AgentHealth): \
         the latest report and the last 20 transitions, and no other label. Before a version of `agents` has \
         arrived it asks no one.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{PROCESSOR_NAME}.supervisor")),
            WriterSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the state agent's writer did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the state agent's writer's task failed: {e}"))?;
        Err("the state agent's writer stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_agent_builder::Access;

    polar_agent_builder::polar_agent_tests!(SoftStateProcessor);

    #[test]
    fn it_asks_each_served_workload_on_its_own_topic_and_alone_hears_the_answers() {
        let served = ["jira-observer".to_string(), "kube-observer".to_string()];
        let set = polar_agent_builder::agent::topic_set_serving::<SoftStateProcessor>(&served);
        for workload in &served {
            assert!(set.allows(
                &polar_health::report::health_topic(workload),
                Access::Publish
            ));
        }
        assert!(!set.allows("agent.git-repo-observer.health", Access::Publish));
        assert!(set.allows(HEALTH_REPLIES_TOPIC, Access::Subscribe));
    }

    #[test]
    fn its_set_of_agents_is_a_signed_runtime_section_it_writes_the_graph_and_owns_only_agent_health()
     {
        let scaffold = polar_agent_builder::agent::scaffold_document::<SoftStateProcessor>();
        assert_eq!(
            scaffold
                .runtime
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["agents"]
        );
        assert_eq!(scaffold.requires, ["cassini", "graph-write"]);
        assert_eq!(scaffold.topic_families[0].over, "POLAR_SERVED_WORKLOADS");
        let variables: Vec<&str> = scaffold
            .document
            .sections
            .iter()
            .flat_map(|s| s.variables.iter().map(|v| v.name.as_str()))
            .collect();
        assert!(variables.contains(&"POLAR_HEALTH_ASK_INTERVAL_SECS"));
        assert!(variables.contains(&"POLAR_HEALTH_ANSWER_TIMEOUT_SECS"));
        assert!(!variables.iter().any(|v| v.contains("AGENTS")));
        let owns: Vec<&str> = scaffold.graph.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(owns, ["AgentHealth"]);
    }
}
