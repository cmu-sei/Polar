//! What the git observer is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::settings;
use crate::supervisor::{RootSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

/// What the agent calls itself: the name a configuration or a command is addressed to.
pub const AGENT_NAME: &str = "git-repo-observer";

pub struct GitObserver;

impl PolarAgent for GitObserver {
    const NAME: &'static str = AGENT_NAME;
    const PURPOSE: &'static str =
        "Watches git repositories it is told of and publishes their commits and ref updates";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[&settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "The git observer subscribes to repository configuration events on Cassini. For each \
         repository it is told of it clones the repository into its cache, and on the frequency \
         that event gives it fetches, walks the new commits and publishes them and the ref \
         updates. It never touches the graph.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(crate::SERVICE_NAME);

        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{}.supervisor", crate::SERVICE_NAME)),
            RootSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the git observer's supervisor did not start: {e}"))?;

        handle
            .await
            .map_err(|e| format!("the git observer's supervisor task failed: {e}"))?;

        // The supervisor stopping is never a success: an observer that no longer observes must be
        // restarted. `run` exits non-zero so the Job does.
        Err("the git observer's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(GitObserver);
}
