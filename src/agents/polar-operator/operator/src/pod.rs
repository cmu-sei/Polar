//! The pod an agent runs as, and what a pod says about the instance in it.
//!
//! Secure by default: every instance runs non-root, with a read-only root filesystem, no
//! capabilities, the runtime's seccomp profile and no service-account token mounted. A
//! writable `/tmp` is the only scratch space. An agent that needs more asks for it in its
//! spec (not yet expressible: that is the catalog's capabilities, later).

use crate::crd::{Ending, EndingKind, PolarAgent, PolarAgentSpec};
use crate::lifecycle::{Instance, InstanceState};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, ContainerStatus, EmptyDirVolumeSource, EnvVar, Pod, PodSpec,
    SeccompProfile, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{Resource, ResourceExt};
use polar_operator_common::{AGENT_LABEL, SPEC_HASH_LABEL};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The agent's container, by name, in every pod.
pub const CONTAINER: &str = "agent";
/// Added to the shutdown budget for the pod's grace period, so Kubernetes never kills an
/// agent that is still inside its budget.
pub const GRACE_MARGIN_SECS: u32 = 10;
/// The longest an instance's own account of its ending is kept.
const MESSAGE_LIMIT: usize = 400;

/// What identifies a spec: anything that changes the pod. `suspend` does not.
pub fn spec_hash(spec: &PolarAgentSpec) -> String {
    let mut shaped = spec.clone();
    shaped.suspend = false;
    let bytes = serde_json::to_vec(&shaped).expect("a spec serializes");
    let digest = Sha256::digest(bytes);
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

pub fn service_account(agent: &PolarAgent) -> String {
    agent
        .spec
        .service_account
        .clone()
        .unwrap_or_else(|| agent.name_any())
}

pub fn build(agent: &PolarAgent, pod_name: &str, hash: &str) -> Pod {
    let spec = &agent.spec;
    let name = agent.name_any();

    let mut env = vec![
        var("POLAR_AGENT_NAME", &agent.agent_name()),
        var("POLAR_CAPABILITIES", &spec.needs.join(",")),
        var(
            "POLAR_SHUTDOWN_BUDGET_SECS",
            &spec.shutdown_budget_secs.to_string(),
        ),
    ];
    env.extend(spec.env.iter().map(|e| var(&e.name, &e.value)));

    let labels = BTreeMap::from([
        (AGENT_LABEL.to_string(), name.clone()),
        (SPEC_HASH_LABEL.to_string(), hash.to_string()),
    ]);

    Pod {
        metadata: ObjectMeta {
            name: Some(pod_name.to_string()),
            namespace: agent.namespace(),
            labels: Some(labels),
            owner_references: agent.controller_owner_ref(&()).map(|o| vec![o]),
            ..Default::default()
        },
        spec: Some(PodSpec {
            restart_policy: Some("Never".into()),
            service_account_name: Some(service_account(agent)),
            automount_service_account_token: Some(false),
            termination_grace_period_seconds: Some(
                (spec.shutdown_budget_secs + GRACE_MARGIN_SECS).into(),
            ),
            containers: vec![Container {
                name: CONTAINER.into(),
                image: Some(spec.image.clone()),
                command: non_empty(&spec.command),
                args: non_empty(&spec.args),
                env: Some(env),
                // An agent that writes no termination message still says something: the
                // tail of its log becomes the message.
                termination_message_policy: Some("FallbackToLogsOnError".into()),
                security_context: Some(hardened()),
                volume_mounts: Some(vec![VolumeMount {
                    name: "tmp".into(),
                    mount_path: "/tmp".into(),
                    ..Default::default()
                }]),
                ..Default::default()
            }],
            volumes: Some(vec![Volume {
                name: "tmp".into(),
                empty_dir: Some(EmptyDirVolumeSource::default()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        status: None,
    }
}

fn hardened() -> SecurityContext {
    SecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(65532),
        run_as_group: Some(65532),
        read_only_root_filesystem: Some(true),
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".into()]),
            add: None,
        }),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".into(),
            localhost_profile: None,
        }),
        ..Default::default()
    }
}

fn var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: Some(value.into()),
        value_from: None,
    }
}

fn non_empty(list: &[String]) -> Option<Vec<String>> {
    (!list.is_empty()).then(|| list.to_vec())
}

