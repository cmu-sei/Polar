use crate::supervisor::{SERVICE_NAME, SHUTDOWN_BUDGET, SchedulerObserverSupervisor};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct SchedulerObserver;

impl PolarAgent for SchedulerObserver {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "Keeps a checkout of the schedules repository current and publishes each schedule change on Cassini";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
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
