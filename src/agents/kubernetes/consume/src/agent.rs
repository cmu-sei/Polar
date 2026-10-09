//! What kube-consumer is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ClusterConsumerSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

pub struct KubeConsumer;

impl PolarAgent for KubeConsumer {
    const NAME: &'static str = "kube-consumer";
    const PURPOSE: &'static str =
        "Projects the Kubernetes resources the observer publishes into the Polar knowledge graph";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite, Capability::Encryption];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const HELP: Option<&'static str> = Some(
        "kube-consumer subscribes to the Kubernetes observer's topics on Cassini, writes what it \
         receives into the graph (nodes, ownership edges and temporal state), and answers the \
         resolution requests the observer sends it. It needs the graph (GRAPH_*) and an escrow \
         key for the secrets it handles.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging("kubernetes.cluster.consumer.supervisor");

        rustls::crypto::ring::default_provider()
            .install_default()
            .map_err(|_| "a rustls crypto provider was already installed")?;

        let (_, handle) = Actor::spawn(
            Some("kubernetes.cluster.supervisor".to_string()),
            ClusterConsumerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("kube-consumer failed to start: {e}"))?;

        handle
            .await
            .map_err(|e| format!("kube-consumer supervisor task failed: {e}"))?;

        // The supervisor stopping is never a success: a consumer that is no longer consuming
        // must be restarted. `run` exits non-zero so the Job does.
        Err("the kube-consumer supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(KubeConsumer);
}
