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

//! The operator's `PolarAgent` resource: the Kubernetes deployment of an agent.
//!
//! Projected like every watched resource (a UID-keyed anchor and its namespace), plus
//! one edge out of the Kubernetes schema: `DEPLOYS` to the canonical `PolarAgent`, which
//! is only the name the agent's code declares (`spec.agent`, else this resource's name)
//! and knows nothing of Kubernetes. The agent processor writes the same edge from the
//! operator's lifecycle events, so the tie holds whichever arrives first. The agent's
//! pods name this resource as their owner, so `handle_owner_refs` gives them their
//! `OWNS` edge from it.
//!
//! The deployment's phase history is the agent processor's: the operator's events say
//! what each phase meant, where this resource's status is only its latest copy. This
//! projection writes one state of its own, the resource's deletion.

use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::agents::AgentNodeKey;
use polar::graph::nodes::kube::KubeNodeKey;
use polar_operator_common::PolarAgent;
use ractor::ActorProcessingErr;

use crate::supervisor::ProjectionCache;

use super::{GraphOperable, now_ms, project_deletion_state, ts};

const KIND: &str = polar_operator_common::KIND;

impl GraphOperable for PolarAgent {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
        _cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let agent = self.agent_name();
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());
        let resource_key = KubeNodeKey::PolarAgent {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: resource_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                Property("namespace".into(), GraphValue::String(namespace.clone())),
                Property("agent".into(), GraphValue::String(agent.clone())),
                Property("image".into(), GraphValue::String(self.spec.image.clone())),
                Property(
                    "generation".into(),
                    GraphValue::I64(self.metadata.generation.unwrap_or_default()),
                ),
                ts("observed_at", now_ms()),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: resource_key.clone().into_key(),
            rel_type: "IN_NAMESPACE".into(),
            to: KubeNodeKey::Namespace {
                name: namespace,
                cluster_uid: cluster_uid.to_string(),
            }
            .into_key(),
            props: Vec::new(),
        }))?;

        // ---- The agent it deploys ----

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: resource_key.into_key(),
            rel_type: "DEPLOYS".into(),
            to: AgentNodeKey::PolarAgent { name: agent }.into_key(),
            props: Vec::new(),
        }))?;

        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();
        project_deletion_state(
            graph,
            KubeNodeKey::PolarAgent {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::PolarAgentResourceState {
                uid: uid.clone(),
                valid_from: now,
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )?;
        cache.evict(KIND.to_string(), cluster_uid.to_string(), &uid);
        Ok(())
    }
}
