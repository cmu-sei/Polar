use std::collections::HashMap;

use crate::container::{
    ContainerSpec, create_container, inspect_container, log_stream, start_container,
    stop_and_remove_container,
};
use crate::error::PodmanError;
use crate::image::{ImagePullPolicy, ensure_image};
use async_trait::async_trait;
use bollard::Docker;
use bollard::query_parameters::ListContainersOptions;
use orchestrator_core::backend::{
    BackendJobHandle, BuildBackend, JobGraphIdentity, JobStatus, LogStream, SubmittedJob,
};
use orchestrator_core::error::BackendError;
use orchestrator_core::types::BuildSpec;
use serde::Deserialize;
use tracing::{info, instrument, warn};

/// Configuration for the Podman backend.
///
/// Loaded from the `[backend.podman]` section of `OrchestratorConfig`.
/// All fields have sane defaults for rootless local operation.
#[derive(Debug, Deserialize, Clone)]
pub struct PodmanConfig {
    /// Path to the Podman Unix socket.
    ///
    /// Rootless default: `/run/user/{uid}/podman/podman.sock`
    /// Rootful default:  `/run/podman/podman.sock`
    ///
    /// The socket must be owned and accessible by the user running the
    /// orchestrator process. For rootless operation, ensure the Podman
    /// socket service is enabled: `systemctl --user enable --now podman.socket`
    pub socket_path: String,

    /// Controls whether images are pulled from a registry before container
    /// creation. `IfNotPresent` is the correct default for enclave deployments
    /// where images are pre-positioned and registry connectivity may be absent
    /// during a partition event.
    pub image_pull_policy: ImagePullPolicy,
}

impl PodmanConfig {
    /// Construct config with the rootless user socket path for the current user.
    ///
    /// Panics if `$UID` is not set, which should never happen on a properly
    /// configured Linux system but will fail loudly in a container that strips
    /// environment variables. Prefer explicit config in production.
    pub fn rootless_default() -> Self {
        // `id -u` equivalent via environment. The $UID variable is set by bash
        // but not always by all shells — fall back to /proc/self if absent.
        let uid = std::env::var("UID")
            .or_else(|_| {
                std::fs::read_to_string("/proc/self/status")
                    .ok()
                    .and_then(|s| {
                        s.lines()
                            .find(|l| l.starts_with("Uid:"))
                            .and_then(|l| l.split_whitespace().nth(1).map(String::from))
                    })
                    .ok_or(std::env::VarError::NotPresent)
            })
            .unwrap_or_else(|_| "1000".to_string());

        Self {
            socket_path: format!("/run/user/{uid}/podman/podman.sock"),
            image_pull_policy: ImagePullPolicy::IfNotPresent,
        }
    }
}

/// Podman-based build backend.
///
/// Executes pipeline jobs as rootless Podman containers on the local host.
/// Communicates with the Podman daemon over a Unix socket using the
/// Docker-compatible API (bollard). No network path to any remote API server
/// is required for job execution — the only external dependency is the
/// Podman daemon itself, which runs as a local user service.
///
/// This is the correct backend for deployments where
/// partition tolerance during HQ outages is a hard requirement. The
/// Kubernetes backend's dependency on a remote API server makes it
/// unsuitable for that scenario.
///
/// `PodmanBackend` is `Send + Sync` and held behind `Arc<dyn BuildBackend>`
/// by the actor tree. The bollard `Docker` client is internally `Arc`-wrapped
/// and clone-cheap.
pub struct PodmanBackend {
    client: Docker,
    config: PodmanConfig,
}

impl PodmanBackend {
    /// Connect to the Podman daemon at the configured socket path.
    ///
    /// Returns an error if the socket is not present or the daemon does not
    /// respond. The orchestrator should treat a connection failure here as a
    /// configuration error, not a transient one — if the daemon is not running
    /// at startup, no builds can be submitted.
    pub fn connect(config: PodmanConfig) -> Result<Self, PodmanError> {
        // bollard connects to a custom socket path via the unix-socket feature.
        // `connect_with_unix` takes the socket path and a timeout in seconds.
        let client = Docker::connect_with_unix(
            &config.socket_path,
            120, // connection timeout in seconds
            bollard::API_DEFAULT_VERSION,
        )
        .map_err(PodmanError::SocketUnreachable)?;

        info!(socket = %config.socket_path, "connected to podman socket");

        Ok(Self { client, config })
    }

    /// Verify the daemon is reachable by issuing a ping.
    ///
    /// Call this after `connect` to confirm the socket exists and the daemon
    /// is responding before accepting any build requests.
    pub async fn ping(&self) -> Result<(), PodmanError> {
        self.client
            .ping()
            .await
            .map_err(PodmanError::SocketUnreachable)?;
        Ok(())
    }

