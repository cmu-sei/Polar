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

//! Kubernetes Job manifest construction for build-orchestrator build and bootstrap jobs.
//!
//! # Job anatomy
//!
//! Every job — whether a pipeline job or a bootstrap job — follows the same
//! two-container structure:
//!
//! ```text
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
use orchestrator_core::backend::Retention;
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
    types::{BuildSpec, RegistryCredentials, SourceRef},
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
const CLONE_INIT_IMAGE: &str = "build-orchestrator/git-clone:latest";

/// Name of the emptyDir volume shared between init and main containers.
const WORKSPACE_VOLUME: &str = "workspace";

/// Scratch emptyDir mounted at /tmp in hardened jobs, whose root filesystems
/// are read-only. The cassini daemon's socket and queue live here.
const TMP_VOLUME: &str = "tmp";

/// Name of the volume that holds the build's git credential, and where the clone container sees it. The
/// credential is a Secret of the build's own, mounted as files into the clone container only — never an
/// environment variable, and never in the pipeline container.
const GIT_CREDS_VOLUME: &str = "git-credentials";
pub const GIT_CREDS_DIR: &str = "/run/git-credentials";

pub const CLIENT_TLS_SECRET_NAME: &str = "client-tls";

/// The Secret that holds a build's git credential: made with the job and owned by it, so it goes with it.
pub fn git_secret_name_for_build(build_id: Uuid) -> String {
    format!("build-{}-git", build_id)
}

/// The ServiceAccount a Job's pod runs under, if the Job has not finished: the one it holds. The accounts are a
/// fixed set that the deployment makes (`JobOptions::accounts`); a Job holds its account until it completes or
/// fails, and then the account is free for the next build.
pub fn account_held_by(job: &Job) -> Option<String> {
    let finished = job
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .is_some_and(|cs| {
            cs.iter()
                .any(|c| (c.type_ == "Complete" || c.type_ == "Failed") && c.status == "True")
        });
    if finished {
        return None;
    }
    job.spec
        .as_ref()?
        .template
        .spec
        .as_ref()?
        .service_account_name
        .clone()
}

/// The first of the deployment's accounts that no unfinished Job holds. The broker allows one session per
/// identity, so a build must not share its account with another that is running.
pub fn free_account<'a>(accounts: &'a [String], jobs: &[Job]) -> Option<&'a String> {
    let held: Vec<String> = jobs.iter().filter_map(account_held_by).collect();
    accounts.iter().find(|a| !held.contains(a))
}

/// The ServiceAccount a job's pod runs under, or `None` when jobs have no certificate of their own. With a
/// `shared_account` it is that one, however many jobs hold it. Otherwise it is the first of `accounts` no unfinished
/// job holds, or, when there is none, `Err` with the number of accounts that are all held.
pub fn choose_account(options: &JobOptions, jobs: &[Job]) -> Result<Option<String>, usize> {
    if options.identity.is_none() {
        return Ok(None);
    }
    if let Some(shared) = &options.shared_account {
        return Ok(Some(shared.clone()));
    }
    free_account(&options.accounts, jobs)
        .cloned()
        .map(Some)
        .ok_or(options.accounts.len())
}

/// How a finished Job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Succeeded,
    Failed,
}

/// How a Job ended and when (seconds since the epoch), if it has: its Complete or Failed condition, once true. A
/// Job that has not finished, or whose condition has no time, has no end.
pub fn ended(job: &Job) -> Option<(Ended, i64)> {
    job.status
        .as_ref()?
        .conditions
        .as_ref()?
        .iter()
        .find_map(|c| {
            let how = match c.type_.as_str() {
                "Complete" => Ended::Succeeded,
                "Failed" => Ended::Failed,
                _ => return None,
            };
            if c.status != "True" {
                return None;
            }
            Some((how, c.last_transition_time.as_ref()?.0.as_second()))
        })
}

/// The Jobs that finished longer ago than `retention` allows for how they ended. A Job still running is never one.
pub fn expired<'a>(jobs: &'a [Job], now: i64, retention: &Retention) -> Vec<&'a Job> {
    jobs.iter()
        .filter(|job| {
            let Some((how, at)) = ended(job) else {
                return false;
            };
            let keep = match how {
                Ended::Succeeded => retention.succeeded,
                Ended::Failed => retention.failed,
            };
            now.saturating_sub(at) > i64::try_from(keep.as_secs()).unwrap_or(i64::MAX)
        })
        .collect()
}

