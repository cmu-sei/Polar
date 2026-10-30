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

//! What the state agent's reader is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::SERVICE_NAME;
use crate::supervisor::{SHUTDOWN_BUDGET, StateSupervisor};
use polar_agent_builder::topics::{FamilyDecl, Over};
use polar_agent_builder::{Capability, Cause, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use soft_state_common::{OBSERVER_NAME, REPLIES_FAMILY, REQUESTS_TOPIC};
use std::time::Duration;

pub struct SoftStateObserver;

impl PolarAgent for SoftStateObserver {
    const NAME: &'static str = OBSERVER_NAME;
    const PURPOSE: &'static str =
        "Answers other agents' requests for soft state, read from the graph, by who asks";
    const NEEDS: &'static [Capability] = &[Capability::GraphRead];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[TopicDecl::subscribe(
        REQUESTS_TOPIC,
        "an agent's request for state it may read",
    )];
    // One reply topic for each workload that reads state, and none for one that does not
    // (docs/architecture/soft-state.md, "Shape").
    const TOPIC_FAMILIES: &'static [FamilyDecl] = &[FamilyDecl::publish(
        REPLIES_FAMILY,
        "the answer to the workload's request, on its own topic",
    )
    .because(&[Cause::Topic(REQUESTS_TOPIC)])
    .over(Over::StateReaders)];
    const SECTIONS: &'static [&'static Section] = &[&crate::floor::SECTION];
    const HELP: Option<&'static str> = Some(
        "soft-state-observer answers the requests other agents publish on soft-state-requests for state that is \
         not authoritative (a poller's cursor, an observer's checkpoint), reading it from the graph. It decides \
         each request by the identity the broker vouches for: Cedar first (Action::\"read-state\" on \
         Label::\"<label>\"), then the floor in SOFT_STATE_FLOOR, which the render writes from every agent's \
         declared graph labels; anything else is refused. It builds the query itself from a label the asker may \
         read and a property name that is a plain identifier, and never writes the graph. Each answer goes to the \
         asker's own topic, soft-state-replies.<workload>.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{OBSERVER_NAME}.supervisor")),
            StateSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the state agent's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the state agent's supervisor task failed: {e}"))?;
        Err("the state agent's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_agent_builder::Access;

    polar_agent_builder::polar_agent_tests!(SoftStateObserver);

    #[test]
    fn its_family_is_the_topic_each_reader_is_answered_on_and_only_readers_get_one() {
        let readers: std::collections::BTreeMap<Over, Vec<String>> = [
            (Over::StateReaders, vec!["jira-observer".to_string()]),
            (
                Over::Served,
                vec!["jira-observer".to_string(), "kube-observer".to_string()],
            ),
        ]
        .into();
        let set = polar_agent_builder::agent::topic_set_over::<SoftStateObserver>(&readers);
        assert!(set.allows(
            &soft_state_common::replies_topic("jira-observer"),
            Access::Publish
        ));
        assert!(!set.allows(
            &soft_state_common::replies_topic("kube-observer"),
            Access::Publish
        ));
        assert!(set.allows(REQUESTS_TOPIC, Access::Subscribe));
    }

    #[test]
    fn it_reads_the_graph_without_its_health_depending_on_it() {
        let scaffold = polar_agent_builder::agent::scaffold_document::<SoftStateObserver>();
        assert_eq!(scaffold.requires, ["cassini", "graph-read"]);
        // It reads on behalf of others, and declares no label of its own.
        assert!(scaffold.graph.is_empty());
        assert_eq!(scaffold.topic_families[0].over, "POLAR_STATE_READERS");
    }
}
