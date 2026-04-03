//! Kubernetes Job manifest construction for Cyclops build and bootstrap jobs.
//!
//! # Job anatomy
//!
//! Every job — whether a pipeline job or a bootstrap job — follows the same
//! two-container structure:
//!
//! ```
//! initContainers:
//!   - name: clone          ← clones source into /workspace
//! containers:
//!   - name: pipeline       ← runs /bin/pipeline-runner (or nix build for bootstrap)
//! volumes:
//!   - name: workspace      ← emptyDir shared between init and main container
//!   - name: git-credentials ← projected from a k8s Secret
//! ```
//!
//! The init container runs to completion before the main container starts.
//! If the clone fails the Job fails immediately — k8s never starts the main
//! container. This is the correct failure mode: a build that can't fetch its
//! source should not proceed.
//!
//! # Credential model
//!
//! Git credentials and registry credentials are referenced by Secret name.
//! The orchestrator does not provision these secrets — they must exist in the
//! build namespace before any job is submitted. Provisioning is an
//! infrastructure concern; the orchestrator only declares which secret to use.
//!
//! Current support:
//! - HTTP token auth for git (GitLab deploy tokens, GitHub PATs)
//! - dockerconfigjson for registry pull/push (ACR, ECR, GCR, etc.)
//!
//! Future: SSH key auth will be added once the identity root agent can issue
//! short-lived per-job SSH credentials, removing the need for long-lived key
//! secrets entirely.
//!
//! # Workspace convention
//!
//! Source is cloned to /workspace inside the pod. The pipeline container and
//! init container share this path via an emptyDir volume. The pipeline runner
//! is expected to treat /workspace as its working directory. This is a
//! framework convention, not a configurable path — the Nix build that produces
//! the pipeline image must agree on /workspace.
//!
//! # Security posture
//!
//! - Non-root, no privilege escalation, no ambient capabilities on both containers.
//! - No service account token mounted — neither container has k8s API access.
//! - Registry credentials mounted read-only; git credentials exposed only as
//!   env vars sourced from the secret, not as a mounted volume, so the
//!   credential value is never written to the container filesystem.
//! - restartPolicy=Never on the pod — retries are the orchestrator's decision,
//!   not Kubernetes's. A failed job is a terminal event from k8s's perspective.

use std::collections::BTreeMap;

use k8s_openapi::{
    api::{
        batch::v1::{Job, JobSpec},
        core::v1::{
            Container, EnvVar, LocalObjectReference, PodSecurityContext, PodSpec, PodTemplateSpec,
            ResourceRequirements, SecretVolumeSource, SecurityContext, Volume, VolumeMount,
        },
    },
    apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::ObjectMeta},
};
use orchestrator_core::types::{BootstrapSpec, BuildSpec, GitCredentials, RegistryCredentials};
use uuid::Uuid;

/// The image used as the init container for source cloning.
///
/// This is a minimal image containing only git and a small shell script.
/// It is separate from both the bootstrap image and the pipeline image —
/// its only job is to clone a repo at a specific commit into /workspace.
///
/// Pinned by digest here as a compile-time constant. When this image is
/// updated, update this constant and redeploy. In future this will be
/// injected from config like the bootstrap image.
///
/// TODO: move this to OrchestratorConfig once the config schema is extended.
const CLONE_INIT_IMAGE: &str = "cyclops/git-clone:latest";

/// The path inside every job pod where source code is available.
/// Both the init container (writer) and the main container (reader) mount
/// the workspace emptyDir volume here. This is a framework convention.
const WORKSPACE_PATH: &str = "/workspace";

/// Name of the emptyDir volume shared between init and main containers.
const WORKSPACE_VOLUME: &str = "workspace";

/// Name of the projected volume that holds git credential env var sources.
/// The actual credential values are sourced from a k8s Secret and injected
/// as env vars into the init container only — never into the main container.
const GIT_CREDS_VOLUME: &str = "git-credentials";

const TLS_CLIENT_KEY: &str = "tls.key";
const TLS_CLIENT_CERT: &str = "tls.crt";
const TLS_CA_CERT: &str = "ca.crt";
const TLS_CERT_DIR: &str = "/etc/tls";

