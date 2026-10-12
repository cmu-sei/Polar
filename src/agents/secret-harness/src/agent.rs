//! What the harness is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::supervisor::{HarnessSupervisor, SERVICE_NAME, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct SecretHarness;

impl PolarAgent for SecretHarness {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "Checks the credentials agent's secret policy from inside the cluster, as two different identities";
    const NEEDS: &'static [Capability] = &[Capability::Encryption];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[&crate::settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "Run it twice with different identities. As HARNESS_ROLE=owner it stores a secret, retrieves \
         it, replaces it, and keeps retrieving it. As HARNESS_ROLE=intruder it tries to retrieve that \
         secret, a name nothing holds, and to store the same name, and must be refused each time, the \
         first two in the same words. Every check is a log line beginning `HARNESS RESULT`.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            HarnessSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the harness supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the harness supervisor task failed: {e}"))?;
        Err("the harness supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(SecretHarness);
}
