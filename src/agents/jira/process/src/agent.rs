//! What jira-processor is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ConsumerSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

const JIRA_PROCESSOR_SUPERVISOR: &str = "jira.supervisor.processor";

pub struct JiraProcessor;

impl PolarAgent for JiraProcessor {
    const NAME: &'static str = "jira-processor";
    const PURPOSE: &'static str =
        "Writes the Jira projects, groups, users and issues the observer publishes into the graph";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const HELP: Option<&'static str> = Some(
        "jira-processor subscribes to the Jira observer's four topics on Cassini (projects, \
         groups, users, issues) and writes what it receives into the graph. It reads no \
         variables of its own: the graph's and the Cassini client's are declared by those \
         components.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(JIRA_PROCESSOR_SUPERVISOR);

        let (_, handle) = Actor::spawn(
            Some(JIRA_PROCESSOR_SUPERVISOR.to_string()),
            ConsumerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("failed to start {JIRA_PROCESSOR_SUPERVISOR}: {e}"))?;

        handle
            .await
            .map_err(|e| format!("{JIRA_PROCESSOR_SUPERVISOR} task failed: {e}"))?;

        // The supervisor stopping is never a success: a processor that no longer processes must
        // be restarted. `run` exits non-zero so the Job does.
        Err(format!("{JIRA_PROCESSOR_SUPERVISOR} stopped").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(JiraProcessor);
}
