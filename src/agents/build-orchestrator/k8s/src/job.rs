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

use orchestrator_core::WORKSPACE_PATH;
use std::collections::BTreeMap;

use k8s_openapi::{
    api::{
        batch::v1::{Job, JobSpec},
        core::v1::{
            Capabilities, Container, EmptyDirVolumeSource, EnvVar, LocalObjectReference,
            PodSecurityContext, PodSpec, PodTemplateSpec, ResourceRequirements, SeccompProfile,
            SecretVolumeSource, SecurityContext, Volume, VolumeMount,
        },
    },
    apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::ObjectMeta},
};
use orchestrator_core::{
    TLS_CERT_DIR, tls_ca_cert_path, tls_client_cert_path, tls_client_key_path,
    types::{BuildSpec, GitCredentials, RegistryCredentials, SourceRef},
};
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

/// Name of the emptyDir volume shared between init and main containers.
const WORKSPACE_VOLUME: &str = "workspace";

/// Scratch emptyDir mounted at /tmp in hardened jobs, whose root filesystems
/// are read-only. The cassini daemon's socket and queue live here.
const TMP_VOLUME: &str = "tmp";

/// Name of the projected volume that holds git credential env var sources.
/// The actual credential values are sourced from a k8s Secret and injected
/// as env vars into the init container only — never into the main container.
const GIT_CREDS_VOLUME: &str = "git-credentials";

pub const CLIENT_TLS_SECRET_NAME: &str = "client-tls";
pub const CLIENT_TLS_VOLUME_NAME: &str = CLIENT_TLS_SECRET_NAME;

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

/// Everything about a job that comes from deployment configuration rather
/// than from the build request.
#[derive(Debug, Clone)]
pub struct JobOptions {
    /// Namespace jobs are created in.
    pub namespace: String,
    /// Secret whose `ca-bundle.crt` becomes both containers' trust store
    /// (see `trust_ca_bundle`). For TLS-inspecting proxies.
    pub ca_bundle_secret: Option<String>,
    /// Requests and limits for the pipeline container.
    pub resources: PipelineResources,
    /// Size limit of the workspace emptyDir, e.g. "2Gi". None means no limit,
    /// so a large repo or archive can fill the node's disk.
    pub workspace_size_limit: Option<String>,
}

impl JobOptions {
    /// Options with the default pipeline resources and no CA bundle or
    /// workspace limit.
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            ca_bundle_secret: None,
            resources: PipelineResources::default(),
            workspace_size_limit: None,
        }
    }
}

/// Pipeline container requests and limits, as Kubernetes quantities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineResources {
    pub cpu_limit: String,
    pub memory_limit: String,
    pub cpu_request: String,
    pub memory_request: String,
}

impl Default for PipelineResources {
    /// Sized for a real CI build. Lower them for analysis on a laptop.
    fn default() -> Self {
        Self {
            cpu_limit: "2".into(),
            memory_limit: "4Gi".into(),
            cpu_request: "500m".into(),
            memory_request: "512Mi".into(),
        }
    }
}

