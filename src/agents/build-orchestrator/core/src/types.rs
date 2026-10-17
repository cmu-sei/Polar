use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Cassini subjects the orchestrator agents are interested in
/// Mirrors the subject constants defined in cyclops-core.
pub mod subjects {
    pub const BUILD_REQUESTS_TOPIC: &str = "polar.orchestrator.builds.requests";
}

// ── Source references ─────────────────────────────────────────────────────────

/// Env var names shared between the orchestrator and the source-fetch init
/// container (the `git-clone-entrypoint` script in package.nix). Both sides
/// must agree on these.
pub mod source_env {
    /// "git" or "archive". Absent means "git" for older entrypoint compatibility.
    pub const SOURCE_KIND: &str = "CYCLOPS_SOURCE_KIND";
    pub const REPO_URL: &str = "CYCLOPS_REPO_URL";
    pub const COMMIT_SHA: &str = "CYCLOPS_COMMIT_SHA";
    pub const ARCHIVE_URL: &str = "CYCLOPS_ARCHIVE_URL";
    pub const ARCHIVE_SHA256: &str = "CYCLOPS_ARCHIVE_SHA256";
    pub const SOURCE_PURL: &str = "CYCLOPS_SOURCE_PURL";
}

/// What source a job runs against, and how to verify it.
///
/// Every variant pins content immutably — a commit SHA or a content hash,
/// never a branch, tag, or version string alone. The init container refuses
/// to populate the workspace if the fetched content does not match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRef {
    /// A git repository at a specific commit. The CI path: triggered by
    /// repository events, the orchestrator acting as the authoritative builder.
    Git {
        /// Full HTTPS or SSH URL to the source repository.
        url: String,
        /// Full 40-character commit SHA. Never a branch name or tag — those are mutable.
        commit_sha: String,
    },
    /// A published package archive (e.g. a `.crate` from crates.io). The
    /// dependency-analysis path: this is the code that was actually compiled
    /// into downstream artifacts, which is not necessarily what is in the
    /// package's declared git repository.
    ///
    /// The archive must be a gzipped tarball with a single top-level
    /// directory (the layout used by crates.io, npm, and PyPI sdists).
    RegistryArchive {
        /// Package URL. Join key to the `Package` node in the graph.
        purl: String,
        /// HTTPS URL the archive is downloaded from.
        download_url: String,
        /// Lowercase hex sha256 of the archive bytes, e.g. the `checksum`
        /// from Cargo.lock.
        sha256: String,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SourceRefError {
    #[error("commit_sha is not a valid 40-character hex SHA: {0}")]
    InvalidCommitSha(String),
    #[error("sha256 is not a valid 64-character lowercase hex digest: {0}")]
    InvalidSha256(String),
    #[error("archive download_url must be https: {0}")]
    InsecureDownloadUrl(String),
    #[error("purl must start with \"pkg:\": {0}")]
    InvalidPurl(String),
}

impl SourceRef {
    /// Check that the reference pins content immutably and can be fetched
    /// safely. Called at the request boundary so a bad reference fails the
    /// request, not the init container.
    pub fn validate(&self) -> Result<(), SourceRefError> {
        fn is_hex(s: &str, len: usize) -> bool {
            s.len() == len && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        }
        match self {
            SourceRef::Git { commit_sha, .. } => {
                if !is_hex(commit_sha, 40) {
                    return Err(SourceRefError::InvalidCommitSha(commit_sha.clone()));
                }
            }
            SourceRef::RegistryArchive {
                purl,
                download_url,
                sha256,
            } => {
                if !purl.starts_with("pkg:") {
                    return Err(SourceRefError::InvalidPurl(purl.clone()));
                }
                if !download_url.starts_with("https://") {
                    return Err(SourceRefError::InsecureDownloadUrl(download_url.clone()));
                }
                if !is_hex(sha256, 64) {
                    return Err(SourceRefError::InvalidSha256(sha256.clone()));
                }
            }
        }
        Ok(())
    }

    /// Where the source comes from, for logs and annotations.
    pub fn locator(&self) -> &str {
        match self {
            SourceRef::Git { url, .. } => url,
            SourceRef::RegistryArchive { download_url, .. } => download_url,
        }
    }

    /// The immutable content pin: commit SHA or archive sha256.
    pub fn revision(&self) -> &str {
        match self {
            SourceRef::Git { commit_sha, .. } => commit_sha,
            SourceRef::RegistryArchive { sha256, .. } => sha256,
        }
    }

    /// The `SOURCE_KIND` value for this reference.
    pub fn kind(&self) -> &'static str {
        match self {
            SourceRef::Git { .. } => "git",
            SourceRef::RegistryArchive { .. } => "archive",
        }
    }

    /// Env vars for the source-fetch init container. Does not include the
    /// workspace path or credentials — those are backend concerns.
    pub fn fetch_env(&self) -> Vec<(String, String)> {
        let mut env = vec![(source_env::SOURCE_KIND.to_string(), self.kind().to_string())];
        match self {
            SourceRef::Git { url, commit_sha } => {
                env.push((source_env::REPO_URL.to_string(), url.clone()));
                env.push((source_env::COMMIT_SHA.to_string(), commit_sha.clone()));
            }
            SourceRef::RegistryArchive {
                purl,
                download_url,
                sha256,
            } => {
                env.push((source_env::SOURCE_PURL.to_string(), purl.clone()));
                env.push((source_env::ARCHIVE_URL.to_string(), download_url.clone()));
                env.push((source_env::ARCHIVE_SHA256.to_string(), sha256.clone()));
            }
        }
        env
    }

    /// Source identity env vars for the pipeline container, so the job can
    /// attribute what it emits to the right source.
    pub fn pipeline_env(&self) -> Vec<(String, String)> {
        let mut env = vec![(source_env::SOURCE_KIND.to_string(), self.kind().to_string())];
        match self {
            SourceRef::Git { commit_sha, .. } => {
                env.push(("CI_COMMIT_SHORT_SHA".to_string(), commit_sha.clone()));
            }
            SourceRef::RegistryArchive { purl, sha256, .. } => {
                env.push((source_env::SOURCE_PURL.to_string(), purl.clone()));
                env.push((source_env::ARCHIVE_SHA256.to_string(), sha256.clone()));
            }
        }
        env
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BuildRequestError {
    #[error(transparent)]
    InvalidSource(#[from] SourceRefError),
    /// The graph records which analyzer image analyzed which package, so the
    /// image must identify immutable content — a tag can move.
    #[error("archive analysis requires an image pinned by digest (…@sha256:…), got: {0:?}")]
    UnpinnedAnalyzerImage(Option<String>),
}

impl BuildRequest {
    pub fn validate(&self) -> Result<(), BuildRequestError> {
        self.source.validate()?;
        if let SourceRef::RegistryArchive { .. } = self.source {
            let pinned = self
                .pipeline_image
                .as_deref()
                .is_some_and(|image| image.contains("@sha256:"));
            if !pinned {
                return Err(BuildRequestError::UnpinnedAnalyzerImage(
                    self.pipeline_image.clone(),
                ));
            }
        }
        Ok(())
    }
}

/// The inbound payload published by Polar's scheduler onto the Cassini broker.
/// This is the authoritative wire format — do not add fields without versioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRequest {
    /// Stable identifier for this build. Generated by the scheduler.
    pub build_id: Uuid,

    /// The source to run against.
    pub source: SourceRef,

    /// Image to run, pinned by digest. When None, the orchestrator resolves
    /// the image from its repo mappings, which only exist for git sources —
    /// analysis requests against archives must name their analyzer image.
    #[serde(default)]
    pub pipeline_image: Option<String>,

    /// Opaque identity string from Polar representing who or what requested the build.
    pub requested_by: String,

    /// Wall-clock time the scheduler emitted this request.
    pub requested_at: DateTime<Utc>,
    pub target_registry: String,

    /// Arbitrary key-value metadata forwarded from the scheduler.
    /// Used for forward-compatibility — do not rely on specific keys in core logic.
    #[serde(default)]
    pub metadata: HashMap<String, String>,

    pub command: Vec<String>,
}

/// The state machine for a single build lifecycle.
///
/// The two-phase image resolution flow sits before actual execution:
///
///   Pending
///     └─► ResolvingImage          ← check registry for pipeline image
///           ├─► Bootstrapping     ← image missing: run bootstrap job to build it
///           │     └─► Scheduled   ← bootstrap done, now schedule the real job
///           └─► Scheduled         ← image present, proceed directly
///                 └─► Running
///                       ├─► Succeeded
///                       └─► Failed
///
/// Cancelled is reachable from any non-terminal state.
/// Failed is reachable from ResolvingImage, Bootstrapping, Scheduled, and Running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    /// Received and validated. Image resolution has not started yet.
    Pending,

    /// Checking the registry for the pipeline image for this repo.
    ResolvingImage,

    /// Pipeline image was not found. A bootstrap job is running to build it.
    /// The bootstrap job uses the well-known base Nix builder image from config.
    Bootstrapping,

    /// Pipeline image is confirmed present. Build job submitted to the backend.
    Scheduled,

    /// Backend has confirmed the job is actively running.
    Running,

    /// Job completed successfully. Artifact produced.
    Succeeded,

    /// Job terminated with a failure at any stage.
    Failed,

    /// Cancelled before reaching a terminal state.
    Cancelled,
    /// Connectivity to the backend was lost while the job was in-flight.
    /// Terminal state of the record in this orchestrator instance.
    /// Requires reconciliation against backend state on reconnect
    /// before a new BuildRecord can be authoritatively issued.
    Unreconciled,
}

impl BuildState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub fn can_transition_to(&self, next: &BuildState) -> bool {
        use BuildState::*;
        matches!(
            (self, next),
            // Normal forward path
            (Pending, ResolvingImage)
            | (ResolvingImage, Bootstrapping)
            | (ResolvingImage, Scheduled)   // image was already present
            | (Bootstrapping, Scheduled)    // bootstrap completed, image now exists
            | (Scheduled, Running)
            | (Running, Succeeded)
            // Failure from any active state
            | (ResolvingImage, Failed)
            | (Bootstrapping, Failed)
            | (Scheduled, Failed)
            | (Running, Failed)
            // Cancellation from any non-terminal state
            | (Pending, Cancelled)
            | (ResolvingImage, Cancelled)
            | (Bootstrapping, Cancelled)
            | (Scheduled, Cancelled)
            | (Running, Cancelled)
            // Job actor loses connection to the backend, putting it in an unreconciled state
            | (Scheduled, Unreconciled)
            | (Bootstrapping, Unreconciled)
            | (Running, Unreconciled)
        )
    }
}

