/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! What kube-observer is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ClusterObserverSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::time::Duration;

pub struct KubeObserver;

impl PolarAgent for KubeObserver {
    const NAME: &'static str = crate::TCP_CLIENT_NAME;
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

        configured.init_logging("kubernetes.cluster.observer.supervisor");

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
