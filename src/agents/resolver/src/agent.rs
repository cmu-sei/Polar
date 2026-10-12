//! What the OCI resolver is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::RESOLVER_SUPERVISOR_NAME;
use crate::settings;
use crate::supervisor::{ResolverSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct OciResolver;

impl PolarAgent for OciResolver {
    const NAME: &'static str = "oci-resolver";
    const PURPOSE: &'static str = "Follows image references other agents discover to their registry and publishes the verified manifest";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[&settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "The resolver subscribes to the provenance discovery topic on Cassini. For each image \
         reference it is told of, it pulls the manifest from the registry (with the credentials \
         in DOCKER_CONFIG, or anonymously) and publishes the resolved artifact for the build \
         processor. It never touches the graph.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(RESOLVER_SUPERVISOR_NAME);

        rustls::crypto::ring::default_provider()
            .install_default()
            .map_err(|_| "cannot install the rustls crypto provider")?;

        let (_supervisor, handle) = Actor::spawn(
            Some(RESOLVER_SUPERVISOR_NAME.to_string()),
            ResolverSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("failed to start {RESOLVER_SUPERVISOR_NAME}: {e}"))?;

        handle
            .await
            .map_err(|e| format!("{RESOLVER_SUPERVISOR_NAME} task failed: {e}"))?;

        // The supervisor stopping is never a success: a resolver that no longer resolves must be
        // restarted. `run` exits non-zero so the Job does.
        Err(format!("{RESOLVER_SUPERVISOR_NAME} stopped").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(OciResolver);
}