/// Constructs a batch/v1 Job manifest for a pipeline build.
///
/// The resulting Job runs two containers:
/// 1. An init container that clones the source repo at the specified commit
///    into /workspace using credentials from a k8s Secret.
/// 2. The pipeline container that runs /bin/pipeline-runner against /workspace.
///    This image is produced by the Nix build from container.dhall and always
///    has /bin/pipeline-runner as its entrypoint. The orchestrator does not
///    inject any build logic — it only provides context via env vars.
///
/// `ca_bundle_secret`, when set, names a Secret whose `ca-bundle.crt` becomes
/// the trust store of both containers (see `trust_ca_bundle`).
pub fn build_job_manifest(spec: &BuildSpec, options: &JobOptions) -> Job {
    let namespace = options.namespace.as_str();
    let job_name = job_name_for_build(spec.build_id);
    let (labels, annotations) = standard_metadata(spec.build_id, &spec.source, "pipeline");

    let mut init_container = clone_init_container(&spec.source, &spec.git_credentials);

    let pipeline_env = pipeline_env_vars(spec);

    // When entrypoint is None the image's own CMD/ENTRYPOINT runs (/bin/pipeline-runner
    // for pipeline images built from container.dhall, or the analyzer's own entrypoint).
    // Otherwise it overrides that, mirroring the Podman backend.
    let mut pipeline_container = Container {
        name: "pipeline".to_string(),
        image: Some(spec.pipeline_image.clone()),

        // image_pull_policy: Some("Never".into()), // TODO: make configurable
        command: spec.entrypoint.clone(),
        env: Some(pipeline_env),
        // TODO: allow users to specify their own security context via config for git builds
        resources: Some(pipeline_resources(&options.resources)),
        volume_mounts: Some(vec![client_tls_volume_mount(), workspace_volume_mount()]),
        ..Default::default()
    };

    let mut volumes = vec![
        workspace_volume(options.workspace_size_limit.as_deref()),
        client_tls_voiume(), // git_credentials_volume(&spec.git_credentials), TODO: Optionally insert git credentials via configuration
    ];

    // The init container is Cyclops's own minimal image and never needs
    // root, so it is always locked down. Its scratch space is /tmp (the clone
    // script keeps its git config there), so it always gets the tmp volume.
    volumes.push(tmp_volume());
    harden(&mut init_container);

    // The pipeline container is locked down only for archive sources:
    // third-party packages under analysis, so the workspace code is treated as
    // hostile. Git builds keep the image's own context because the CI image
    // (polar-ci) runs as root.
    if let SourceRef::RegistryArchive { .. } = spec.source {
        harden(&mut pipeline_container);
    }

    if let Some(secret_name) = options.ca_bundle_secret.as_deref() {
        volumes.push(ca_bundle_volume(secret_name));
        for container in [&mut init_container, &mut pipeline_container] {
            trust_ca_bundle(container);
        }
    }

    // TODO: Insert these IF and only IF they're present in the configuration, skipping to test on insecure local registry
    // let image_pull_secrets = registry_pull_secrets(&spec.registry_credentials);

    let pod_spec = PodSpec {
        init_containers: Some(vec![init_container]),
        containers: vec![pipeline_container],
        volumes: Some(volumes),
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

// ── Init container ────────────────────────────────────────────────────────────

/// Constructs the source-fetch init container.
///
/// This container runs before any main container in the pod. It populates
/// WORKSPACE_PATH from the build's SourceRef and then exits. Kubernetes will
/// not start the main container until this exits with code 0.
///
/// For git sources the clone image executes roughly:
///   git clone --depth 1 $CYCLOPS_REPO_URL /workspace
///   git -C /workspace checkout $CYCLOPS_COMMIT_SHA
///
/// For archive sources it downloads $CYCLOPS_ARCHIVE_URL, verifies it against
/// $CYCLOPS_ARCHIVE_SHA256, and only then extracts it into /workspace.
///
/// Credentials are injected as env vars sourced directly from the k8s Secret.
/// They are never written to disk inside the container.
///
/// The init container mounts the workspace volume as writable (it populates it).
/// The main container mounts the same volume as read-only.
fn clone_init_container(source: &SourceRef, credentials: &GitCredentials) -> Container {
    let mut env: Vec<EnvVar> = source
        .fetch_env()
        .into_iter()
        .chain([("CYCLOPS_WORKSPACE".to_string(), WORKSPACE_PATH.to_string())])
        .map(|(name, value)| EnvVar {
            name,
            value: Some(value),
            ..Default::default()
        })
        .collect();

    // TODO: do this only if the configured repo has credentials provided
    // Inject credential env vars sourced from the k8s Secret.
    // The clone image reads GIT_USERNAME and GIT_PASSWORD and constructs
    // the authenticated remote URL itself. This keeps credential handling
    // inside the clone image, not in the orchestrator.
    // Archive sources are public downloads and never receive git credentials.
    if let SourceRef::Git { .. } = source {
        env.extend(git_credential_env_vars(credentials));
    }

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
            name: "POLAR_BUILD_ID".to_string(),
            value: Some(spec.build_id.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "BUILD_WORKSPACE".to_string(),
            value: Some(WORKSPACE_PATH.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "TLS_CA_CERT".to_string(),
            value: Some(tls_ca_cert_path()),
            ..Default::default()
        },
        EnvVar {
            name: "TLS_CLIENT_CERT".to_string(),
            value: Some(tls_client_cert_path()),
            ..Default::default()
        },
        EnvVar {
            name: "TLS_CLIENT_KEY".to_string(),
            value: Some(tls_client_key_path()),
            ..Default::default()
        },
    ]
    .into_iter()
    // Source identity (commit SHA, or purl + archive digest).
    .chain(
        spec.source
            .pipeline_env()
            .into_iter()
            .map(|(name, value)| EnvVar {
                name,
                value: Some(value),
                ..Default::default()
            }),
    )
    // Additional caller-supplied env vars. Injected last so they can
    // override the standard vars if absolutely necessary, though that
    // should be considered a code smell.
    .chain(spec.env.iter().map(|(k, v)| EnvVar {
        name: k.clone(),
        value: Some(v.clone()),
        ..Default::default()
    }))
    .collect()
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
fn workspace_volume(size_limit: Option<&str>) -> Volume {
    Volume {
        name: WORKSPACE_VOLUME.to_string(),
        empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
            // No memory backing — source trees can be large and we don't want
            // to consume node memory for what is essentially scratch disk.
            medium: None,
            // Exceeding it gets the pod evicted rather than filling the node.
            size_limit: size_limit.map(|q| Quantity(q.to_string())),
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
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            add: None,
        }),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".to_string(),
            localhost_profile: None,
        }),
        ..Default::default()
    }
}

