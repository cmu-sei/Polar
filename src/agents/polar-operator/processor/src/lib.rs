//! agent-processor: the single writer of agent lifecycle facts into the graph.
//!
//! It reads the [`AgentLifecycleEvent`]s an operator publishes and writes, for each
//! one, the canonical `PolarAgent` node, the deployment that deploys it, the instances
//! the deployment started and how each ended. [`project`] is the whole of what it
//! decides, and is pure; [`supervisor`] only decodes and sends the operations in order.
//!
//! ```text
//! (deployment)-[:DEPLOYS]->(:PolarAgent {name})
//! (deployment)-[:STARTED_INSTANCE {number, spec_hash, started_at}]->(instance)
//! (deployment)-[:TRANSITIONED_TO]->(deployment state {phase, message})-[:OF_TYPE]->(:State)
//! (deployment)-[:INSTANCE_ENDED]->(:AgentInstanceEnding)<-[:ENDED_WITH]-(instance)
//! ```
//!
//! The canonical agent carries nothing about where it runs: it is only its name, the
//! one its code declares. Which nodes a deployment and an instance are depends on the
//! target, and is decided in [`deployment_key`], [`deployment_state_key`] and
//! [`instance_key`] alone. For Kubernetes they are the kube consumer's own nodes
//! (`KubernetesPolarAgent`, `Pod`), matched by the same keys, so the two agents' writes
//! meet whichever arrives first.
//!
//! Capabilities are not written: what a deployment is configured to give an agent
//! asserts nothing about what is present, and the agent judges that itself. A refusal
//! is an ending like any other, with its exit code and the agent's own message.

pub mod supervisor;

use polar::graph::controller::{GraphNodeKey, GraphOp, GraphValue, IntoGraphKey, Property};
use polar::graph::nodes::agents::AgentNodeKey;
use polar::graph::nodes::kube::KubeNodeKey;
use polar::{AgentLifecycleEvent, DeploymentRef, InstanceRef};

pub const AGENT_PROCESSOR_NAME: &str = "polar.agents.processor";

/// The graph operations one event becomes, in the order they must run.
pub fn project(event: &AgentLifecycleEvent) -> Vec<GraphOp> {
    let deployment = event.deployment();
    let mut ops = vec![
        // The agent, and that this deployment deploys it, follow from every event.
        upsert(canonical_key(event.agent()).into_key(), vec![]),
        edge(
            deployment_key(deployment),
            "DEPLOYS",
            canonical_key(event.agent()).into_key(),
            vec![],
        ),
    ];

    match event {
        AgentLifecycleEvent::DeploymentDeclared {
            spec_hash,
            image,
            shutdown_budget_secs,
            identity,
            observed_at,
            ..
        } => {
            ops.push(upsert(
                deployment_key(deployment),
                vec![
                    text("image", image),
                    text("spec_hash", spec_hash),
                    prop(
                        "shutdown_budget_secs",
                        GraphValue::I64(*shutdown_budget_secs as i64),
                    ),
                    text("identity", identity),
                    ms("declared_at", *observed_at),
                ],
            ));
        }

        AgentLifecycleEvent::DeploymentPhaseChanged {
            phase,
            message,
            observed_at,
            ..
        } => {
            ops.push(GraphOp::UpdateState {
                resource_key: deployment_key(deployment),
                state_type_key: KubeNodeKey::State.into_key(),
                state_instance_key: deployment_state_key(deployment, *observed_at),
                state_instance_props: vec![
                    text("phase", phase),
                    optional("message", message),
                    // The operator attests the moment it decided the phase.
                    ms("valid_from", *observed_at),
                    ms("observed_at", *observed_at),
                ],
            });
        }

        AgentLifecycleEvent::InstanceStarted {
            instance,
            number,
            spec_hash,
            started_at,
            ..
        } => {
            ops.push(edge(
                deployment_key(deployment),
                "STARTED_INSTANCE",
                instance_key(instance),
                vec![
                    prop("number", GraphValue::I64(*number as i64)),
                    text("spec_hash", spec_hash),
                    ms("started_at", *started_at),
                ],
            ));
        }

        AgentLifecycleEvent::InstanceEnded {
            instance,
            kind,
            exit_code,
            reason,
            message,
            ran_secs,
            finished_at,
            ..
        } => {
            let ending = AgentNodeKey::InstanceEnding {
                instance_id: match instance {
                    Some(i) => instance_id(i),
                    // The target no longer says which instance it was; the ending still
                    // belongs to the deployment.
                    None => format!("{}:unknown", deployment_id(deployment)),
                },
                finished_at: finished_at.to_string(),
            };
            ops.push(upsert(
                ending.clone().into_key(),
                vec![
                    text("kind", kind),
                    prop("exit_code", GraphValue::I64(*exit_code as i64)),
                    text("reason", reason),
                    optional("message", message),
                    prop("ran_secs", GraphValue::I64(*ran_secs)),
                    ms("finished_at", *finished_at),
                ],
            ));
            ops.push(edge(
                deployment_key(deployment),
                "INSTANCE_ENDED",
                ending.clone().into_key(),
                vec![],
            ));
            if let Some(instance) = instance {
                ops.push(edge(
                    instance_key(instance),
                    "ENDED_WITH",
                    ending.into_key(),
                    vec![],
                ));
            }
        }
    }
    ops
}

