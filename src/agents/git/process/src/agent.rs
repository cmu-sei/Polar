//! What the git processor is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::SERVICE_NAME;
use crate::supervisor::{GitProcessorSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

/// What the agent calls itself: the name a configuration or a command is addressed to.
pub const AGENT_NAME: &str = "git-repo-processor";

pub struct GitProcessor;

impl PolarAgent for GitProcessor {
    const NAME: &'static str = AGENT_NAME;
    const PURPOSE: &'static str =
        "Records the commits and ref updates the git observer publishes in the graph";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const HELP: Option<&'static str> = Some(
        "The git processor subscribes to the repository events the git observer publishes \
         (polar.git.repositories.events). Each discovered commit becomes a commit node in its \
         repository with edges to its parents; each ref update adds an edge from the ref to the \
         commit it now points at, and earlier edges stay as the ref's history. It needs the \
         graph (GRAPH_*).",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            GitProcessorSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the git processor's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the git processor's supervisor task failed: {e}"))?;
        // A processor that is no longer processing must be restarted.
        Err("the git processor's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(GitProcessor);
}