    /// Enumerate all containers created by this backend that are not in a
    /// terminal state. Used during startup to identify candidates for
    /// `Unreconciled` reconciliation.
    ///
    /// Any container with the `cyclops.managed=true` label that is in a
    /// non-terminal state (running, created, paused, restarting) was left
    /// orphaned by a previous orchestrator process — either a crash or an
    /// unclean shutdown during a partition event.
    pub async fn list_managed_containers(&self) -> Result<Vec<(String, String)>, PodmanError> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec!["cyclops.managed=true".to_string()],
        );

        let containers = self
            .client
            .list_containers(Some(ListContainersOptions {
                all: true, // include stopped containers for reconciliation
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(PodmanError::SocketUnreachable)?;

        let result = containers
            .into_iter()
            .filter_map(|c| {
                let id = c.id?;
                let build_id = c.labels?.remove("cyclops.build_id")?;
                Some((build_id, id))
            })
            .collect();

        Ok(result)
    }
}

#[async_trait]
impl BuildBackend for PodmanBackend {
    /// Submit a pipeline job as a Podman container.
    ///
    /// The sequence is:
    ///   1. Ensure the pipeline image is available locally per pull policy.
    ///   2. Create the container (does not start it).
    ///   3. Start the container.
    ///   4. Return the handle and graph identity.
    ///
    /// Note on `BuildSpec` fields: `GitCredentials` and `RegistryCredentials`
    /// are Kubernetes Secret references and are meaningless to this backend.
    /// Git cloning is the pipeline runner's responsibility — the runner has
    /// access to `CYCLOPS_REPO_URL` and `CYCLOPS_COMMIT_SHA` via env and must
    /// handle its own credential acquisition. For the spike, public repos or
    /// repos accessible via ambient SSH agent credentials are assumed.
    /// Credential injection via SPIFFE SVID is the intended long-term solution.
    #[instrument(skip(self, spec), fields(build_id = %spec.build_id, image = %spec.pipeline_image))]
    async fn submit(&self, spec: &BuildSpec) -> Result<SubmittedJob, BackendError> {
        // Step 1: Ensure image is available.
        ensure_image(
            &self.client,
            &spec.pipeline_image,
            &self.config.image_pull_policy,
        )
        .await
        .map_err(BackendError::from)?;

        // Step 2: Create container (not yet running).
        let container_spec = ContainerSpec {
            build_id: &spec.build_id,
            image: &spec.pipeline_image,
            entrypoint: spec.entrypoint.clone(),
            env: &spec.env,
            repo_url: &spec.repo_url,
            commit_sha: &spec.commit_sha,
        };

        let (container_name, container_id) = create_container(&self.client, &container_spec)
            .await
            .map_err(BackendError::from)?;

        // Step 3: Start it.
        start_container(&self.client, &container_id)
            .await
            .map_err(BackendError::from)?;

        info!(
            build_id = %spec.build_id,
            container_id = %container_id,
            name = %container_name,
            "pipeline container started"
        );

        let handle = BackendJobHandle {
            name: container_name.clone(),
            uid: container_id.clone(),
        };

        // The graph identity for a Podman container. The container ID is the
        // stable, unique identifier — the name is human-readable but could
        // theoretically be reused if a container is removed and recreated.
        // Polar's graph processor should MERGE on container_id, not name.
        let graph_identity = JobGraphIdentity {
            node_label: "PodmanContainer".into(),
            display_name: container_name,
            identity_props: vec![
                ("container_id".into(), container_id),
                ("build_id".into(), spec.build_id.to_string()),
            ],
        };

        Ok(SubmittedJob {
            handle,
            graph_identity,
        })
    }

    /// Poll the current status of a submitted container.
    ///
    /// `handle.uid` is the container ID. If the container is not found (404),
    /// the daemon may have been restarted with pruning enabled, or the
    /// container was manually removed. Return `Unknown` so the actor can
    /// transition to `Unreconciled`.
    #[instrument(skip(self), fields(handle = %handle))]
    async fn poll(&self, handle: &BackendJobHandle) -> Result<JobStatus, BackendError> {
        // `handle.uid` is the container ID for this backend.
        match inspect_container(&self.client, &handle.uid).await {
            Ok(status) => Ok(status),
            Err(PodmanError::InspectFailed { .. }) => {
                // 404 or inspect error — container is gone. Return Unknown
                // so the actor transitions to Unreconciled rather than Failed,
                // since we cannot assert the job failed — it may have succeeded
                // before the container record was pruned.
                warn!(handle = %handle, "container not found during poll, returning Unknown");
                Ok(JobStatus::Unknown)
            }
            Err(e) => Err(BackendError::from(e)),
        }
    }

    /// Stop and remove the container.
    ///
    /// Best-effort. If the container is already stopped or not found, log and
    /// return Ok — the important thing is that the handle is invalidated from
    /// the orchestrator's perspective.
    #[instrument(skip(self), fields(handle = %handle))]
    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), BackendError> {
        match stop_and_remove_container(&self.client, &handle.uid).await {
            Ok(()) => Ok(()),
            Err(PodmanError::CancellationFailed {
                ref source,
                container_id,
            }) => {
                // If the container is already gone, treat as success.
                if let bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                } = source
                {
                    warn!(handle = %handle, "container already gone during cancel, treating as success");
                    Ok(())
                } else {
                    let err = format!(
                        "Failed to stop container {container_id} for job {}",
                        handle.name
                    );
                    tracing::error!(handle = %handle, "{err}");
                    Err(BackendError::CancellationFailed(err))
                }
            }
            Err(e) => Err(BackendError::from(e)),
        }
    }

    /// Stream logs from the container identified by `handle.uid`.
    #[instrument(skip(self), fields(handle = %handle))]
    async fn logs(&self, handle: &BackendJobHandle) -> Result<LogStream, BackendError> {
        log_stream(&self.client, &handle.uid)
            .await
            .map_err(BackendError::from)
    }

    fn name(&self) -> &'static str {
        "podman"
    }
}
