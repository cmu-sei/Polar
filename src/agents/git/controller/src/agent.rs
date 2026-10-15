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

//! What the git controller is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::SERVICE_NAME;
use crate::supervisor::{RootSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

/// What the agent calls itself: the name a configuration or a command is addressed to.
pub const AGENT_NAME: &str = "git-repo-controller";

pub struct GitController;

impl PolarAgent for GitController {
    const NAME: &'static str = AGENT_NAME;
    const PURPOSE: &'static str =
        "Decides which git repositories are observed, and tells the git observer";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static polar_env::Section] = &[&crate::settings::SECTION];
    const RUNTIME: &'static [&'static polar_agent_builder::RuntimeSection] =
        &[&crate::desired::REPOSITORIES, &crate::admission::ADMISSION];
    const HELP: Option<&'static str> = Some(
        "The git controller holds the latest `repositories` runtime section (a signed document \
         from the schedules repository) and announces every repository in it on \
         git.repo.config.events for the git observer: when it starts, when the section changes, \
         and every POLAR_GIT_CONTROLLER_REPUBLISH_SECS seconds, since a message is not kept for an \
         observer that is not there yet. A repository names the credential it needs; the \
         controller never sees a credential. It does not touch the graph.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            RootSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the git controller's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the git controller's supervisor task failed: {e}"))?;
        // A controller that no longer controls must be restarted: `run` exits non-zero, so the Job does.
        Err("the git controller's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(GitController);
}