const CLIENT_TLS_SECRET_NAME: &str = "client-tls";
const CLIENT_TLS_VOLUME_NAME: &str = CLIENT_TLS_SECRET_NAME;

/// Derives a deterministic, DNS-safe Job name from a build ID.
/// Kubernetes names must be <= 63 chars, lowercase alphanumeric and hyphens.
/// A UUID is 36 chars; with the prefix "cyclops-build-" (14 chars) = 50 chars total.
pub fn job_name_for_build(build_id: Uuid) -> String {
    format!("cyclops-build-{}", build_id)
}

/// Same naming convention for bootstrap jobs.
/// Distinguished from pipeline jobs so they are identifiable in the cluster
/// without having to inspect annotations.
pub fn job_name_for_bootstrap(build_id: Uuid) -> String {
    format!("cyclops-bootstrap-{}", build_id)
}

// ── Pipeline job ──────────────────────────────────────────────────────────────

/// Constructs a batch/v1 Job manifest for a pipeline build.
///
/// The resulting Job runs two containers:
/// 1. An init container that clones the source repo at the specified commit
///    into /workspace using credentials from a k8s Secret.
/// 2. The pipeline container that runs /bin/pipeline-runner against /workspace.
///    This image is produced by the Nix build from container.dhall and always
///    has /bin/pipeline-runner as its entrypoint. The orchestrator does not
///    inject any build logic — it only provides context via env vars.
pub fn build_job_manifest(spec: &BuildSpec, namespace: &str) -> Job {
    let job_name = job_name_for_build(spec.build_id);
    let (labels, annotations) =
        standard_metadata(spec.build_id, &spec.repo_url, &spec.commit_sha, "pipeline");

    let init_container =
        clone_init_container(&spec.repo_url, &spec.commit_sha, &spec.git_credentials);

    let pipeline_env = pipeline_env_vars(spec);

    // The pipeline container deliberately has no command override.
    // /bin/pipeline-runner is baked into the image by the Nix build as CMD/ENTRYPOINT.
    // If entrypoint is set (tests only), it overrides that.
    let pipeline_container = Container {
        name: "pipeline".to_string(),
        image: Some(spec.pipeline_image.clone()),

        // image_pull_policy: Some("Never".into()), // TODO: make configurable
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo 'workspace contents:' && ls -la /workspace && echo 'done'".to_string(),
        ]),
        env: Some(pipeline_env),
        // TODO: add hardned security context back, or at least allow users to specify their own via config
        // security_context: Some(hardened_security_context()),
        resources: Some(default_resources()),
        volume_mounts: Some(vec![client_tls_volume_mount(), workspace_volume_mount()]),
        ..Default::default()
    };

    // TODO: Insert these IF and only IF they're present in the configuration, skipping to test on insecure local registry
    // let image_pull_secrets = registry_pull_secrets(&spec.registry_credentials);

    let pod_spec = PodSpec {
        init_containers: Some(vec![init_container]),
        containers: vec![pipeline_container],
        volumes: Some(vec![
            workspace_volume(),
            client_tls_voiume(), // git_credentials_volume(&spec.git_credentials), TODO: Optionally insert git credentials via configuration
        ]),
        // the clone image runs as non-root user, so we must make sure that the emptydir we clone into is owned by git
        security_context: Some(PodSecurityContext {
            fs_group: Some(65532), //
            ..Default::default()
        }),
        restart_policy: Some("Never".to_string()),
        // No service account token — neither container has k8s API access.
        automount_service_account_token: Some(false),
        // image_pull_secrets: Some(image_pull_secrets), // TODO: insert pull secrets dyanmically based on config
        ..Default::default()
    };

    assemble_job(job_name, namespace, labels, annotations, pod_spec)
}

// ── Bootstrap job ─────────────────────────────────────────────────────────────