impl std::fmt::Display for BuildState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BuildState::Pending => "pending",
            BuildState::ResolvingImage => "resolving_image",
            BuildState::Bootstrapping => "bootstrapping",
            BuildState::Scheduled => "scheduled",
            BuildState::Running => "running",
            BuildState::Succeeded => "succeeded",
            BuildState::Failed => "failed",
            BuildState::Cancelled => "cancelled",
            BuildState::Unreconciled => "unreconciled",
        };
        write!(f, "{}", s)
    }
}

/// Operational record maintained in the registry actor for the duration of a build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRecord {
    pub build_id: Uuid,
    pub state: BuildState,
    pub request: BuildRequest,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Opaque handle returned by the backend on submission.
    /// None until Scheduled.
    pub backend_handle: Option<String>,
    /// Digest of the resolved pipeline image.
    /// Populated once ResolvingImage or Bootstrapping completes successfully.
    pub resolved_image: Option<String>,
    /// Human-readable reason for failure or cancellation, if applicable.
    pub failure_reason: Option<String>,
}

impl BuildRecord {
    pub fn new(request: BuildRequest) -> Self {
        let now = Utc::now();
        Self {
            build_id: request.build_id,
            state: BuildState::Pending,
            request,
            created_at: now,
            updated_at: now,
            backend_handle: None,
            resolved_image: None,
            failure_reason: None,
        }
    }

