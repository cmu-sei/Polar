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

//! What the sink is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::supervisor::{SERVICE_NAME, SHUTDOWN_BUDGET, SinkSupervisor};
use harness_common::lanes::LANES;
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

const RECEIVES: &str = "the run's messages, when this sink's index puts it on this lane";

pub struct HarnessSink;

impl PolarAgent for HarnessSink {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "Receives a run's messages from Cassini and measures them, for the load harness to measure the broker";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[
        &harness_common::settings::SECTION,
        &crate::settings::SECTION,
    ];
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(LANES[0], RECEIVES),
        TopicDecl::subscribe(LANES[1], RECEIVES),
        TopicDecl::subscribe(LANES[2], RECEIVES),
        TopicDecl::subscribe(LANES[3], RECEIVES),
        TopicDecl::subscribe(LANES[4], RECEIVES),
        TopicDecl::subscribe(LANES[5], RECEIVES),
        TopicDecl::subscribe(LANES[6], RECEIVES),
        TopicDecl::subscribe(LANES[7], RECEIVES),
    ];
    const HELP: Option<&'static str> = Some(
        "It subscribes to lane HARNESS_INDEX mod HARNESS_LANES and, once the broker acknowledges the subscription, logs \
         a line beginning `HARNESS LOAD READY`. It expects HARNESS_COUNT messages from each producer on its lane, and \
         measures their latency (after the first HARNESS_WARMUP of each), loss, repeats and order. When it has them all, \
         or none has come for HARNESS_IDLE_SECS (HARNESS_FIRST_WAIT_SECS for the first), it logs one line beginning \
         `HARNESS LOAD RESULT` and ends, exiting 0. `infra/load-harness.nu` runs it.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            SinkSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the sink's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the sink's supervisor task failed: {e}"))?;
        Err("the sink's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(HarnessSink);

    #[test]
    fn it_may_subscribe_to_every_lane_and_nothing_else() {
        let topics: Vec<&str> = HarnessSink::TOPICS.iter().map(|t| t.name).collect();
        assert_eq!(topics, LANES);
    }
}
