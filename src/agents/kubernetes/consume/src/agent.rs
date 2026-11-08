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
use polar_agent_builder::{
    Capability, Cause, Configured, GraphDecl, PolarAgent, StartError, TopicDecl,
};
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
        TopicDecl::subscribe(
            kube_common::KUBERNETES_DISCOVERY_ANNOUNCE,
            "the UID of each cluster an observer watches, so the cluster is known before its first event",
        ),
        TopicDecl::publish(
            kube_common::KUBERNETES_DISCOVERY_QUERY,
            "a request that every observer say again which cluster it watches, when it registers",
        )
        .because(&[Cause::Registered]),
        TopicDecl::publish(
            polar::topics::PROVENANCE_DISCOVERY,
            "each container image it meets, for the resolver to resolve",
        )
        .because(&[Cause::Topic(kube_common::KUBERNETES_CONSUMER)]),
    ];
    // The labels it writes, each a node of polar::graph::nodes::kube::KubeNodeKey. Not `State`, the taxonomy anchor
    // every state instance is OF_TYPE, which build-processor writes too and which has no key.
    const GRAPH: &'static [GraphDecl] = &[
        GraphDecl::owns("KubernetesDeployment", "the deployments"),
        GraphDecl::owns("DeploymentState", "each state a deployment was seen in"),
        GraphDecl::owns("ReplicaSet", "the replica sets"),
        GraphDecl::owns("ReplicaSetState", "each state a replica set was seen in"),
        GraphDecl::owns("Pod", "the pods"),
        GraphDecl::owns("PodState", "each state a pod was seen in"),
        GraphDecl::owns("PodContainer", "each pod's containers"),
        GraphDecl::owns("PodContainerState", "each state a container was seen in"),
        GraphDecl::owns("Volume", "the volumes pods mount"),
        GraphDecl::owns("PersistentVolumeClaim", "the claims volumes are backed by"),
        GraphDecl::owns("Secret", "the secrets pods use, by name"),
        GraphDecl::owns("ConfigMap", "the config maps pods use, by name"),
        GraphDecl::owns("KubernetesJob", "the jobs"),
        GraphDecl::owns("KubernetesJobState", "each state a job was seen in"),
        GraphDecl::owns("StatefulSet", "the stateful sets that own pods, as anchors"),
        GraphDecl::owns("DaemonSet", "the daemon sets that own pods, as anchors"),
        GraphDecl::owns("KubernetesNode", "the nodes pods run on"),
        GraphDecl::owns("KubernetesNodeState", "each state a node was seen in"),
        GraphDecl::owns("KubernetesCluster", "each cluster an observer announced"),
        GraphDecl::owns(
            "KubernetesPolarAgent",
            "the PolarAgent resources the operator keeps, one per deployed agent",
        ),
        GraphDecl::owns(
            "KubernetesPolarAgentState",
            "each state a PolarAgent resource was seen in",
        ),
        GraphDecl::owns("Namespace", "the namespaces, by cluster"),
        GraphDecl::owns("NamespaceState", "each state a namespace was seen in"),
        GraphDecl::owns("FluxOCIRepository", "Flux's OCI repositories"),
        GraphDecl::owns(
            "FluxOCIRepositoryState",
            "each artifact an OCI repository resolved",
        ),
        GraphDecl::owns("FluxKustomization", "Flux's kustomizations"),
        GraphDecl::owns(
            "FluxKustomizationState",
            "each revision a kustomization applied",
        ),
    ];
    // The constraints for the labels above come from here, on what each is merged on.
    const GRAPH_KEYS: &'static [fn() -> Vec<polar::graph::controller::NodeShape>] =
        &[polar::graph::controller::shapes_of::<polar::graph::nodes::kube::KubeNodeKey>];
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

    /// Every label it owns is merged on a key its vocabulary names, so each gets its constraint or index at start;
    /// FluxOCIRepository, merged on its uid and, from a reference, on its name and namespace, gets an index for each.
    #[test]
    fn every_label_it_owns_gets_a_constraint_or_an_index() {
        use polar::graph::schema::KeyIndexKind;
        let indexes = polar_agent_builder::agent::graph_key_indexes::<KubeConsumer>();
        for decl in KubeConsumer::GRAPH {
            assert!(
                indexes.iter().any(|i| i.label == decl.label),
                "{} has none",
                decl.label
            );
        }
        let names: Vec<String> = indexes.iter().map(|i| i.name()).collect();
        assert!(
            names.contains(&"polar_unique_Pod_uid_cluster_uid".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(
                &"polar_unique_PodContainerState_pod_uid_name_valid_from_cluster_uid".to_string()
            ),
            "{names:?}"
        );
        assert!(
            indexes
                .iter()
                .filter(|i| i.label == "FluxOCIRepository")
                .all(|i| i.kind == KeyIndexKind::Index)
        );
    }
}