/// The canonical node of an agent: its name, and nothing about where it runs.
pub fn canonical_key(agent: &str) -> AgentNodeKey {
    AgentNodeKey::PolarAgent {
        name: agent.to_string(),
    }
}

// ── Where a target is named ────────────────────────────────────────────────────

/// The node of the object that deploys the agent on its target.
pub fn deployment_key(deployment: &DeploymentRef) -> Box<dyn GraphNodeKey> {
    match deployment {
        DeploymentRef::Kubernetes {
            cluster_uid, uid, ..
        } => KubeNodeKey::PolarAgent {
            uid: uid.clone(),
            cluster_uid: cluster_uid.clone(),
        }
        .into_key(),
    }
}

/// One phase of the deployment, from `valid_from` on.
pub fn deployment_state_key(deployment: &DeploymentRef, valid_from: i64) -> Box<dyn GraphNodeKey> {
    match deployment {
        DeploymentRef::Kubernetes {
            cluster_uid, uid, ..
        } => KubeNodeKey::PolarAgentResourceState {
            uid: uid.clone(),
            valid_from: valid_from.to_string(),
            cluster_uid: cluster_uid.clone(),
        }
        .into_key(),
    }
}

/// The node of one instance on its target.
pub fn instance_key(instance: &InstanceRef) -> Box<dyn GraphNodeKey> {
    match instance {
        InstanceRef::KubernetesPod {
            cluster_uid,
            pod_uid,
            ..
        } => KubeNodeKey::Pod {
            uid: pod_uid.clone(),
            cluster_uid: cluster_uid.clone(),
        }
        .into_key(),
    }
}

fn instance_id(instance: &InstanceRef) -> String {
    match instance {
        InstanceRef::KubernetesPod { pod_uid, .. } => pod_uid.clone(),
    }
}

fn deployment_id(deployment: &DeploymentRef) -> String {
    match deployment {
        DeploymentRef::Kubernetes { uid, .. } => uid.clone(),
    }
}

// ── Op builders ────────────────────────────────────────────────────────────────

fn upsert(key: Box<dyn GraphNodeKey>, props: Vec<Property>) -> GraphOp {
    GraphOp::UpsertNode { key, props }
}

fn edge(
    from: Box<dyn GraphNodeKey>,
    rel_type: &str,
    to: Box<dyn GraphNodeKey>,
    props: Vec<Property>,
) -> GraphOp {
    GraphOp::EnsureEdge {
        from,
        rel_type: rel_type.to_string(),
        to,
        props,
    }
}

fn prop(name: &str, value: GraphValue) -> Property {
    Property(name.to_string(), value)
}

fn text(name: &str, value: &str) -> Property {
    prop(name, GraphValue::String(value.to_string()))
}

/// Epoch milliseconds, as every temporal property in the kube schema.
fn ms(name: &str, value: i64) -> Property {
    prop(name, GraphValue::I64(value))
}