/// The pod additions that give each job a certificate of its own: the library's `polar-init` init container
/// with its projected token, the volume the certificate is written to, and what the main container is told.
/// Read from the configuration file as JSON (`serde_json`), since the file's loader lowercases the keys of a
/// map, which Kubernetes' camelCase names cannot survive.
///
/// Without one, jobs share the Secret named `CLIENT_TLS_SECRET_NAME`, and builds that overlap supersede each
/// other on the broker.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobIdentity {
    /// Run before the clone, with the token the pod's ServiceAccount is given.
    #[serde(default)]
    pub init_containers: Vec<Container>,
    #[serde(default)]
    pub volumes: Vec<Volume>,
    /// Where the pipeline container finds its certificate.
    #[serde(default)]
    pub pipeline_mounts: Vec<VolumeMount>,
    #[serde(default)]
    pub pipeline_env: Vec<EnvVar>,
}

impl JobIdentity {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}
pub const CLIENT_TLS_VOLUME_NAME: &str = CLIENT_TLS_SECRET_NAME;

/// Derives a deterministic, DNS-safe Job name from a build ID.
/// Kubernetes names must be <= 63 chars, lowercase alphanumeric and hyphens.
/// A UUID is 36 chars; with the prefix "build-" (14 chars) = 50 chars total.
pub fn job_name_for_build(build_id: Uuid) -> String {
    format!("build-{}", build_id)
}

/// Same naming convention for bootstrap jobs.
/// Distinguished from pipeline jobs so they are identifiable in the cluster
/// without having to inspect annotations.
pub fn job_name_for_bootstrap(build_id: Uuid) -> String {
    format!("build-bootstrap-{}", build_id)
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
    /// `Always`, `IfNotPresent` or `Never` for the clone and pipeline images. Left alone, a `latest` tag is
    /// pulled every time, which an image loaded into a node by hand cannot survive.
    pub image_pull_policy: Option<String>,
    /// A certificate for the job, issued to the ServiceAccount its pod runs under (see `JobIdentity`).
    pub identity: Option<JobIdentity>,
    /// The ServiceAccounts a job may run under, made by the deployment. Each is a workload the authority model
    /// knows, and the broker allows one session per identity, so a job takes an account no running job holds
    /// (`free_account`). Needed with an `identity`.
    pub accounts: Vec<String>,
    /// The one ServiceAccount every job runs under, when its workload has many instances (the fork's authority model,
    /// "Multi-instance workloads"): each pod has a certificate and a session of its own, so jobs never wait for it.
    /// Exclusive with `accounts`. Needed with an `identity` when `accounts` is empty.
    pub shared_account: Option<String>,
    /// Keep the repository's `.git` in the workspace. The instrumented pipeline records which commit and tree it
    /// built, so it needs it; a pipeline that only reads source does not.
    pub keep_git: bool,
    /// How long finished jobs are kept. The orchestrator applies it by outcome (`expired`); Kubernetes' own expiry
    /// on each Job is set to the longest of them, so that jobs are still collected if the orchestrator is not
    /// running to apply it.
    pub retention: Retention,
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
            image_pull_policy: None,
            identity: None,
            accounts: Vec::new(),
            shared_account: None,
            keep_git: false,
            retention: Retention::default(),
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
    build_job_manifest_as(spec, options, None)
}

