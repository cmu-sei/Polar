//! What the git scheduler is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::SERVICE_NAME;
use crate::supervisor::{GitSchedulerSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

/// What the agent calls itself: the name a configuration or a command is addressed to.
pub const AGENT_NAME: &str = "git-repo-scheduler";

pub struct GitScheduler;

impl PolarAgent for GitScheduler {
    const NAME: &'static str = AGENT_NAME;
    const PURPOSE: &'static str =
        "Decides how a discovered git repository is observed and tells the git observer";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const HELP: Option<&'static str> = Some(
        "The git scheduler subscribes to the repositories other agents discover \
         (polar.git.repositories.discovery). For each it looks in the graph for a stored \
         observation configuration, falls back to a default (origin, refs/heads/main, a shallow \
         depth of 100) and publishes the result on git.repo.config.events for the git observer. \
         It needs the graph (GRAPH_*), which it only reads.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            GitSchedulerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the git scheduler's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the git scheduler's supervisor task failed: {e}"))?;
        // A scheduler that is no longer scheduling must be restarted.
        Err("the git scheduler's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(GitScheduler);
}