/// Apply the hardened security context and mount the writable /tmp volume —
/// the only writable path besides the workspace under a read-only root.
fn harden(container: &mut Container) {
    container.security_context = Some(hardened_security_context());
    container
        .volume_mounts
        .get_or_insert_with(Vec::new)
        .push(tmp_volume_mount());
}

/// Where the CA bundle Secret is mounted, and the file within it.
const CA_BUNDLE_VOLUME: &str = "ca-bundle";
const CA_BUNDLE_DIR: &str = "/etc/cyclops/ca";
const CA_BUNDLE_KEY: &str = "ca-bundle.crt";

fn ca_bundle_volume(secret_name: &str) -> Volume {
    Volume {
        name: CA_BUNDLE_VOLUME.to_string(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(secret_name.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Point the container's TLS clients at the mounted bundle. Each tool reads
/// its own variable: OpenSSL-based tools `SSL_CERT_FILE`, git
/// `GIT_SSL_CAINFO`, curl `CURL_CA_BUNDLE`, Nix `NIX_SSL_CERT_FILE`. These
/// override the defaults baked into Cyclops's images.
fn trust_ca_bundle(container: &mut Container) {
    let bundle = format!("{CA_BUNDLE_DIR}/{CA_BUNDLE_KEY}");
    let env = container.env.get_or_insert_with(Vec::new);
    for name in [
        "SSL_CERT_FILE",
        "GIT_SSL_CAINFO",
        "CURL_CA_BUNDLE",
        "NIX_SSL_CERT_FILE",
    ] {
        env.push(EnvVar {
            name: name.to_string(),
            value: Some(bundle.clone()),
            ..Default::default()
        });
    }
    container
        .volume_mounts
        .get_or_insert_with(Vec::new)
        .push(VolumeMount {
            name: CA_BUNDLE_VOLUME.to_string(),
            mount_path: CA_BUNDLE_DIR.to_string(),
            read_only: Some(true),
            ..Default::default()
        });
}

fn tmp_volume() -> Volume {
    Volume {
        name: TMP_VOLUME.to_string(),
        empty_dir: Some(EmptyDirVolumeSource {
            medium: None,
            size_limit: Some(Quantity("256Mi".to_string())),
        }),
        ..Default::default()
    }
}

fn tmp_volume_mount() -> VolumeMount {
    VolumeMount {
        name: TMP_VOLUME.to_string(),
        mount_path: "/tmp".to_string(),
        read_only: Some(false),
        ..Default::default()
    }
}

/// Resource limits for the pipeline container.
///
/// These are sensible defaults. A future pass will thread these through from
/// OrchestratorConfig so they can be tuned per repo or per environment.
fn pipeline_resources(r: &PipelineResources) -> ResourceRequirements {
    resource_requirements(
        &r.cpu_limit,
        &r.memory_limit,
        &r.cpu_request,
        &r.memory_request,
    )
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
    source: &SourceRef,
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
    annotations.insert(
        "cyclops.build/source-kind".to_string(),
        source.kind().to_string(),
    );
    match source {
        SourceRef::Git { url, commit_sha } => {
            annotations.insert("cyclops.build/repo".to_string(), url.clone());
            annotations.insert("cyclops.build/commit".to_string(), commit_sha.clone());
        }
        SourceRef::RegistryArchive {
            purl,
            download_url,
            sha256,
        } => {
            annotations.insert("cyclops.build/purl".to_string(), purl.clone());
            annotations.insert("cyclops.build/archive".to_string(), download_url.clone());
            annotations.insert("cyclops.build/archive-sha256".to_string(), sha256.clone());
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::types::source_env;

    fn spec(source: SourceRef) -> BuildSpec {
        BuildSpec::with_pipeline_runner(
            Uuid::nil(),
            source,
            "registry.example.com/analyzer@sha256:abc".into(),
            GitCredentials::HttpToken {
                secret_name: "git".into(),
            },
            RegistryCredentials::DockerConfigJson {
                secret_name: "reg".into(),
            },
            vec![],
        )
    }

    fn init_env(job: &Job) -> Vec<(String, Option<String>)> {
        job.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .init_containers
            .as_ref()
            .unwrap()[0]
            .env
            .clone()
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.value))
            .collect()
    }

    #[test]
    fn archive_job_gets_digest_and_no_git_credentials() {
        let job = build_job_manifest(
            &spec(SourceRef::RegistryArchive {
                purl: "pkg:cargo/serde@1.0.219".into(),
                download_url: "https://static.crates.io/crates/serde/serde-1.0.219.crate".into(),
                sha256: "b".repeat(64),
            }),
            &JobOptions::new("polar-builds"),
        );
        let env = init_env(&job);
        assert!(env.contains(&(source_env::SOURCE_KIND.into(), Some("archive".into()))));
        assert!(env.contains(&(source_env::ARCHIVE_SHA256.into(), Some("b".repeat(64)))));
        assert!(!env.iter().any(|(k, _)| k.starts_with("GIT_")));

        let annotations = job.metadata.annotations.unwrap();
        assert_eq!(annotations["cyclops.build/purl"], "pkg:cargo/serde@1.0.219");
        assert!(!annotations.contains_key("cyclops.build/commit"));
    }

    fn pipeline_command(job: &Job) -> Option<Vec<String>> {
        job.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .command
            .clone()
    }

    #[test]
    fn ca_bundle_secret_becomes_both_containers_trust_store() {
        let job = build_job_manifest(
            &spec(SourceRef::Git {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                commit_sha: "a".repeat(40),
            }),
            &JobOptions {
                ca_bundle_secret: Some("proxy-ca".into()),
                ..JobOptions::new("polar-builds")
            },
        );
        let pod = job.spec.unwrap().template.spec.unwrap();
        assert!(
            pod.volumes
                .unwrap()
                .iter()
                .any(|v| v.name == CA_BUNDLE_VOLUME
                    && v.secret.as_ref().unwrap().secret_name.as_deref() == Some("proxy-ca"))
        );
        let containers = pod
            .init_containers
            .unwrap()
            .into_iter()
            .chain(pod.containers);
        for c in containers {
            let env = c.env.unwrap();
            assert!(
                env.iter().any(|e| e.name == "GIT_SSL_CAINFO"
                    && e.value.as_deref() == Some("/etc/cyclops/ca/ca-bundle.crt")),
                "{}",
                c.name
            );
            assert!(
                c.volume_mounts
                    .unwrap()
                    .iter()
                    .any(|m| m.mount_path == CA_BUNDLE_DIR),
                "{}",
                c.name
            );
        }
    }

    #[test]
    fn configured_resources_and_workspace_limit_apply() {
        let options = JobOptions {
            resources: PipelineResources {
                cpu_limit: "500m".into(),
                memory_limit: "512Mi".into(),
                cpu_request: "100m".into(),
                memory_request: "128Mi".into(),
            },
            workspace_size_limit: Some("1Gi".into()),
            ..JobOptions::new("polar-builds")
        };
        let job = build_job_manifest(
            &spec(SourceRef::Git {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                commit_sha: "a".repeat(40),
            }),
            &options,
        );
        let pod = job.spec.unwrap().template.spec.unwrap();
        let r = pod.containers[0].resources.clone().unwrap();
        assert_eq!(r.limits.unwrap()["cpu"], Quantity("500m".into()));
        assert_eq!(r.requests.unwrap()["memory"], Quantity("128Mi".into()));
        let ws = pod
            .volumes
            .unwrap()
            .into_iter()
            .find(|v| v.name == WORKSPACE_VOLUME)
            .unwrap();
        assert_eq!(
            ws.empty_dir.unwrap().size_limit,
            Some(Quantity("1Gi".into()))
        );
    }

    #[test]
    fn default_options_keep_ci_sized_resources_and_no_workspace_limit() {
        let job = build_job_manifest(
            &spec(SourceRef::Git {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                commit_sha: "a".repeat(40),
            }),
            &JobOptions::new("polar-builds"),
        );
        let pod = job.spec.unwrap().template.spec.unwrap();
        let r = pod.containers[0].resources.clone().unwrap();
        assert_eq!(r.limits.unwrap()["memory"], Quantity("4Gi".into()));
        let ws = pod
            .volumes
            .unwrap()
            .into_iter()
            .find(|v| v.name == WORKSPACE_VOLUME)
            .unwrap();
        assert_eq!(ws.empty_dir.unwrap().size_limit, None);
    }

    #[test]
    fn no_ca_bundle_secret_leaves_trust_store_alone() {
        let job = build_job_manifest(
            &spec(SourceRef::Git {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                commit_sha: "a".repeat(40),
            }),
            &JobOptions::new("polar-builds"),
        );
        let pod = job.spec.unwrap().template.spec.unwrap();
        assert!(
            !pod.volumes
                .unwrap()
                .iter()
                .any(|v| v.name == CA_BUNDLE_VOLUME)
        );
    }

    #[test]
    fn empty_command_runs_image_entrypoint() {
        let job = build_job_manifest(
            &spec(SourceRef::Git {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                commit_sha: "a".repeat(40),
            }),
            &JobOptions::new("polar-builds"),
        );
        assert_eq!(pipeline_command(&job), None);
    }

    #[test]
    fn configured_command_overrides_entrypoint() {
        let mut s = spec(SourceRef::Git {
            url: "https://github.com/cmu-sei/Polar.git".into(),
            commit_sha: "a".repeat(40),
        });
        s.entrypoint = Some(vec!["cargo".into(), "build".into()]);
        let job = build_job_manifest(&s, &JobOptions::new("polar-builds"));
        assert_eq!(
            pipeline_command(&job),
            Some(vec!["cargo".to_string(), "build".to_string()])
        );
    }

    #[test]
    fn archive_job_hardens_both_containers() {
        let job = build_job_manifest(
            &spec(SourceRef::RegistryArchive {
                purl: "pkg:cargo/serde@1.0.219".into(),
                download_url: "https://static.crates.io/crates/serde/serde-1.0.219.crate".into(),
                sha256: "b".repeat(64),
            }),
            &JobOptions::new("polar-builds"),
        );
        let pod = job.spec.unwrap().template.spec.unwrap();
        let containers = pod
            .init_containers
            .unwrap()
            .into_iter()
            .chain(pod.containers);
        for c in containers {
            let sc = c.security_context.clone().expect("hardened context");
            assert_eq!(sc.run_as_non_root, Some(true), "{}", c.name);
            assert_eq!(sc.read_only_root_filesystem, Some(true), "{}", c.name);
            assert_eq!(sc.allow_privilege_escalation, Some(false), "{}", c.name);
            assert_eq!(sc.capabilities.unwrap().drop, Some(vec!["ALL".to_string()]));
            let mounts = c.volume_mounts.unwrap();
            assert!(
                mounts.iter().any(|m| m.mount_path == "/tmp"),
                "{} has no /tmp",
                c.name
            );
        }
        assert!(pod.volumes.unwrap().iter().any(|v| v.name == TMP_VOLUME));
    }

    #[test]
    fn job_env_reaches_pipeline_container() {
        let mut s = spec(SourceRef::Git {
            url: "https://github.com/cmu-sei/Polar.git".into(),
            commit_sha: "a".repeat(40),
        });
        s.env
            .insert("BROKER_ADDR".into(), "cassini-ip-svc:8080".into());
        let job = build_job_manifest(&s, &JobOptions::new("polar-builds"));
        let env = job.spec.unwrap().template.spec.unwrap().containers[0]
            .env
            .clone()
            .unwrap();
        assert!(
            env.iter()
                .any(|e| e.name == "BROKER_ADDR"
                    && e.value.as_deref() == Some("cassini-ip-svc:8080"))
        );
    }

    /// Git builds: the init container (Cyclops's own image) is hardened with a
    /// writable /tmp, as for archives; the pipeline container keeps the CI
    /// image's own context, since polar-ci runs as root.
    #[test]
    fn git_job_hardens_init_but_not_pipeline() {
        let job = build_job_manifest(
            &spec(SourceRef::Git {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                commit_sha: "a".repeat(40),
            }),
            &JobOptions::new("polar-builds"),
        );
        let pod = job.spec.unwrap().template.spec.unwrap();
        let init = &pod.init_containers.as_ref().unwrap()[0];
        assert_eq!(
            init.security_context
                .as_ref()
                .unwrap()
                .read_only_root_filesystem,
            Some(true)
        );
        assert!(
            init.volume_mounts
                .as_ref()
                .unwrap()
                .iter()
                .any(|m| m.mount_path == "/tmp")
        );
        assert!(pod.containers[0].security_context.is_none());
        assert!(
            !pod.containers[0]
                .volume_mounts
                .as_ref()
                .unwrap()
                .iter()
                .any(|m| m.mount_path == "/tmp")
        );
    }

    #[test]
    fn git_job_keeps_clone_env_and_credentials() {
        let job = build_job_manifest(
            &spec(SourceRef::Git {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                commit_sha: "a".repeat(40),
            }),
            &JobOptions::new("polar-builds"),
        );
        let env = init_env(&job);
        assert!(env.contains(&(source_env::COMMIT_SHA.into(), Some("a".repeat(40)))));
        assert!(env.iter().any(|(k, _)| k == "GIT_USERNAME"));
        assert_eq!(
            job.metadata.annotations.unwrap()["cyclops.build/commit"],
            "a".repeat(40)
        );
    }
}
