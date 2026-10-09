//! What the credentials agent consumer is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.
//!
//! Its settings are a section of its own (`settings.rs`), read from the snapshot the builder took.

use crate::supervisor::{ConsumerSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct CredentialsConsumer;

impl PolarAgent for CredentialsConsumer {
    const NAME: &'static str = "credentials-agent-consumer";
    const PURPOSE: &'static str =
        "Issues certificates and keeps the credentials ledger: the cluster's certificate authority front end";
    // The graph is declared as a section of its own, not required as a capability: this agent
    // projects to the graph but its health must not depend on Neo4j (the ledger is its source of
    // truth and the graph only a projection).
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[
        &polar::graph::environment::SECTION,
        &crate::settings::SECTION,
    ];
    const HELP: Option<&'static str> = Some(
        "The credentials agent consumer answers the certificate, identity, account, secret and \
         escrow-key requests the cluster's agents make over Cassini. The ledger is its source of \
         truth, and the graph a projection of it.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging("credentials-agent-consumer");
        let (_agent, handle) = Actor::spawn(
            Some("credentials-agent-consumer.supervisor".to_string()),
            ConsumerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the consumer supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the consumer supervisor task failed: {e}"))?;
        // The supervisor is meant to live as long as the pod; `run` exits non-zero, so the Job
        // starts a fresh pod, which reruns polar-init and rebuilds every client from fresh
        // certificates.
        Err("the consumer supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(CredentialsConsumer);
}