/// What a pod says about its instance.
pub fn view(pod: &Pod) -> Instance {
    let name = pod.name_any();
    let spec_hash = pod.labels().get(SPEC_HASH_LABEL).cloned();
    let terminating = pod.metadata.deletion_timestamp.is_some();
    let status = pod.status.as_ref();
    let container: Option<&ContainerStatus> = status
        .and_then(|s| s.container_statuses.as_ref())
        .and_then(|cs| cs.iter().find(|c| c.name == CONTAINER));

    let state = match container.and_then(|c| c.state.as_ref()) {
        Some(s) if s.terminated.is_some() => {
            let t = s.terminated.as_ref().unwrap();
            let started = t.started_at.as_ref().map(|t| t.0);
            let finished = t.finished_at.as_ref().map(|t| t.0);
            let ran_secs = match (started, finished) {
                (Some(s), Some(f)) => f.as_second() - s.as_second(),
                _ => 0,
            };
            let reason = t.reason.clone().unwrap_or_default();
            InstanceState::Ended(Ending {
                pod: name.clone(),
                pod_uid: pod.metadata.uid.clone(),
                kind: classify(t.exit_code, &reason),
                exit_code: t.exit_code,
                reason,
                message: t.message.as_deref().and_then(trimmed),
                ran_secs,
                finished_at: finished
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            })
        }
        Some(s) if s.running.is_some() => InstanceState::Running,
        Some(s) => InstanceState::Waiting {
            reason: s.waiting.as_ref().map(|w| {
                let reason = w.reason.clone().unwrap_or_default();
                match &w.message {
                    Some(m) => format!("{reason}: {m}"),
                    None => reason,
                }
            }),
        },
        None => match status.and_then(|s| s.phase.as_deref()) {
            // Gone from under the instance before its container reported anything:
            // evicted, or its node lost.
            Some("Failed") => InstanceState::Ended(Ending {
                pod: name.clone(),
                pod_uid: pod.metadata.uid.clone(),
                kind: EndingKind::Lost,
                exit_code: -1,
                reason: status
                    .and_then(|s| s.reason.clone())
                    .unwrap_or_else(|| "Failed".into()),
                message: status.and_then(|s| s.message.as_deref()).and_then(trimmed),
                ran_secs: 0,
                finished_at: chrono::Utc::now().to_rfc3339(),
            }),
            _ => InstanceState::Waiting {
                reason: Some("Scheduling".into()),
            },
        },
    };

    Instance {
        pod: name,
        spec_hash,
        terminating,
        state,
    }
}

/// How an instance ended, from its exit code and Kubernetes' reason.
pub fn classify(exit_code: i32, reason: &str) -> EndingKind {
    match (exit_code, reason) {
        (_, "OOMKilled") => EndingKind::OomKilled,
        (0, _) => EndingKind::Completed,
        (137, _) => EndingKind::Killed,
        _ => EndingKind::Exited,
    }
}

fn trimmed(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Keep the end: the last lines of a log say the most about how it ended.
    let start = text
        .char_indices()
        .rev()
        .nth(MESSAGE_LIMIT.saturating_sub(1))
        .map(|(i, _)| i)
        .unwrap_or(0);
    Some(text[start..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> PolarAgent {
        let mut agent = PolarAgent::new(
            "demo",
            PolarAgentSpec {
                image: "busybox:1.37".into(),
                command: vec![],
                args: vec![],
                env: vec![],
                agent: None,
                needs: vec!["cassini".into(), "graph-write".into()],
                shutdown_budget_secs: 20,
                service_account: None,
                suspend: false,
            },
        );
        agent.metadata.namespace = Some("polar".into());
        agent.metadata.uid = Some("uid-1".into());
        agent
    }

    #[test]
    fn suspending_does_not_change_the_hash() {
        let mut a = agent();
        let before = spec_hash(&a.spec);
        a.spec.suspend = true;
        assert_eq!(spec_hash(&a.spec), before);
        a.spec.image = "busybox:1.36".into();
        assert_ne!(spec_hash(&a.spec), before);
    }

    #[test]
    fn the_pod_is_hardened_owned_and_given_the_contract() {
        let pod = build(&agent(), "demo-1", "abc");
        let spec = pod.spec.unwrap();
        assert_eq!(spec.termination_grace_period_seconds, Some(30));
        assert_eq!(spec.automount_service_account_token, Some(false));
        assert_eq!(spec.service_account_name.as_deref(), Some("demo"));
        let container = &spec.containers[0];
        let sc = container.security_context.as_ref().unwrap();
        assert_eq!(sc.read_only_root_filesystem, Some(true));
        assert_eq!(sc.run_as_non_root, Some(true));
        let env = container.env.as_ref().unwrap();
        let caps = env.iter().find(|e| e.name == "POLAR_CAPABILITIES").unwrap();
        assert_eq!(caps.value.as_deref(), Some("cassini,graph-write"));
        let owner = &pod.metadata.owner_references.unwrap()[0];
        assert_eq!(owner.kind, "PolarAgent");
        assert_eq!(owner.controller, Some(true));
    }

    #[test]
    fn endings_are_classified() {
        assert_eq!(classify(1, "Error"), EndingKind::Exited);
        assert_eq!(classify(0, "Completed"), EndingKind::Completed);
        assert_eq!(classify(137, "Error"), EndingKind::Killed);
        assert_eq!(classify(137, "OOMKilled"), EndingKind::OomKilled);
    }

    #[test]
    fn a_long_message_keeps_its_end() {
        let long = format!("{}the reason", "x".repeat(1000));
        let kept = trimmed(&long).unwrap();
        assert_eq!(kept.chars().count(), MESSAGE_LIMIT);
        assert!(kept.ends_with("the reason"));
    }
}
