//! The pod an agent runs as, and what a pod says about the instance in it.
//!
//! Secure by default: every instance runs non-root, with a read-only root filesystem, no
//! capabilities, the runtime's seccomp profile and no service-account token mounted. A
//! writable `/tmp` is the only scratch space. An agent that needs more asks for it in its
//! spec (not yet expressible: that is the catalog's capabilities, later).

use crate::crd::{Ending, EndingKind, PodOverlay, PolarAgent, PolarAgentSpec};
use crate::lifecycle::{Instance, InstanceState};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, ContainerStatus, EmptyDirVolumeSource, EnvVar, Pod, PodSecurityContext,
    PodSpec, SeccompProfile, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{Resource, ResourceExt};
use polar_operator_common::{AGENT_LABEL, SPEC_HASH_LABEL};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The agent's container, by name, in every pod.
pub const CONTAINER: &str = "agent";
/// The operator's own scratch volume, mounted at [`TMP_PATH`]. An overlay cannot reuse the name or
/// the path.
pub const TMP_VOLUME: &str = "tmp";
pub const TMP_PATH: &str = "/tmp";
/// The user and group an agent runs as unless its overlay names another.
pub const DEFAULT_UID: i64 = 65532;
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

/// What is wrong with a spec's overlay, in words for the person who wrote it, or nothing. The
/// operator refuses to start a pod from a spec that has any of these, and says so in the status.
pub fn overlay_problems(spec: &PolarAgentSpec) -> Vec<String> {
    let Some(overlay) = &spec.pod else {
        return Vec::new();
    };
    let mut problems = Vec::new();

    if overlay.run_as_user == Some(0) {
        problems.push("pod.runAsUser is 0: an agent never runs as root".to_string());
    }
    if let Some(policy) = &overlay.image_pull_policy
        && !["Always", "IfNotPresent", "Never"].contains(&policy.as_str())
    {
        problems.push(format!(
            "pod.imagePullPolicy is '{policy}': expected Always, IfNotPresent or Never"
        ));
    }

    let mut volumes: Vec<&str> = vec![TMP_VOLUME];
    for volume in &overlay.volumes {
        if volume.name == TMP_VOLUME {
            problems.push(format!(
                "pod.volumes names a volume '{TMP_VOLUME}', which is the operator's scratch space"
            ));
        } else if volumes.contains(&volume.name.as_str()) {
            problems.push(format!("pod.volumes names '{}' twice", volume.name));
        } else {
            volumes.push(&volume.name);
        }
    }
    for mount in &overlay.volume_mounts {
        if mount.mount_path == TMP_PATH {
            problems.push(format!(
                "pod.volumeMounts mounts '{}' at {TMP_PATH}, which is the operator's scratch space",
                mount.name
            ));
        }
        if !volumes.contains(&mount.name.as_str()) {
            problems.push(format!(
                "pod.volumeMounts mounts '{}', which is not one of pod.volumes",
                mount.name
            ));
        }
    }

    let mut inits: Vec<&str> = Vec::new();
    for init in &overlay.init_containers {
        if init.name.is_empty() {
            problems.push("an entry of pod.initContainers has no name".to_string());
        } else if init.name == CONTAINER || inits.contains(&init.name.as_str()) {
            problems.push(format!(
                "pod.initContainers names '{}', which is already a container of the pod",
                init.name
            ));
        } else {
            inits.push(&init.name);
        }
        for mount in init.volume_mounts.iter().flatten() {
            if !volumes.contains(&mount.name.as_str()) {
                problems.push(format!(
                    "init container '{}' mounts '{}', which is not one of pod.volumes",
                    init.name, mount.name
                ));
            }
        }
    }
    problems
}

