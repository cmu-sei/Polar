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
        &[&crate::desired::REPOSITORIES];
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