    pub fn transition(&mut self, next: BuildState) -> Result<(), crate::error::CyclopsError> {
        if self.state.can_transition_to(&next) {
            tracing::debug!(
                build_id = %self.build_id,
                from = %self.state,
                to = %next,
                "build state transition"
            );
            self.state = next;
            self.updated_at = Utc::now();
            Ok(())
        } else {
            Err(crate::error::CyclopsError::InvalidStateTransition {
                from: self.state.clone(),
                to: next,
            })
        }
    }
}

/// Fully resolved specification handed to the backend for a pipeline job.
///
/// By the time this is constructed, image resolution is complete — the pipeline
/// image digest is known and confirmed present in the registry. The backend
/// uses this to build the Kubernetes Job manifest.
///
/// Source code is NOT baked into the image. An init container clones the repo
/// into a shared emptyDir volume at /workspace before the pipeline container
/// starts. See job.rs for the init container implementation.
#[derive(Debug, Clone)]
pub struct BuildSpec {
    pub build_id: Uuid,
    pub source: SourceRef,

    /// Fully qualified pipeline image reference, pinned by digest.
    /// e.g. "registry.internal.example.com/builds/myapp@sha256:abc123..."
    /// The image must have /bin/pipeline-runner as its entrypoint.
    pub pipeline_image: String,