pub fn build(agent: &PolarAgent, pod_name: &str, hash: &str) -> Pod {
    let spec = &agent.spec;
    let name = agent.name_any();
    let overlay = spec.pod.clone().unwrap_or_else(PodOverlay::default);
    let uid = overlay.run_as_user.unwrap_or(DEFAULT_UID);

    let mut env = vec![
        var("POLAR_AGENT_NAME", &agent.agent_name()),
        var("POLAR_CAPABILITIES", &spec.needs.join(",")),
        var(
            "POLAR_SHUTDOWN_BUDGET_SECS",
            &spec.shutdown_budget_secs.to_string(),
        ),
    ];
    env.extend(spec.env.iter().map(|e| var(&e.name, &e.value)));
    env.extend(overlay.env.iter().cloned());

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
            security_context: overlay.fs_group.map(|group| PodSecurityContext {
                fs_group: Some(group),
                ..Default::default()
            }),
            automount_service_account_token: Some(overlay.automount_service_account_token.unwrap_or(false)),
            // Kubernetes puts a variable for every Service of the namespace into every pod (the old
            // Docker-link scheme: a Service `credentials-agent-observer` becomes
            // `CREDENTIALS_AGENT_OBSERVER_PORT=tcp://10.0.0.1:8080`), which collides with an agent's own
            // settings of the same name. An agent is told where things are, explicitly.
            enable_service_links: Some(false),
            termination_grace_period_seconds: Some(
                (spec.shutdown_budget_secs + GRACE_MARGIN_SECS).into(),
            ),
            init_containers: (!overlay.init_containers.is_empty())
                .then(|| overlay.init_containers.clone()),
            containers: vec![Container {
                name: CONTAINER.into(),
                image: Some(spec.image.clone()),
                image_pull_policy: overlay.image_pull_policy.clone(),
                command: non_empty(&spec.command),
                args: non_empty(&spec.args),
                env: Some(env),
                // An agent that writes no termination message still says something: the
                // tail of its log becomes the message.
                termination_message_policy: Some("FallbackToLogsOnError".into()),
                security_context: Some(hardened(uid, overlay.read_only_root_filesystem.unwrap_or(true))),
                ports: (!overlay.ports.is_empty()).then(|| overlay.ports.clone()),
                liveness_probe: overlay.liveness_probe.clone(),
                volume_mounts: Some(
                    std::iter::once(VolumeMount {
                        name: TMP_VOLUME.into(),
                        mount_path: TMP_PATH.into(),
                        ..Default::default()
                    })
                    .chain(overlay.volume_mounts.iter().cloned())
                    .collect(),
                ),
                ..Default::default()
            }],
            volumes: Some(
                std::iter::once(Volume {
                    name: TMP_VOLUME.into(),
                    empty_dir: Some(EmptyDirVolumeSource::default()),
                    ..Default::default()
                })
                .chain(overlay.volumes.iter().cloned())
                .collect(),
            ),
            ..Default::default()
        }),
        status: None,
    }
}

