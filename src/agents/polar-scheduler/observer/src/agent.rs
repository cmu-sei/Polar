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

use crate::supervisor::{SERVICE_NAME, SHUTDOWN_BUDGET, SchedulerObserverSupervisor};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct SchedulerObserver;

impl PolarAgent for SchedulerObserver {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "Keeps a checkout of the schedules repository current and publishes each schedule change on Cassini";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            polar_git_server_common::POLAR_GIT_REPO_CHANGED_TOPIC,
            "a push to the schedules repository, which starts a sync",
        ),
        TopicDecl::subscribe(
            polar_scheduler_common::CONFIGURATION_REPLAY_TOPIC,
            "an agent asking for the documents held for it",
        ),
        TopicDecl::publish(
            polar_scheduler_common::SCHEDULER_IN_TOPIC,
            "each schedule created, changed or deleted in the checkout",
        ),
    ];
    const SECTIONS: &'static [&'static Section] = &[&crate::settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "The observer clones the schedules repository from its remote, watches the checkout, \
         evaluates each changed Dhall schedule and publishes the change to the scheduler. It \
         pulls when the Polar git server announces a change to that repository on Cassini, and \
         on an interval if one is set (a remote that is not the Polar git server announces \
         nothing). To a Polar git server it presents the certificate it is given, which is how \
         the server decides it may read.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            SchedulerObserverSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the scheduler observer supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the scheduler observer supervisor task failed: {e}"))?;
        Err("the scheduler observer supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(SchedulerObserver);
}