/// The same, with the pod running under `account` (one of `JobOptions::accounts`) when the job has an identity.
pub fn build_job_manifest_as(spec: &BuildSpec, options: &JobOptions, account: Option<&str>) -> Job {
    let namespace = options.namespace.as_str();
    let job_name = job_name_for_build(spec.build_id);
    let (labels, annotations) = standard_metadata(spec.build_id, &spec.source, "pipeline");

    let has_git_secret = spec.git_secret.is_some() && matches!(spec.source, SourceRef::Git { .. });
    let mut init_container = clone_init_container(&spec.source, options.keep_git, has_git_secret);
    init_container.image_pull_policy = options.image_pull_policy.clone();

    let identity = options.identity.as_ref();
    let mut pipeline_env = pipeline_env_vars(spec, identity.is_none());
    if let Some(identity) = identity {
        pipeline_env.extend(identity.pipeline_env.iter().cloned());
    }

    // When entrypoint is None the image's own CMD/ENTRYPOINT runs (/bin/pipeline-runner
    // for pipeline images built from container.dhall, or the analyzer's own entrypoint).
    // Otherwise it overrides that, mirroring the Podman backend.
    let mut pipeline_container = Container {
        name: "pipeline".to_string(),
        image: Some(spec.pipeline_image.clone()),

        image_pull_policy: options.image_pull_policy.clone(),
        command: spec.entrypoint.clone(),
        env: Some(pipeline_env),
        // TODO: allow users to specify their own security context via config for git builds
        resources: Some(pipeline_resources(&options.resources)),
        volume_mounts: Some(
            match identity {
                Some(identity) => identity.pipeline_mounts.clone(),
                None => vec![client_tls_volume_mount()],
            }
            .into_iter()
            // A git build's pipeline writes beside its source (its artifacts, its build directory); only
            // an archive, which is third-party code under analysis, gets the source read-only.
            .chain([workspace_volume_mount(matches!(
                spec.source,
                SourceRef::RegistryArchive { .. }
            ))])
            .collect(),
        ),
        ..Default::default()
    };

    let mut volumes = vec![workspace_volume(options.workspace_size_limit.as_deref())];
    match identity {
        Some(identity) => volumes.extend(identity.volumes.iter().cloned()),
        None => volumes.push(client_tls_voiume()),
    }
    if has_git_secret {
        volumes.push(git_secret_volume(spec.build_id));
    }

    // The init container is build-orchestrator'ss own minimal image and never needs
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

    // The certificate is obtained before anything else runs, and before the source is fetched.
    let init_containers: Vec<Container> = identity
        .map(|i| i.init_containers.clone())
        .unwrap_or_default()
        .into_iter()
        .chain([init_container])
        .collect();

    let pod_spec = PodSpec {
        init_containers: Some(init_containers),
        service_account_name: identity.and(account).map(str::to_string),
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

    assemble_job(
        job_name,
        namespace,
        labels,
        annotations,
        pod_spec,
        options.retention.longest(),
    )
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
fn clone_init_container(source: &SourceRef, keep_git: bool, git_secret: bool) -> Container {
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

    // Archive sources are public downloads and never receive git credentials. A git source gets the
    // build's own credential, when its repository's mapping names one, as files: the clone script reads
    // them through git's askpass, so the value is in no URL, no environment and no process argument.
    if let SourceRef::Git { .. } = source {
        if git_secret {
            env.push(EnvVar {
                name: "CYCLOPS_GIT_CREDENTIALS_DIR".to_string(),
                value: Some(GIT_CREDS_DIR.to_string()),
                ..Default::default()
            });
        }
        if keep_git {
            env.push(EnvVar {
                name: "CYCLOPS_KEEP_GIT".to_string(),
                value: Some("1".to_string()),
                ..Default::default()
            });
        }
    }

    let mut mounts = vec![VolumeMount {
        name: WORKSPACE_VOLUME.to_string(),
        mount_path: WORKSPACE_PATH.to_string(),
        // Init container writes — must be writable.
        read_only: Some(false),
        ..Default::default()
    }];
    if git_secret {
        mounts.push(VolumeMount {
            name: GIT_CREDS_VOLUME.to_string(),
            mount_path: GIT_CREDS_DIR.to_string(),
            read_only: Some(true),
            ..Default::default()
        });
    }

    Container {
        name: "clone".to_string(),
        image_pull_policy: Some("Never".to_string()), // TODO: remove NEVER pull policy.
        image: Some(CLONE_INIT_IMAGE.to_string()),
        env: Some(env),
        // TODO: enable hardened sec context when we get a controlled clone image.
        // security_context: Some(hardened_security_context()),
        resources: Some(clone_resources()),
        volume_mounts: Some(mounts),
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
fn pipeline_env_vars(spec: &BuildSpec, shared_tls: bool) -> Vec<EnvVar> {
    let var = |name: &str, value: String| EnvVar {
        name: name.to_string(),
        value: Some(value),
        ..Default::default()
    };
    let mut vars = vec![
        var("POLAR_BUILD_ID", spec.build_id.to_string()),
        var("BUILD_WORKSPACE", WORKSPACE_PATH.to_string()),
    ];
    // With a certificate of its own, where it is comes from the identity (`JobIdentity::pipeline_env`).
    if shared_tls {
        vars.extend([
            var("TLS_CA_CERT", tls_ca_cert_path()),
            var("TLS_CLIENT_CERT", tls_client_cert_path()),
            var("TLS_CLIENT_KEY", tls_client_key_path()),
        ]);
    }
    vars.into_iter()
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

// Env vars that source git credentials from a k8s Secret would be built here: values pulled from the Secret at pod
// startup by the kubelet, never present in the Job manifest itself, which matters for audit log hygiene. Anyone
// reading the Job spec sees only which secret is referenced, not what the credentials are.

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
#[allow(dead_code)] // for the bootstrap job, which is not submitted yet
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
fn workspace_volume_mount(read_only: bool) -> VolumeMount {
    VolumeMount {
        name: WORKSPACE_VOLUME.to_string(),
        mount_path: WORKSPACE_PATH.to_string(),
        read_only: Some(read_only),
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

/// The volume that holds the build's git credential, from the Secret the backend made for it.
///
/// It is mounted into the clone container only, not the pipeline container: the pipeline has no business
/// touching the credential it was cloned with, and nothing here puts the value in an environment variable.
fn git_secret_volume(build_id: Uuid) -> Volume {
    Volume {
        name: GIT_CREDS_VOLUME.to_string(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(git_secret_name_for_build(build_id)),
            // optional: false — a pod whose Secret is not there waits, rather than start without it.
            optional: Some(false),
            // The clone container does not run as root; only it mounts this volume.
            default_mode: Some(0o444),
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
#[allow(dead_code)] // for the bootstrap job, which is not submitted yet
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
const CA_BUNDLE_DIR: &str = "/etc/build-orchestrator/ca";
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
/// `GIT_SSL_CAINFO`, curl `CURL_CA_BUNDLE`, Nix `NIX_SSL_CERT_FILE`, and cargo
/// (its own git fetches too) `CARGO_HTTP_CAINFO`. These override the defaults
/// baked into the images.
fn trust_ca_bundle(container: &mut Container) {
    let bundle = format!("{CA_BUNDLE_DIR}/{CA_BUNDLE_KEY}");
    let env = container.env.get_or_insert_with(Vec::new);
    for name in [
        "SSL_CERT_FILE",
        "GIT_SSL_CAINFO",
        "CURL_CA_BUNDLE",
        "NIX_SSL_CERT_FILE",
        "CARGO_HTTP_CAINFO",
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
#[allow(dead_code)] // for the bootstrap job, which is not submitted yet
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

/// Standard labels and annotations applied to all build-orchestrator Jobs.
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
        "build-orchestrator".to_string(),
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
    keep_finished: std::time::Duration,
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
            // Kubernetes removes a finished job after the longest the retention policy keeps one. The orchestrator
            // removes them sooner by how they ended (`expired`); this is for when it is not running. A job's
            // logs go to the object store as it finishes, before either fires.
            ttl_seconds_after_finished: Some(
                i32::try_from(keep_finished.as_secs()).unwrap_or(i32::MAX),
            ),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::types::{GitCredentials, GitSecret, source_env};

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
                    && e.value.as_deref() == Some("/etc/build-orchestrator/ca/ca-bundle.crt")),
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
    fn the_clone_keeps_git_only_when_asked() {
        let source = || SourceRef::Git {
            url: "https://github.com/cmu-sei/Polar.git".into(),
            commit_sha: "a".repeat(40),
        };
        let keeps = |options: &JobOptions| {
            let job = build_job_manifest(&spec(source()), options);
            let pod = job.spec.unwrap().template.spec.unwrap();
            pod.init_containers
                .unwrap()
                .iter()
                .find(|c| c.name == "clone")
                .unwrap()
                .env
                .clone()
                .unwrap()
                .iter()
                .any(|e| e.name == "CYCLOPS_KEEP_GIT" && e.value.as_deref() == Some("1"))
        };
        assert!(!keeps(&JobOptions::new("polar-builds")));
        let mut options = JobOptions::new("polar-builds");
        options.keep_git = true;
        assert!(keeps(&options));
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

    /// Git builds: the init container (build-orchestrator'ss own image) is hardened with a
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
        assert!(
            !env.iter()
                .any(|(k, _)| k.starts_with("GIT_USERNAME") || k.starts_with("GIT_PASSWORD")),
            "a credential is never an environment variable"
        );
        assert_eq!(
            job.metadata.annotations.unwrap()["cyclops.build/commit"],
            "a".repeat(40)
        );
    }

    fn with_secret(mut spec: BuildSpec) -> BuildSpec {
        spec.git_secret = Some(GitSecret {
            username: "deploy".into(),
            password: "tokentokentoken".into(),
        });
        spec
    }

    #[test]
    fn a_git_credential_is_a_secret_volume_mounted_read_only_into_the_clone_container_alone() {
        let build = with_secret(git_spec());
        let job = build_job_manifest(&build, &JobOptions::new("polar-builds"));
        let pod = pod(&job);

        let volume = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == GIT_CREDS_VOLUME)
            .unwrap();
        assert_eq!(
            volume.secret.as_ref().unwrap().secret_name.as_deref(),
            Some(git_secret_name_for_build(build.build_id).as_str())
        );

        let clone = &pod.init_containers.as_ref().unwrap()[0];
        let mount = clone
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.name == GIT_CREDS_VOLUME)
            .unwrap();
        assert_eq!(mount.mount_path, GIT_CREDS_DIR);
        assert_eq!(mount.read_only, Some(true));
        let dir = clone
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == "CYCLOPS_GIT_CREDENTIALS_DIR")
            .unwrap();
        assert_eq!(dir.value.as_deref(), Some(GIT_CREDS_DIR));

        for container in pod
            .containers
            .iter()
            .chain(pod.init_containers.as_ref().unwrap().iter().skip(1))
        {
            assert!(
                !container
                    .volume_mounts
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|m| m.name == GIT_CREDS_VOLUME),
                "only the clone container sees the credential, not {}",
                container.name
            );
        }
    }

    #[test]
    fn the_credential_is_in_no_manifest_field() {
        let build = with_secret(git_spec());
        let job = build_job_manifest(&build, &JobOptions::new("polar-builds"));
        let text = serde_json::to_string(&job).unwrap();
        assert!(!text.contains("tokentokentoken"));
        assert!(
            !format!("{build:?}").contains("tokentokentoken"),
            "its Debug must not print it"
        );
    }

    #[test]
    fn a_build_with_no_credential_has_no_volume_and_the_clone_is_told_nothing() {
        let job = build_job_manifest(&git_spec(), &JobOptions::new("polar-builds"));
        let pod = pod(&job);
        assert!(
            !pod.volumes
                .as_ref()
                .unwrap()
                .iter()
                .any(|v| v.name == GIT_CREDS_VOLUME)
        );
        let clone = &pod.init_containers.as_ref().unwrap()[0];
        assert!(
            !clone
                .env
                .as_ref()
                .unwrap()
                .iter()
                .any(|e| e.name == "CYCLOPS_GIT_CREDENTIALS_DIR")
        );
    }

    const IDENTITY: &str = r#"{"initContainers":[{"name":"polar-init","image":"polar-init:latest","command":["polar-init"],"args":["--cert","client:/home/polar/certs:ecdsa-p256"],"volumeMounts":[{"name":"sa-token","mountPath":"/workspace","readOnly":true},{"name":"polar-certs","mountPath":"/home/polar/certs"}]}],"volumes":[{"name":"sa-token","projected":{"sources":[{"serviceAccountToken":{"audience":"a","expirationSeconds":3600,"path":"token"}}]}},{"name":"polar-certs","emptyDir":{}}],"pipelineMounts":[{"name":"polar-certs","mountPath":"/home/polar/certs","readOnly":true}],"pipelineEnv":[{"name":"TLS_CLIENT_CERT","value":"/home/polar/certs/cert.pem"}]}"#;

    fn git_spec() -> BuildSpec {
        spec(SourceRef::Git {
            url: "https://github.com/cmu-sei/Polar.git".into(),
            commit_sha: "a".repeat(40),
        })
    }

    fn pod(job: &Job) -> &PodSpec {
        job.spec.as_ref().unwrap().template.spec.as_ref().unwrap()
    }

    #[test]
    fn a_job_with_an_identity_runs_as_the_account_it_is_given_and_gets_its_certificate_first() {
        let mut options = JobOptions::new("polar-builds");
        options.identity = Some(JobIdentity::from_json(IDENTITY).unwrap());
        let build = git_spec();
        let job = build_job_manifest_as(&build, &options, Some("build-job-2"));
        let pod = pod(&job);

        assert_eq!(pod.service_account_name.as_deref(), Some("build-job-2"));
        let inits: Vec<_> = pod
            .init_containers
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(inits.len(), 2);
        assert_eq!(
            inits[0], "polar-init",
            "the certificate comes before the source"
        );

        let volumes: Vec<_> = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(
            !volumes.contains(&CLIENT_TLS_VOLUME_NAME),
            "no certificate is shared between builds: {volumes:?}"
        );
        assert!(volumes.contains(&"polar-certs") && volumes.contains(&"sa-token"));

        let pipeline = &pod.containers[0];
        let tls_cert: Vec<_> = pipeline
            .env
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.name == "TLS_CLIENT_CERT")
            .collect();
        assert_eq!(
            tls_cert.len(),
            1,
            "where the certificate is comes from the identity alone"
        );
        assert_eq!(
            tls_cert[0].value.as_deref(),
            Some("/home/polar/certs/cert.pem")
        );
        let mounts = pipeline.volume_mounts.as_ref().unwrap();
        assert!(
            mounts
                .iter()
                .any(|m| m.name == "polar-certs" && m.read_only == Some(true))
        );
        assert!(
            mounts
                .iter()
                .any(|m| m.name == WORKSPACE_VOLUME && m.read_only == Some(false)),
            "a git build's pipeline writes beside its source"
        );
    }

    fn a_job_as(account: &str, conditions: &[(&str, &str)]) -> Job {
        use k8s_openapi::api::batch::v1::{JobCondition, JobStatus};
        let mut options = JobOptions::new("polar-builds");
        options.identity = Some(JobIdentity::from_json(IDENTITY).unwrap());
        let mut job = build_job_manifest_as(&git_spec(), &options, Some(account));
        job.status = Some(JobStatus {
            conditions: Some(
                conditions
                    .iter()
                    .map(|(t, s)| JobCondition {
                        type_: t.to_string(),
                        status: s.to_string(),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        });
        job
    }

    fn accounts() -> Vec<String> {
        vec!["build-job-1".into(), "build-job-2".into()]
    }

    #[test]
    fn a_job_takes_the_first_account_no_running_job_holds() {
        assert_eq!(
            free_account(&accounts(), &[]).map(String::as_str),
            Some("build-job-1")
        );
        let running = [a_job_as("build-job-1", &[])];
        assert_eq!(
            free_account(&accounts(), &running).map(String::as_str),
            Some("build-job-2")
        );
        let both = [
            a_job_as("build-job-1", &[]),
            a_job_as("build-job-2", &[("Suspended", "False")]),
        ];
        assert_eq!(
            free_account(&accounts(), &both),
            None,
            "two running builds leave nothing to give a third"
        );
    }

    fn options_with(
        identity: bool,
        accounts: Vec<String>,
        shared_account: Option<&str>,
    ) -> JobOptions {
        let mut options = JobOptions::new("polar-builds");
        if identity {
            options.identity = Some(JobIdentity::from_json(IDENTITY).unwrap());
        }
        options.accounts = accounts;
        options.shared_account = shared_account.map(str::to_string);
        options
    }

    #[test]
    fn a_shared_account_is_given_to_every_job_however_many_hold_it() {
        let options = options_with(true, Vec::new(), Some("build-job"));
        let running = [
            a_job_as("build-job", &[]),
            a_job_as("build-job", &[]),
            a_job_as("build-job", &[]),
        ];
        assert_eq!(
            choose_account(&options, &running),
            Ok(Some("build-job".to_string())),
            "a multi-instance workload's pods each have a certificate, so a job never waits for the account"
        );
    }

    #[test]
    fn exclusive_accounts_are_still_taken_one_job_at_a_time() {
        let options = options_with(true, accounts(), None);
        let running = [a_job_as("build-job-1", &[])];
        assert_eq!(
            choose_account(&options, &running),
            Ok(Some("build-job-2".to_string()))
        );
        let both = [a_job_as("build-job-1", &[]), a_job_as("build-job-2", &[])];
        assert_eq!(
            choose_account(&options, &both),
            Err(2),
            "every one of the two accounts is held"
        );
    }

    #[test]
    fn a_job_without_a_certificate_of_its_own_runs_under_no_account_of_ours() {
        assert_eq!(
            choose_account(&options_with(false, accounts(), None), &[]),
            Ok(None)
        );
        assert_eq!(
            choose_account(&options_with(false, Vec::new(), Some("build-job")), &[]),
            Ok(None)
        );
    }

    #[test]
    fn a_finished_job_gives_its_account_back() {
        let done = [
            a_job_as("build-job-1", &[("Complete", "True")]),
            a_job_as("build-job-2", &[("Failed", "True")]),
        ];
        assert_eq!(
            free_account(&accounts(), &done).map(String::as_str),
            Some("build-job-1")
        );
        // A condition that is not true does not end the job.
        assert!(account_held_by(&a_job_as("build-job-1", &[("Failed", "False")])).is_some());
    }

    #[test]
    fn a_job_without_an_identity_holds_no_account() {
        let job = build_job_manifest(&git_spec(), &JobOptions::new("polar-builds"));
        assert_eq!(account_held_by(&job), None);
    }

    /// A Job whose one condition became true at `at` (seconds since the epoch).
    fn a_job_that_ended(condition: &str, at: i64) -> Job {
        use k8s_openapi::api::batch::v1::{JobCondition, JobStatus};
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
        let mut job = a_job_as("build-job-1", &[]);
        job.status = Some(JobStatus {
            conditions: Some(vec![JobCondition {
                type_: condition.to_string(),
                status: "True".to_string(),
                last_transition_time: Some(Time(
                    k8s_openapi::jiff::Timestamp::from_second(at).unwrap(),
                )),
                ..Default::default()
            }]),
            ..Default::default()
        });
        job
    }

    const HOUR: i64 = 60 * 60;

    #[test]
    fn a_job_is_kept_for_as_long_as_its_outcome_is_retained() {
        let retention = Retention::default();
        let now = 100 * 24 * HOUR;
        let jobs = [
            a_job_that_ended("Complete", now - 2 * HOUR),
            a_job_that_ended("Complete", now - HOUR / 2),
            a_job_that_ended("Failed", now - 2 * HOUR),
            a_job_that_ended("Failed", now - 25 * HOUR),
        ];
        let gone: Vec<usize> = expired(&jobs, now, &retention)
            .iter()
            .map(|j| jobs.iter().position(|k| std::ptr::eq(k, *j)).unwrap())
            .collect();
        assert_eq!(
            gone,
            vec![0, 3],
            "an old success and a very old failure; a recent one of each stays"
        );
    }

    #[test]
    fn a_job_that_has_not_finished_is_never_expired() {
        let now = 100 * 24 * HOUR;
        let running = a_job_as("build-job-1", &[]);
        let not_true = a_job_as("build-job-1", &[("Failed", "False")]);
        // A finished one with no time on its condition cannot be shown to be old.
        let mut untimed = a_job_as("build-job-1", &[("Complete", "True")]);
        untimed
            .status
            .as_mut()
            .unwrap()
            .conditions
            .as_mut()
            .unwrap()[0]
            .last_transition_time = None;
        assert!(expired(&[running, not_true, untimed], now, &Retention::default()).is_empty());
    }

    #[test]
    fn kubernetes_keeps_a_finished_job_as_long_as_the_longest_retention() {
        let mut options = JobOptions::new("polar-builds");
        options.retention = Retention {
            succeeded: std::time::Duration::from_secs(600),
            failed: std::time::Duration::from_secs(7200),
        };
        let job = build_job_manifest(&git_spec(), &options);
        assert_eq!(job.spec.unwrap().ttl_seconds_after_finished, Some(7200));
    }

    #[test]
    fn without_an_identity_jobs_share_the_secret_as_before() {
        let job = build_job_manifest(&git_spec(), &JobOptions::new("polar-builds"));
        let pod = pod(&job);
        assert!(pod.service_account_name.is_none());
        let volumes: Vec<_> = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(volumes.contains(&CLIENT_TLS_VOLUME_NAME));
    }

    #[test]
    fn an_archive_build_still_reads_its_source_read_only() {
        let job = build_job_manifest(
            &spec(SourceRef::RegistryArchive {
                purl: "pkg:cargo/serde@1.0.219".into(),
                download_url: "https://static.crates.io/crates/serde/serde-1.0.219.crate".into(),
                sha256: "b".repeat(64),
            }),
            &JobOptions::new("polar-builds"),
        );
        let mounts = pod(&job).containers[0].volume_mounts.as_ref().unwrap();
        assert!(
            mounts
                .iter()
                .any(|m| m.name == WORKSPACE_VOLUME && m.read_only == Some(true))
        );
    }
}
