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

//! What the producer is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ProducerSupervisor, SERVICE_NAME, SHUTDOWN_BUDGET};
use harness_common::lanes::LANES;
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

const SENDS: &str = "the run's messages, when this producer's index puts it on this lane";

pub struct HarnessProducer;

impl PolarAgent for HarnessProducer {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "Sends a run's messages to Cassini at a given rate and size, for the load harness to measure the broker";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[
        &harness_common::settings::SECTION,
        &crate::settings::SECTION,
    ];
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::publish(LANES[0], SENDS),
        TopicDecl::publish(LANES[1], SENDS),
        TopicDecl::publish(LANES[2], SENDS),
        TopicDecl::publish(LANES[3], SENDS),
        TopicDecl::publish(LANES[4], SENDS),
        TopicDecl::publish(LANES[5], SENDS),
        TopicDecl::publish(LANES[6], SENDS),
        TopicDecl::publish(LANES[7], SENDS),
    ];
    const HELP: Option<&'static str> = Some(
        "Once registered, it sends HARNESS_COUNT messages of HARNESS_MESSAGE_SIZE bytes on lane HARNESS_INDEX mod \
         HARNESS_LANES, at HARNESS_RATE a second, never with more than HARNESS_WINDOW messages (or HARNESS_WINDOW_BYTES) unacknowledged. Each message \
         carries its producer, its sequence number and its send time. When every message is acknowledged or refused, \
         or nothing has been for HARNESS_STALL_SECS, it logs one line beginning `HARNESS LOAD RESULT` and ends, exiting 0. \
         `infra/load-harness.nu` runs it.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            ProducerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the producer's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the producer's supervisor task failed: {e}"))?;
        Err("the producer's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(HarnessProducer);

    #[test]
    fn it_may_publish_on_every_lane_and_nothing_else() {
        let topics: Vec<&str> = HarnessProducer::TOPICS.iter().map(|t| t.name).collect();
        assert_eq!(topics, LANES);
    }
}