fn hardened(uid: i64, read_only_root: bool) -> SecurityContext {
    SecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(uid),
        run_as_group: Some(uid),
        read_only_root_filesystem: Some(read_only_root),
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
                pod: None,
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

    fn overlay() -> PodOverlay {
        PodOverlay {
            run_as_user: Some(1000),
            fs_group: Some(1000),
            image_pull_policy: Some("IfNotPresent".into()),
            automount_service_account_token: None,
            read_only_root_filesystem: None,
            init_containers: vec![Container {
                name: "polar-init".into(),
                image: Some("polar-init:1".into()),
                volume_mounts: Some(vec![VolumeMount {
                    name: "certs".into(),
                    mount_path: "/certs".into(),
                    ..Default::default()
                }]),
                ..Default::default()
            }],
            volumes: vec![Volume {
                name: "certs".into(),
                empty_dir: Some(EmptyDirVolumeSource::default()),
                ..Default::default()
            }],
            volume_mounts: vec![VolumeMount {
                name: "certs".into(),
                mount_path: "/certs".into(),
                read_only: Some(true),
                ..Default::default()
            }],
            env: vec![EnvVar {
                name: "SECRET".into(),
                value: None,
                value_from: Some(k8s_openapi::api::core::v1::EnvVarSource {
                    secret_key_ref: Some(k8s_openapi::api::core::v1::SecretKeySelector {
                        name: "s".into(),
                        key: "k".into(),
                        optional: None,
                    }),
                    ..Default::default()
                }),
            }],
            ports: vec![k8s_openapi::api::core::v1::ContainerPort {
                container_port: 8080,
                ..Default::default()
            }],
            liveness_probe: Some(k8s_openapi::api::core::v1::Probe::default()),
        }
    }

    fn with_overlay(overlay: PodOverlay) -> PolarAgent {
        let mut agent = agent();
        agent.spec.pod = Some(overlay);
        agent
    }

    #[test]
    fn a_spec_without_an_overlay_is_the_pod_it_always_was() {
        let pod = build(&agent(), "demo-1", "h");
        let spec = pod.spec.unwrap();
        assert!(spec.init_containers.is_none());
        assert_eq!(spec.volumes.unwrap().len(), 1);
        let container = &spec.containers[0];
        assert!(container.liveness_probe.is_none() && container.ports.is_none());
        let context = container.security_context.as_ref().unwrap();
        assert_eq!(context.run_as_user, Some(DEFAULT_UID));
        // A spec written before overlays existed hashes as it did: no running agent is
        // restarted by an upgrade of the operator.
        assert!(!serde_json::to_string(&agent().spec).unwrap().contains("\"pod\""));
    }

    #[test]
    fn an_overlay_is_merged_over_the_hardened_defaults() {
        let pod = build(&with_overlay(overlay()), "demo-1", "h");
        let spec = pod.spec.unwrap();

        assert_eq!(spec.init_containers.as_ref().unwrap()[0].name, "polar-init");
        let volumes: Vec<_> = spec.volumes.unwrap().into_iter().map(|v| v.name).collect();
        assert_eq!(volumes, ["tmp", "certs"], "the scratch volume stays first");

        let container = &spec.containers[0];
        let mounts: Vec<_> = container
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .map(|m| m.mount_path.as_str())
            .collect();
        assert_eq!(mounts, ["/tmp", "/certs"]);
        assert_eq!(container.ports.as_ref().unwrap()[0].container_port, 8080);
        assert!(container.liveness_probe.is_some());

        // The contract's own variables come first; the overlay's follow `spec.env`.
        let env = container.env.as_ref().unwrap();
        assert_eq!(env[0].name, "POLAR_AGENT_NAME");
        assert_eq!(env.last().unwrap().name, "SECRET");
        assert!(env.last().unwrap().value_from.is_some());

        // Still hardened; only the user moved, and it moved for the group too.
        let context = container.security_context.as_ref().unwrap();
        assert_eq!(context.run_as_user, Some(1000));
        assert_eq!(context.run_as_group, Some(1000));
        assert_eq!(context.run_as_non_root, Some(true));
        assert_eq!(context.read_only_root_filesystem, Some(true));
        assert_eq!(spec.automount_service_account_token, Some(false));
        assert_eq!(spec.enable_service_links, Some(false), "no variable per Service in the namespace");
        assert_eq!(container.image_pull_policy.as_deref(), Some("IfNotPresent"));
        assert_eq!(spec.security_context.as_ref().unwrap().fs_group, Some(1000));
    }

    #[test]
    fn the_token_and_the_read_only_root_are_off_and_on_unless_the_spec_says_otherwise() {
        let default = build(&with_overlay(overlay()), "demo-1", "h").spec.unwrap();
        assert_eq!(default.automount_service_account_token, Some(false));
        assert_eq!(default.containers[0].security_context.as_ref().unwrap().read_only_root_filesystem, Some(true));

        let mut asked = overlay();
        asked.automount_service_account_token = Some(true);
        asked.read_only_root_filesystem = Some(false);
        let spec = build(&with_overlay(asked), "demo-1", "h").spec.unwrap();
        assert_eq!(spec.automount_service_account_token, Some(true));
        let context = spec.containers[0].security_context.as_ref().unwrap();
        assert_eq!(context.read_only_root_filesystem, Some(false));
        // Everything else about the hardening stays.
        assert_eq!(context.run_as_non_root, Some(true));
        assert_eq!(context.allow_privilege_escalation, Some(false));
    }

    #[test]
    fn an_overlay_is_part_of_what_identifies_a_spec() {
        let plain = spec_hash(&agent().spec);
        let overlaid = spec_hash(&with_overlay(overlay()).spec);
        assert_ne!(plain, overlaid);
        let mut other = overlay();
        other.run_as_user = Some(1001);
        assert_ne!(overlaid, spec_hash(&with_overlay(other).spec));
    }

    #[test]
    fn a_sound_overlay_has_no_problems() {
        assert!(overlay_problems(&agent().spec).is_empty());
        assert!(overlay_problems(&with_overlay(overlay()).spec).is_empty());
    }

    #[test]
    fn every_problem_with_an_overlay_is_named() {
        let mut bad = overlay();
        bad.run_as_user = Some(0);
        bad.image_pull_policy = Some("Sometimes".into());
        bad.volumes.push(Volume {
            name: "tmp".into(),
            ..Default::default()
        });
        bad.volumes.push(Volume {
            name: "certs".into(),
            ..Default::default()
        });
        bad.volume_mounts.push(VolumeMount {
            name: "tmp".into(),
            mount_path: "/tmp".into(),
            ..Default::default()
        });
        bad.volume_mounts.push(VolumeMount {
            name: "missing".into(),
            mount_path: "/missing".into(),
            ..Default::default()
        });
        bad.init_containers.push(Container {
            name: String::new(),
            ..Default::default()
        });
        bad.init_containers.push(Container {
            name: CONTAINER.into(),
            ..Default::default()
        });
        bad.init_containers.push(Container {
            name: "late".into(),
            volume_mounts: Some(vec![VolumeMount {
                name: "nowhere".into(),
                mount_path: "/x".into(),
                ..Default::default()
            }]),
            ..Default::default()
        });

        let problems = overlay_problems(&with_overlay(bad).spec).join("\n");
        for expected in [
            "runAsUser is 0",
            "imagePullPolicy is 'Sometimes'",
            "a volume 'tmp'",
            "names 'certs' twice",
            "at /tmp",
            "'missing', which is not one of pod.volumes",
            "has no name",
            "'agent', which is already a container",
            "init container 'late' mounts 'nowhere'",
        ] {
            assert!(problems.contains(expected), "missing {expected:?} in:\n{problems}");
        }
    }

    /// The Dhall library and the operator are two programs that must agree on a shape. This is
    /// the library's own rendered example (`src/deploy/examples`), read by the operator.
    #[test]
    fn what_the_library_renders_is_a_spec_the_operator_runs() {
        let rendered = include_str!("../../../../deploy/examples/builder-agent.yaml");
        let mut agent: PolarAgent = serde_yaml::from_str(rendered).expect("the library's output is a PolarAgent");
        agent.metadata.uid = Some("uid-1".into());
        assert!(overlay_problems(&agent.spec).is_empty());

        let pod = build(&agent, "example-1", &spec_hash(&agent.spec));
        let spec = pod.spec.unwrap();
        // 30 s budget, plus the margin.
        assert_eq!(spec.termination_grace_period_seconds, Some(40));

        let init = &spec.init_containers.as_ref().unwrap()[0];
        assert_eq!(init.name, "polar-init");
        let args = init.args.as_ref().unwrap();
        assert!(args.contains(&"--check-dep-cert".to_string()) && args.contains(&"--cert".to_string()));

        let volumes: Vec<_> = spec.volumes.as_ref().unwrap().iter().map(|v| v.name.as_str()).collect();
        assert_eq!(volumes, ["tmp", "sa-token", "polar-certs", "data"]);

        let container = &spec.containers[0];
        let context = container.security_context.as_ref().unwrap();
        assert_eq!(context.run_as_user, Some(1000));
        assert_eq!(container.ports.as_ref().unwrap()[0].container_port, 8080);
        let probe = container.liveness_probe.as_ref().unwrap();
        assert_eq!(probe.exec.as_ref().unwrap().command, Some(vec!["polar-healthcheck".to_string()]));

        let env = container.env.as_ref().unwrap();
        let broker = env.iter().find(|e| e.name == "BROKER_ADDR").expect("the broker address");
        assert_eq!(broker.value.as_deref(), Some("cassini-ip-svc.polar.svc.cluster.local:8080"));
        let secret = env.iter().find(|e| e.name == "EXAMPLE_PASSWORD").expect("the secret reference");
        assert!(secret.value.is_none() && secret.value_from.is_some(), "a secret is a reference, never a value");
        // The operator's own variables are still there and still the operator's.
        assert!(env.iter().any(|e| e.name == "POLAR_SHUTDOWN_BUDGET_SECS" && e.value.as_deref() == Some("30")));
        assert!(env.iter().any(|e| e.name == "POLAR_CAPABILITIES" && e.value.as_deref() == Some("cassini")));
    }

    /// Every PolarAgent in a rendered plan, as the operator will read it.
    fn agents_in(plan: &str) -> Vec<PolarAgent> {
        let plan: serde_yaml::Value = serde_yaml::from_str(plan).unwrap();
        plan["stages"]
            .as_sequence()
            .unwrap()
            .iter()
            .flat_map(|stage| stage["objects"].as_sequence().cloned().unwrap_or_default())
            .filter(|object| object["kind"] == "PolarAgent")
            .map(|object| serde_yaml::from_value(object).unwrap())
            .collect()
    }

    /// The core's own agents (`src/deploy/local/core`) and an ordinary one differ in exactly the ways the
    /// core needs: the core's builder agents take their certificates from the issuer directly and do not
    /// bootstrap an escrow key (Cassini is what the bootstrap travels through, the credentials agent is
    /// what it asks); an ordinary agent does bootstrap, with the environment that needs. The core's
    /// services (the issuer, the graph) are not builder agents and get none of it.
    #[test]
    fn the_cores_agents_differ_from_an_ordinary_one_where_they_must() {
        // The least each agent's own `--scaffold` says it needs to drain.
        let least_budget = [("cassini", 20), ("credentials-agent", 50), ("credentials-agent-observer", 40)];
        let mut builder = 0;
        let mut services = Vec::new();
        for mut agent in agents_in(include_str!("../../../../deploy/local/core/rendered/plan.yaml")) {
            agent.metadata.uid = Some("uid".into());
            let name = agent.metadata.name.clone().unwrap();
            assert!(overlay_problems(&agent.spec).is_empty(), "{name}");
            let pod = build(&agent, &format!("{name}-1"), &spec_hash(&agent.spec)).spec.unwrap();
            assert_eq!(
                pod.termination_grace_period_seconds,
                Some(agent.spec.shutdown_budget_secs as i64 + GRACE_MARGIN_SECS as i64)
            );

            let Some(init) = pod.init_containers.as_ref().map(|i| &i[0]) else {
                // A service: no polar-init, no certificate, no health file.
                services.push((name, agent, pod));
                continue;
            };
            let least = least_budget.iter().find(|(n, _)| *n == name).expect("a core builder agent").1;
            assert!(agent.spec.shutdown_budget_secs >= least, "{name}: budget below what the agent declares");
            assert_eq!(init.command, Some(vec!["polar-init".to_string()]), "{name}: args alone would replace the image's command");
            let init_env: Vec<_> = init.env.as_ref().unwrap().iter().map(|e| (e.name.as_str(), e.value.as_deref())).collect();
            assert!(init_env.contains(&("POLAR_INIT_ISSUER", Some("cert-issuer"))), "{name} asks the issuer directly");
            assert!(init_env.contains(&("POLAR_INIT_SKIP_ESCROW_BOOTSTRAP", Some("true"))), "{name} skips the escrow bootstrap");
            assert!(!init_env.iter().any(|(n, _)| *n == "KUBERNETES_SA_TOKEN"), "{name} needs no escrow environment");
            let args = init.args.as_ref().unwrap();
            assert!(!args.contains(&"--check-dep-cert".to_string()), "{name} does not wait for the broker");
            if name == "cassini" {
                assert!(args.iter().any(|a| a.starts_with("server:") && a.ends_with(":cassini-ip-svc.polar.svc.cluster.local")));
            } else {
                assert!(args.iter().any(|a| a.starts_with("client:")));
            }
            builder += 1;
        }
        assert_eq!(builder, 3);

        // The services: the issuer and the graph.
        let names: Vec<_> = services.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, ["cert-issuer", "polar-neo4j"]);
        for (name, agent, pod) in &services {
            assert!(pod.init_containers.is_none(), "{name} is not an agent that asks for a certificate");
            let container = &pod.containers[0];
            assert!(container.liveness_probe.is_some(), "{name} is probed");
            assert!(agent.spec.needs.is_empty());
        }
        let (_, issuer, issuer_pod) = &services[0];
        // The issuer reads the API server's signing keys with its token, and its CA key must stay 0600,
        // which a pod-level group would undo.
        assert_eq!(issuer_pod.automount_service_account_token, Some(true));
        assert!(issuer_pod.security_context.is_none(), "no fsGroup on the issuer");
        assert_eq!(issuer_pod.containers[0].security_context.as_ref().unwrap().read_only_root_filesystem, Some(true));
        assert!(issuer.spec.pod.as_ref().unwrap().fs_group.is_none());
        // The graph's image writes outside its volumes: a visible exception, and the only one.
        let (_, _, graph_pod) = &services[1];
        assert_eq!(graph_pod.containers[0].security_context.as_ref().unwrap().read_only_root_filesystem, Some(false));
        assert_eq!(graph_pod.automount_service_account_token, Some(false));

        // An ordinary agent (the library's own example): vouched for, and it bootstraps its escrow key,
        // which needs the client environment and its token in the init container.
        let example = include_str!("../../../../deploy/examples/builder-agent.yaml");
        let agent: PolarAgent = serde_yaml::from_str(example).unwrap();
        let pod = build(&agent, "example-1", &spec_hash(&agent.spec)).spec.unwrap();
        let init = &pod.init_containers.as_ref().unwrap()[0];
        assert_eq!(init.command, Some(vec!["polar-init".to_string()]));
        let env: Vec<_> = init.env.as_ref().unwrap().iter().map(|e| e.name.as_str()).collect();
        for needed in ["POLAR_INIT_OBSERVER_URL", "KUBERNETES_SA_TOKEN", "TLS_CLIENT_CERT", "BROKER_ADDR", "CASSINI_SERVER_NAME"] {
            assert!(env.contains(&needed), "an ordinary agent's init container needs {needed}");
        }
        assert!(!env.contains(&"POLAR_INIT_SKIP_ESCROW_BOOTSTRAP"));
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
