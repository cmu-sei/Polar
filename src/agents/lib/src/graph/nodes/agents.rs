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

//! Graph keys for agents, independent of where they are deployed.
//!
//! `PolarAgent` is the canonical node of an agent: what its code is, named by the name
//! the code declares. It carries nothing about any deployment target. A target's own
//! node for the object that deploys it (for Kubernetes, the kube consumer's
//! `KubernetesPolarAgent`) points at it with `DEPLOYS`, and holds the deployment's facts,
//! its phase history and its instances.
//!
//! ```text
//! (deployment)-[:DEPLOYS]->(:PolarAgent {name})
//! (deployment)-[:INSTANCE_ENDED]->(:AgentInstanceEnding)<-[:ENDED_WITH]-(instance)
//! ```

use crate::graph::controller::{GraphNodeKey, NodeIdentity, NodeKeyVocabulary};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentNodeKey {
    /// The canonical node of one agent.
    PolarAgent { name: String },
    /// How one instance ended. `instance_id` is the target's identifier for the
    /// instance (a pod's UID), or its name when the identifier is not known.
    InstanceEnding {
        instance_id: String,
        finished_at: String,
    },
}

impl GraphNodeKey for AgentNodeKey {
    fn identity(&self) -> NodeIdentity {
        match self {
            AgentNodeKey::PolarAgent { name } => {
                NodeIdentity::fixed(&["PolarAgent"], vec![("name", name.clone().into())])
            }
            AgentNodeKey::InstanceEnding {
                instance_id,
                finished_at,
            } => NodeIdentity::fixed(
                &["AgentInstanceEnding"],
                vec![
                    ("instance_id", instance_id.clone().into()),
                    ("finished_at_key", finished_at.clone().into()),
                ],
            ),
        }
    }
}

impl NodeKeyVocabulary for AgentNodeKey {
    fn every_variant() -> Vec<Self> {
        let all = vec![
            AgentNodeKey::PolarAgent { name: "x".into() },
            AgentNodeKey::InstanceEnding {
                instance_id: "x".into(),
                finished_at: "x".into(),
            },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                AgentNodeKey::PolarAgent { .. } | AgentNodeKey::InstanceEnding { .. } => {}
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::controller::pattern_problem;

    #[test]
    fn every_key_keeps_the_pattern_rules() {
        for key in AgentNodeKey::every_variant() {
            for variable in ["n", "m", "n0"] {
                assert_eq!(pattern_problem(&key, variable), None, "{key:?}");
            }
        }
    }
}