/// Constructs a batch/v1 Job manifest for a bootstrap build.
///
/// A bootstrap job is structurally identical to a pipeline job, with two
/// differences:
///
/// 1. The main container image is the well-known Nix builder rather than a
///    repo-specific pipeline image. It knows how to evaluate container.dhall
///    and run `nix build` to produce and push the pipeline image.
///
/// 2. The main container receives registry push credentials in addition to
///    the standard env vars, because it must push the produced image to the
///    registry. Pipeline jobs never push — they only pull.
///
/// The init container is identical to the pipeline job — source must be
/// cloned so the Nix evaluator can find container.dhall.
pub fn bootstrap_job_manifest(spec: &BootstrapSpec, namespace: &str) -> Job {
    let job_name = job_name_for_bootstrap(spec.build_id);
    let (labels, annotations) =
        standard_metadata(spec.build_id, &spec.repo_url, &spec.commit_sha, "bootstrap");

    let init_container =
        clone_init_container(&spec.repo_url, &spec.commit_sha, &spec.git_credentials);

    let bootstrap_env = bootstrap_env_vars(spec);

    // The bootstrap container runs nix build against the cloned source.
    // It does not have a baked-in entrypoint convention like pipeline images —
    // the bootstrap image's CMD drives the evaluation and push.
    let bootstrap_container = Container {
        name: "bootstrap".to_string(),
        image: Some(spec.bootstrap_image.clone()),
        env: Some(bootstrap_env),
        security_context: Some(hardened_security_context()),
        // Bootstrap builds are more resource-intensive than typical pipeline
        // runs because nix evaluation and build closure download happen here.
        resources: Some(bootstrap_resources()),
        volume_mounts: Some(vec![workspace_volume_mount()]),
        ..Default::default()
    };

    let image_pull_secrets = registry_pull_secrets(&spec.registry_credentials);

    let pod_spec = PodSpec {
        init_containers: Some(vec![init_container]),
        containers: vec![bootstrap_container],
        volumes: Some(vec![
            workspace_volume(),
            git_credentials_volume(&spec.git_credentials),
        ]),
        restart_policy: Some("Never".to_string()),
        automount_service_account_token: Some(false),
        image_pull_secrets: Some(image_pull_secrets),
        ..Default::default()
    };

    assemble_job(job_name, namespace, labels, annotations, pod_spec)
}

// ── Init container ────────────────────────────────────────────────────────────