fn optional(name: &str, value: &Option<String>) -> Property {
    prop(
        name,
        value
            .clone()
            .map(GraphValue::String)
            .unwrap_or(GraphValue::Null),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment() -> DeploymentRef {
        DeploymentRef::Kubernetes {
            cluster_uid: "cluster".into(),
            namespace: "polar-demo".into(),
            name: "steady".into(),
            uid: "cr-uid".into(),
        }
    }

    fn pod() -> InstanceRef {
        InstanceRef::KubernetesPod {
            cluster_uid: "cluster".into(),
            pod_uid: "pod-uid".into(),
            pod_name: "steady-3".into(),
        }
    }

    fn label(key: &dyn GraphNodeKey) -> String {
        let pattern = key.cypher_match("x").0;
        pattern
            .trim_start_matches("(x:")
            .split([' ', ')'])
            .next()
            .unwrap()
            .to_string()
    }

    /// Each op as (kind, from-or-node label, relationship, to label).
    fn labels(ops: &[GraphOp]) -> Vec<(String, String, String, String)> {
        ops.iter()
            .map(|op| match op {
                GraphOp::UpsertNode { key, .. } => (
                    "upsert".into(),
                    label(key.as_ref()),
                    String::new(),
                    String::new(),
                ),
                GraphOp::EnsureEdge {
                    from, rel_type, to, ..
                } => (
                    "edge".into(),
                    label(from.as_ref()),
                    rel_type.clone(),
                    label(to.as_ref()),
                ),
                GraphOp::UpdateState {
                    resource_key,
                    state_instance_key,
                    ..
                } => (
                    "state".into(),
                    label(resource_key.as_ref()),
                    "TRANSITIONED_TO".into(),
                    label(state_instance_key.as_ref()),
                ),
                other => panic!("unexpected op {other:?}"),
            })
            .collect()
    }

    fn row(k: &str, a: &str, r: &str, b: &str) -> (String, String, String, String) {
        (k.into(), a.into(), r.into(), b.into())
    }

    fn every_event() -> Vec<AgentLifecycleEvent> {
        vec![
            AgentLifecycleEvent::DeploymentDeclared {
                agent: "steady".into(),
                deployment: deployment(),
                spec_hash: "abc".into(),
                image: "busybox:1.37".into(),
                shutdown_budget_secs: 5,
                identity: "steady".into(),
                observed_at: 1,
            },
            AgentLifecycleEvent::DeploymentPhaseChanged {
                agent: "steady".into(),
                deployment: deployment(),
                phase: "Running".into(),
                message: None,
                observed_at: 1,
            },
            AgentLifecycleEvent::InstanceStarted {
                agent: "steady".into(),
                deployment: deployment(),
                instance: pod(),
                number: 3,
                spec_hash: "abc".into(),
                started_at: 1,
            },
            AgentLifecycleEvent::InstanceEnded {
                agent: "steady".into(),
                deployment: deployment(),
                instance: Some(pod()),
                kind: "Exited".into(),
                exit_code: 1,
                reason: "Error".into(),
                message: Some("rejuvenating".into()),
                ran_secs: 45,
                finished_at: 2,
            },
        ]
    }

    #[test]
    fn the_agent_is_touched_only_as_what_a_deployment_deploys() {
        for event in every_event() {
            for (kind, from, rel, to) in labels(&project(&event)) {
                if from == "PolarAgent" {
                    assert_eq!(
                        kind, "upsert",
                        "{event:?}: PolarAgent is never the source of an edge"
                    );
                }
                if to == "PolarAgent" {
                    assert_eq!(rel, "DEPLOYS", "{event:?}");
                }
            }
        }
    }

    #[test]
    fn the_canonical_node_carries_no_properties() {
        for event in every_event() {
            for op in project(&event) {
                if let GraphOp::UpsertNode { key, props } = &op
                    && label(key.as_ref()) == "PolarAgent"
                {
                    assert!(props.is_empty(), "{props:?}");
                }
            }
        }
    }

    #[test]
    fn a_phase_is_a_state_of_the_deployment() {
        let ops = project(&every_event()[1]);
        assert_eq!(
            labels(&ops)[2],
            row(
                "state",
                "KubernetesPolarAgent",
                "TRANSITIONED_TO",
                "KubernetesPolarAgentState"
            )
        );
    }

    #[test]
    fn an_instance_is_the_kube_consumers_pod() {
        let ops = project(&every_event()[2]);
        assert_eq!(
            labels(&ops)[2],
            row("edge", "KubernetesPolarAgent", "STARTED_INSTANCE", "Pod")
        );
    }

    #[test]
    fn an_ending_hangs_off_the_deployment_and_its_instance() {
        let ops = project(&every_event()[3]);
        assert_eq!(
            labels(&ops)[2..],
            [
                row("upsert", "AgentInstanceEnding", "", ""),
                row(
                    "edge",
                    "KubernetesPolarAgent",
                    "INSTANCE_ENDED",
                    "AgentInstanceEnding"
                ),
                row("edge", "Pod", "ENDED_WITH", "AgentInstanceEnding"),
            ]
        );
    }

    #[test]
    fn an_ending_without_its_instance_still_reaches_the_deployment() {
        let AgentLifecycleEvent::InstanceEnded {
            agent,
            deployment,
            kind,
            exit_code,
            reason,
            message,
            ran_secs,
            finished_at,
            ..
        } = every_event().remove(3)
        else {
            unreachable!()
        };
        let ops = project(&AgentLifecycleEvent::InstanceEnded {
            agent,
            deployment,
            instance: None,
            kind,
            exit_code,
            reason,
            message,
            ran_secs,
            finished_at,
        });
        assert_eq!(labels(&ops).len(), 4);
    }
}
