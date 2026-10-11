//! What the scheduler processor is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::supervisor::{SERVICE_NAME, SHUTDOWN_BUDGET, SchedulerSupervisor};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

pub struct SchedulerProcessor;

impl PolarAgent for SchedulerProcessor {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str =
        "Records schedule changes in the graph and notifies the agents they concern";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const HELP: Option<&'static str> = Some(
        "The processor subscribes to the schedule changes the scheduler observer publishes \
         (scheduler.in) and to the announcements of ad-hoc agents (scheduler.adhoc). It stores \
         each schedule in the graph and, when a schedule's version has changed, publishes a \
         notification on the topic of the agent or agent type it is for. It needs the graph \
         (GRAPH_*).",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            SchedulerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the scheduler supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the scheduler supervisor task failed: {e}"))?;
        // A processor that is no longer processing must be restarted.
        Err("the scheduler supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(SchedulerProcessor);
}
