//! What kube-observer is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ClusterObserverSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

pub struct KubeObserver;

impl PolarAgent for KubeObserver {
    const NAME: &'static str = "kube-observer";
    const PURPOSE: &'static str =
        "Watches the Kubernetes API and publishes what it sees to Cassini";
    const NEEDS: &'static [Capability] = &[Capability::KubeApiRead];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const HELP: Option<&'static str> = Some(
        "kube-observer watches the cluster's resources through the Kubernetes API, with the \
         service account the deployment gives it (read access to everything, nothing else), and \
         publishes each change on Cassini for kube-consumer to project into the graph. The \
         cluster API address and its token are the ones Kubernetes gives every pod, so it reads \
         no variables of its own.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        // Required when multiple providers are present in the dependency tree.
        rustls::crypto::ring::default_provider()
            .install_default()
            .map_err(|_| "a rustls crypto provider was already installed")?;

        polar::init_logging("kubernetes.cluster.observer.supervisor".to_string());

        let (_, handle) = Actor::spawn(
            Some("kubernetes.cluster.observer.supervisor".to_string()),
            ClusterObserverSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("kube-observer failed to start: {e}"))?;

        handle
            .await
            .map_err(|e| format!("kube-observer supervisor task failed: {e}"))?;

        // The supervisor stopping is never a success: an observer that is no longer observing
        // must be restarted. `run` exits non-zero so the Job does.
        Err("the kube-observer supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(KubeObserver);
}