/// Constructs the git clone init container.
///
/// This container runs before any main container in the pod. It clones the
/// repository at the specified commit SHA into WORKSPACE_PATH and then exits.
/// Kubernetes will not start the main container until this exits with code 0.
///
/// The clone image is expected to execute roughly:
///   git clone --depth 1 $CYCLOPS_REPO_URL /workspace
///   git -C /workspace checkout $CYCLOPS_COMMIT_SHA
///
/// Credentials are injected as env vars sourced directly from the k8s Secret.
/// They are never written to disk inside the container.
///
/// The init container mounts the workspace volume as writable (it populates it).
/// The main container mounts the same volume as read-only.
fn clone_init_container(
    repo_url: &str,
    commit_sha: &str,
    credentials: &GitCredentials,
) -> Container {
    let mut env = vec![
        EnvVar {
            name: "CYCLOPS_REPO_URL".to_string(),
            value: Some(repo_url.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "CYCLOPS_COMMIT_SHA".to_string(),
            value: Some(commit_sha.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "CYCLOPS_WORKSPACE".to_string(),
            value: Some(WORKSPACE_PATH.to_string()),
            ..Default::default()
        },
    ];

    // TODO: do this only if the configured repo has credentials provided
    // Inject credential env vars sourced from the k8s Secret.
    // The clone image reads GIT_USERNAME and GIT_PASSWORD and constructs
    // the authenticated remote URL itself. This keeps credential handling
    // inside the clone image, not in the orchestrator.
    env.extend(git_credential_env_vars(credentials));

    Container {
        name: "clone".to_string(),
        image_pull_policy: Some("Never".to_string()), // TODO: remove NEVER pull policy.
        image: Some(CLONE_INIT_IMAGE.to_string()),
        env: Some(env),
        // TODO: enable hardened sec context when we get a controlled clone image.
        // security_context: Some(hardened_security_context()),
        resources: Some(clone_resources()),
        volume_mounts: Some(vec![VolumeMount {
            name: WORKSPACE_VOLUME.to_string(),
            mount_path: WORKSPACE_PATH.to_string(),
            // Init container writes — must be writable.
            read_only: Some(false),
            ..Default::default()
        }]),
        ..Default::default()
    }
}

// ── Env var builders ──────────────────────────────────────────────────────────

/// Standard env vars injected into every pipeline container.
///
/// These give the pipeline runner the context it needs to locate source,
/// report results, and emit Cassini events. The pipeline runner must not
/// need to be told anything about its own internal build steps — that is
/// encoded in the pipeline manifest baked into the image.
fn pipeline_env_vars(spec: &BuildSpec) -> Vec<EnvVar> {
    vec![
        EnvVar {
            name: "CYCLOPS_BUILD_ID".to_string(),
            value: Some(spec.build_id.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "CYCLOPS_REPO_URL".to_string(),
            value: Some(spec.repo_url.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "CYCLOPS_COMMIT_SHA".to_string(),
            value: Some(spec.commit_sha.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "CYCLOPS_WORKSPACE".to_string(),
            value: Some(WORKSPACE_PATH.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "TLS_CA_CERT".to_string(),
            value: Some(format!("{TLS_CERT_DIR}/{TLS_CA_CERT}")),
            ..Default::default()
        },
        EnvVar {
            name: "TLS_CLIENT_CERT".to_string(),
            value: Some(format!("{TLS_CERT_DIR}/{TLS_CLIENT_CERT}")),
            ..Default::default()
        },
        EnvVar {
            name: "TLS_CLIENT_KEY".to_string(),
            value: Some(format!("{TLS_CERT_DIR}/{TLS_CLIENT_KEY}")),
            ..Default::default()
        },
        // Additional caller-supplied env vars. Injected last so they can
        // override the standard vars if absolutely necessary, though that
        // should be considered a code smell.
    ]
    .into_iter()
    .chain(spec.env.iter().map(|(k, v)| EnvVar {
        name: k.clone(),
        value: Some(v.clone()),
        ..Default::default()
    }))
    .collect()
}

/// Env vars for bootstrap jobs. Superset of pipeline vars.
///
/// The bootstrap container additionally needs to know the container config
/// path so it can find container.dhall within /workspace, and it needs
/// registry push credentials so it can push the produced image.
fn bootstrap_env_vars(spec: &BootstrapSpec) -> Vec<EnvVar> {
    let mut env = vec![
        EnvVar {
            name: "POLAR_BUILD_ID".to_string(),
            value: Some(spec.build_id.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "POLAR_REPO_URL".to_string(),
            value: Some(spec.repo_url.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "BUILD_COMMIT_SHA".to_string(),
            value: Some(spec.commit_sha.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "BUILD_WORKSPACE".to_string(),
            value: Some(WORKSPACE_PATH.to_string()),
            ..Default::default()
        },
        // The path within /workspace where container.dhall is located.
        // The bootstrap image evaluates this file to produce the pipeline image.
        EnvVar {
            name: "CYCLOPS_CONTAINER_CONFIG_REF".to_string(),
            value: Some(spec.container_config_ref.clone()),
            ..Default::default()
        },
    ];

    // Registry push credentials for the bootstrap container.
    // Pipeline containers never push, so these are bootstrap-only.
    env.extend(registry_push_env_vars(&spec.registry_credentials));
    env
}

/// Constructs env vars that source git credentials from a k8s Secret.
///
/// Values are pulled from the Secret at pod startup by the kubelet — they
/// are never present in the Job manifest itself, which is important for
/// audit log hygiene. Anyone reading the Job spec sees only which secret
/// is referenced, not what the credentials are.
fn git_credential_env_vars(credentials: &GitCredentials) -> Vec<EnvVar> {
    match credentials {
        GitCredentials::HttpToken { secret_name } => vec![
            EnvVar {
                name: "GIT_USERNAME".to_string(),
                // TODO: remove hardcoded test username, replace with val from secret
                value: Some("username".to_string()),
                // value_from: Some(EnvVarSource {
                //     secret_key_ref: Some(SecretKeySelector {
                //         name: secret_name.clone(),
                //         key: "username".to_string(),
                //         optional: Some(false),
                //     }),
                //     ..Default::default()
                // }),
                ..Default::default()
            },
            EnvVar {
                name: "GIT_PASSWORD".to_string(),
                // TODO: remove,
                value: Some("password".to_string()),
                // value_from: Some(EnvVarSource {
                //     secret_key_ref: Some(SecretKeySelector {
                //         name: secret_name.clone(),
                //         key: "password".to_string(),
                //         optional: Some(false),
                //     }),
                //     ..Default::default()
                // }),
                ..Default::default()
            },
        ],
    }
}

/// Registry push credentials for the bootstrap container.
///
/// For dockerconfigjson secrets, the conventional approach is to mount
/// the secret as a volume at ~/.docker/config.json. However, mounting
/// the entire dockerconfig gives access to all registries in that file.
/// A tighter approach — referencing specific keys — is not universally
/// supported across registry clients.
///
/// For now we use the imagePullSecrets mechanism for pulls (handled in
/// registry_pull_secrets) and expose the secret name as an env var for
/// the bootstrap container to use however its registry client expects.
/// The bootstrap image is responsible for the actual push logic.
fn registry_push_env_vars(credentials: &RegistryCredentials) -> Vec<EnvVar> {
    match credentials {
        RegistryCredentials::DockerConfigJson { secret_name } => vec![EnvVar {
            name: "CYCLOPS_REGISTRY_SECRET".to_string(),
            value: Some(secret_name.clone()),
            ..Default::default()
        }],
    }
}

// ── Volume builders ───────────────────────────────────────────────────────────

/// The emptyDir workspace volume. Populated by the init container, consumed
/// by the main container. Exists only for the lifetime of the pod.
fn workspace_volume() -> Volume {
    Volume {
        name: WORKSPACE_VOLUME.to_string(),
        empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
            // No memory backing — source trees can be large and we don't want
            // to consume node memory for what is essentially scratch disk.
            medium: None,
            size_limit: None,
        }),
        ..Default::default()
    }
}

/// The tls cert volume. Populated by a secret current provided by cert-manager.
/// Must exist for jobs to use the cassini-client
fn client_tls_voiume() -> Volume {
    Volume {
        name: CLIENT_TLS_VOLUME_NAME.to_string(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(CLIENT_TLS_SECRET_NAME.to_string()),
            optional: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Volume mount for the workspace. Read-only in the main container.
///
/// The pipeline runner should never need to write back to /workspace.
/// Read-only enforces this at the kernel level and is a meaningful
/// security boundary — a compromised pipeline cannot tamper with the
/// source it just built from.
fn workspace_volume_mount() -> VolumeMount {
    VolumeMount {
        name: WORKSPACE_VOLUME.to_string(),
        mount_path: WORKSPACE_PATH.to_string(),
        read_only: Some(true),
        ..Default::default()
    }
}
/// Volume mount for the client tls files. Read-only in the main container.
///
fn client_tls_volume_mount() -> VolumeMount {
    VolumeMount {
        name: CLIENT_TLS_VOLUME_NAME.to_string(),
        mount_path: TLS_CERT_DIR.to_string(),
        read_only: Some(true),
        ..Default::default()
    }
}

/// Projects the git credential secret into a volume.
///
/// This volume is mounted into the init container only, not the main container.
/// The main container has no business touching git credentials.
///
/// Note: we also inject credentials as env vars (see git_credential_env_vars).
/// The volume exists for future SSH key support — an SSH key cannot be
/// passed as an env var, it must be a file at a known path for the ssh client.
/// For HTTP token auth today, the env var approach is sufficient and the
/// volume is vestigial — but it's cheap to declare and removes a future
/// refactor.
fn git_credentials_volume(credentials: &GitCredentials) -> Volume {
    let secret_name = match credentials {
        GitCredentials::HttpToken { secret_name } => secret_name.clone(),
    };

    Volume {
        name: GIT_CREDS_VOLUME.to_string(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(secret_name),
            // optional: false — if the secret doesn't exist, fail fast at
            // pod scheduling time rather than letting the init container
            // start and fail with a less obvious error.
            optional: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Converts registry credentials into imagePullSecrets entries.
///
/// imagePullSecrets are referenced at the pod level and apply to all
/// containers in the pod, including the init container. This means the
/// clone init image and the pipeline image must both be pullable with
/// the same secret. In practice: put both images in the same registry
/// or ensure the dockerconfigjson covers all registries in use.
fn registry_pull_secrets(credentials: &RegistryCredentials) -> Vec<LocalObjectReference> {
    match credentials {
        RegistryCredentials::DockerConfigJson { secret_name } => {
            vec![LocalObjectReference {
                name: secret_name.clone(),
            }]
        }
    }
}

// ── Security and resources ────────────────────────────────────────────────────

/// Security context applied to all containers.
///
/// These constraints are non-negotiable. If a pipeline image requires root,
/// that is a property of container.dhall that should be fixed, not a reason
/// to relax these constraints here.
fn hardened_security_context() -> SecurityContext {
    SecurityContext {
        run_as_non_root: Some(true),
        read_only_root_filesystem: Some(true),
        allow_privilege_escalation: Some(false),
        ..Default::default()
    }
}

/// Resource limits for the pipeline container.
///
/// These are sensible defaults. A future pass will thread these through from
/// OrchestratorConfig so they can be tuned per repo or per environment.
fn default_resources() -> ResourceRequirements {
    resource_requirements("2", "4Gi", "500m", "512Mi")
}

/// Bootstrap jobs get more resources because nix evaluation downloads and
/// builds a potentially large closure. A bootstrap that gets OOMKilled is
/// a poor experience.
fn bootstrap_resources() -> ResourceRequirements {
    resource_requirements("4", "8Gi", "1", "2Gi")
}

/// The clone init container is intentionally small. It runs git clone and
/// exits — it does not need significant CPU or memory.
fn clone_resources() -> ResourceRequirements {
    resource_requirements("200m", "256Mi", "100m", "128Mi")
}

fn resource_requirements(
    cpu_limit: &str,
    memory_limit: &str,
    cpu_request: &str,
    memory_request: &str,
) -> ResourceRequirements {
    let mut limits = BTreeMap::new();
    limits.insert("cpu".to_string(), Quantity(cpu_limit.to_string()));
    limits.insert("memory".to_string(), Quantity(memory_limit.to_string()));

    let mut requests = BTreeMap::new();
    requests.insert("cpu".to_string(), Quantity(cpu_request.to_string()));
    requests.insert("memory".to_string(), Quantity(memory_request.to_string()));

    ResourceRequirements {
        limits: Some(limits),
        requests: Some(requests),
        ..Default::default()
    }
}

// ── Assembly ──────────────────────────────────────────────────────────────────

/// Standard labels and annotations applied to all Cyclops Jobs.
///
/// Labels are queryable — use them for filtering in kubectl and for
/// network policy selectors. Annotations carry richer metadata for
/// observability tooling.
fn standard_metadata(
    build_id: Uuid,
    repo_url: &str,
    commit_sha: &str,
    job_type: &str,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/managed-by".to_string(),
        "cyclops".to_string(),
    );
    labels.insert("cyclops.build/id".to_string(), build_id.to_string());
    labels.insert("cyclops.build/type".to_string(), job_type.to_string());

    let mut annotations = BTreeMap::new();
    annotations.insert("cyclops.build/repo".to_string(), repo_url.to_string());
    annotations.insert("cyclops.build/commit".to_string(), commit_sha.to_string());

    (labels, annotations)
}

fn assemble_job(
    job_name: String,
    namespace: &str,
    labels: BTreeMap<String, String>,
    annotations: BTreeMap<String, String>,
    pod_spec: PodSpec,
) -> Job {
    let pod_template = PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(labels.clone()),
            ..Default::default()
        }),
        spec: Some(pod_spec),
    };

    Job {
        metadata: ObjectMeta {
            name: Some(job_name),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            annotations: Some(annotations),
            ..Default::default()
        },
        spec: Some(JobSpec {
            template: pod_template,
            // No retries — a failed job is a terminal event. The orchestrator
            // decides whether to retry at a higher level.
            backoff_limit: Some(0),
            // Clean up completed jobs after one hour. Logs should be
            // collected by Polar before this TTL fires.
            ttl_seconds_after_finished: Some(3600),
            ..Default::default()
        }),
        ..Default::default()
    }
}