    /// Entrypoint override. None means the image's own CMD is used, which
    /// must be /bin/pipeline-runner. Only set this in tests.
    pub entrypoint: Option<Vec<String>>,

    /// Credentials for cloning the source repository.
    /// The referenced Secret must exist in the build namespace.
    pub git_credentials: GitCredentials,

    /// Credentials for pulling the pipeline image from the registry.
    /// The referenced Secret must be of type kubernetes.io/dockerconfigjson.
    pub registry_credentials: RegistryCredentials,

    /// Additional env vars injected into the pipeline container.
    /// Standard Cyclops vars (build ID, repo, commit, registry) are always
    /// injected by the manifest builder and should not be duplicated here.
    pub env: HashMap<String, String>,
}

impl BuildSpec {
    pub fn with_pipeline_runner(
        build_id: Uuid,
        source: SourceRef,
        pipeline_image: String,
        git_credentials: GitCredentials,
        registry_credentials: RegistryCredentials,
        command: Vec<String>,
    ) -> Self {
        Self {
            build_id,
            source,
            pipeline_image,
            // An empty command means "run the image's own entrypoint" —
            // analyzer images define their own, unlike CI pipeline images.
            entrypoint: (!command.is_empty()).then_some(command),
            git_credentials,
            registry_credentials,
            env: HashMap::new(),
        }
    }
}

/// Specification for a bootstrap job.
///
/// A bootstrap job is run when no pipeline image exists yet for a given repo.
/// It uses the a well known, configured build environment and runs a known command
/// and produce the repo-specific pipeline image, pushing it to the registry.
///
/// Bootstrap jobs use the same init container clone pattern as pipeline jobs —
/// the Nix evaluator needs the source.
///
/// Once the bootstrap job completes and pushes the image, the orchestrator
/// records the resulting digest against the repo URL so subsequent builds
/// skip bootstrapping entirely.
#[derive(Debug, Clone)]
pub struct BootstrapSpec {
    pub build_id: Uuid,
    pub repo_url: String,
    pub commit_sha: String,

    /// The well-known base Nix builder image. Pinned by digest in config.
    /// This image knows how to evaluate Dhall and run nix build. It is built
    /// manually and changes only when the Nix evaluation environment changes.
    pub bootstrap_image: String,

    /// Path within the repo to the Dhall config defining the pipeline container.
    /// Convention-based — defaults to "container.dhall" at repo root.
    pub container_config_ref: String,

    /// Where to push the resulting pipeline image once built.
    pub target_registry: String,

    /// Credentials for cloning the source repository.
    pub git_credentials: GitCredentials,

    /// Credentials for pushing the produced image to the registry.
    /// Must have push access to target_registry.
    pub registry_credentials: RegistryCredentials,
}

// ── Credential types ──────────────────────────────────────────────────────────

/// Describes how the init container should authenticate to the git host
/// when cloning the source repository.
///
/// These map to Kubernetes Secrets that must already exist in the build
/// namespace before any job runs. Provisioning those secrets is an
/// infrastructure concern, not an orchestrator concern — the orchestrator
/// only needs to know which secret to reference in the Job manifest.
///
/// SSH key support is intentionally absent here. It will be added once the
/// identity root agent can issue short-lived SSH credentials per job, which
/// removes the need to manage long-lived key secrets entirely.
#[derive(Debug, Clone)]
pub enum GitCredentials {
    /// HTTP basic auth or token auth (GitLab deploy token, GitHub PAT, etc.).
    /// The secret must contain a `username` key and a `password` key.
    /// For GitLab CI tokens the username is typically "gitlab-ci-token".
    HttpToken {
        /// Name of the Kubernetes Secret in the build namespace.
        secret_name: String,
    },
}

/// How the init container should authenticate to the container registry
/// when pulling the pipeline image or pushing bootstrap build output.
///
/// Azure ACR and other registries that support OCI token exchange are handled
/// here. The orchestrator injects these as imagePullSecrets on the pod spec
/// and as env vars for the bootstrap push step.
///
/// Long-term, ACR managed identity via workload identity federation replaces
/// this entirely — the pod gets an AAD token from the identity root agent and
/// exchanges it for an ACR refresh token at pull time. Until then, static
/// secrets it is.
#[derive(Debug, Clone)]
pub enum RegistryCredentials {
    /// A standard dockerconfigjson secret. Works for ACR, GCR, ECR, and
    /// any registry that speaks Docker Hub auth protocol.
    /// Must be of type kubernetes.io/dockerconfigjson.
    DockerConfigJson {
        /// Name of the Kubernetes Secret in the build namespace.
        secret_name: String,
    },
}

