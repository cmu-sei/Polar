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

//! What kube-consumer is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`.

use crate::supervisor::{ClusterConsumerSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError, TopicDecl};
use ractor::Actor;
use std::time::Duration;

pub struct KubeConsumer;

impl PolarAgent for KubeConsumer {
    const NAME: &'static str = crate::BROKER_CLIENT_NAME;
    const PURPOSE: &'static str =
        "Projects the Kubernetes resources the observer publishes into the Polar knowledge graph";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite, Capability::Encryption];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            kube_common::KUBERNETES_CONSUMER,
            "the observer's resource changes, projected into the graph",
        ),
        TopicDecl::subscribe(
            polar::topics::KUBERNETES_RESOLUTION_EVENTS,
            "what the resolver found for a container's image, joined to the container",
        ),
        TopicDecl::publish(
            polar::topics::PROVENANCE_DISCOVERY,
            "each container image it meets, for the resolver to resolve",
        ),
    ];
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
