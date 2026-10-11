use crate::supervisor::{GitServerSupervisor, SERVICE_NAME, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct GitServer;

impl PolarAgent for GitServer {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "Hosts Polar's configuration repositories over git smart HTTP, authenticating users and agents by their mTLS certificates";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[&crate::settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "The git server serves the repositories its configuration file declares to the user's own \
         git client. Every connection presents a certificate chaining to the Polar CA; the \
         identity in it decides what that caller may read or push, per repository. After a push \
         is accepted it publishes the change on Cassini. Repositories are created and seeded \
         from the configuration the first time, and never overwritten after that.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            GitServerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the git server supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the git server supervisor task failed: {e}"))?;
        Err("the git server supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(GitServer);
}