#[cfg(test)]
mod source_ref_tests {
    use super::*;

    fn git() -> SourceRef {
        SourceRef::Git {
            url: "https://github.com/cmu-sei/Polar.git".into(),
            commit_sha: "a".repeat(40),
        }
    }

    fn archive() -> SourceRef {
        SourceRef::RegistryArchive {
            purl: "pkg:cargo/serde@1.0.219".into(),
            download_url: "https://static.crates.io/crates/serde/serde-1.0.219.crate".into(),
            sha256: "b".repeat(64),
        }
    }

    fn env_value(env: &[(String, String)], key: &str) -> Option<String> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn valid_refs_pass() {
        assert_eq!(git().validate(), Ok(()));
        assert_eq!(archive().validate(), Ok(()));
    }

    #[test]
    fn git_rejects_short_or_uppercase_sha() {
        for sha in ["abc123".to_string(), "A".repeat(40)] {
            let r = SourceRef::Git {
                url: "https://example.com/r.git".into(),
                commit_sha: sha,
            };
            assert!(matches!(
                r.validate(),
                Err(SourceRefError::InvalidCommitSha(_))
            ));
        }
    }

    #[test]
    fn archive_rejects_plain_http() {
        let mut r = archive();
        if let SourceRef::RegistryArchive { download_url, .. } = &mut r {
            *download_url = "http://static.crates.io/crates/serde/serde-1.0.219.crate".into();
        }
        assert!(matches!(
            r.validate(),
            Err(SourceRefError::InsecureDownloadUrl(_))
        ));
    }

    #[test]
    fn archive_rejects_bad_digest_and_purl() {
        let mut r = archive();
        if let SourceRef::RegistryArchive { sha256, .. } = &mut r {
            *sha256 = "z".repeat(64);
        }
        assert!(matches!(
            r.validate(),
            Err(SourceRefError::InvalidSha256(_))
        ));

        let mut r = archive();
        if let SourceRef::RegistryArchive { purl, .. } = &mut r {
            *purl = "serde@1.0.219".into();
        }
        assert!(matches!(r.validate(), Err(SourceRefError::InvalidPurl(_))));
    }

    #[test]
    fn fetch_env_carries_kind_and_pin() {
        let env = git().fetch_env();
        assert_eq!(
            env_value(&env, source_env::SOURCE_KIND).as_deref(),
            Some("git")
        );
        assert_eq!(
            env_value(&env, source_env::COMMIT_SHA),
            Some("a".repeat(40))
        );

        let env = archive().fetch_env();
        assert_eq!(
            env_value(&env, source_env::SOURCE_KIND).as_deref(),
            Some("archive")
        );
        assert_eq!(
            env_value(&env, source_env::ARCHIVE_SHA256),
            Some("b".repeat(64))
        );
        assert_eq!(env_value(&env, source_env::REPO_URL), None);
    }

    fn request(source: SourceRef, pipeline_image: Option<&str>) -> BuildRequest {
        BuildRequest {
            build_id: Uuid::nil(),
            source,
            pipeline_image: pipeline_image.map(Into::into),
            requested_by: "test".into(),
            requested_at: Utc::now(),
            target_registry: String::new(),
            metadata: HashMap::new(),
            command: vec![],
        }
    }

    #[test]
    fn archive_request_requires_digest_pinned_image() {
        for image in [None, Some("reg/analyzer:latest")] {
            assert!(matches!(
                request(archive(), image).validate(),
                Err(BuildRequestError::UnpinnedAnalyzerImage(_))
            ));
        }
        assert_eq!(
            request(archive(), Some("reg/analyzer@sha256:abc")).validate(),
            Ok(())
        );
    }

    #[test]
    fn git_request_may_rely_on_repo_mapping() {
        assert_eq!(request(git(), None).validate(), Ok(()));
    }

    #[test]
    fn serde_round_trip_is_tagged() {
        let json = serde_json::to_value(archive()).unwrap();
        assert_eq!(json["kind"], "registry_archive");
        let back: SourceRef = serde_json::from_value(json).unwrap();
        assert_eq!(back, archive());
    }
}
