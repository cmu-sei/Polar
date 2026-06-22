use std::collections::HashMap;

use crate::container::{inspect_container, log_stream};
use crate::error::PodmanError;
use crate::image::{ImagePullPolicy, ensure_image};
use crate::pod::{CiPodHandle, CiPodSpec, create_ci_pod, remove_ci_pod};
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
        // Step 1: Ensure the pipeline image is available locally.
        // The cloner image (alpine/git) should also be ensured here if you
        // cannot guarantee it is pre-positioned.
        ensure_image(
            &self.client,
            &spec.pipeline_image,
            &self.config.image_pull_policy,
        )
        .await
        .map_err(BackendError::from)?;

        // Step 2: Map BuildSpec to CiPodSpec.
        //
        // GitCredentials is left as None here — for the spike we assume public
        // repos or ambient SSH agent credentials available to the cloner image.
        // Wire `spec.git_credentials` through once you have a concrete type.
        let pod_spec = CiPodSpec {
            build_id: &spec.build_id,
            pipeline_image: &spec.pipeline_image,
            repo_url: &spec.repo_url,
            commit_sha: &spec.commit_sha,
            entrypoint: spec.entrypoint.clone(),
            env: &spec.env,
            git_credentials: None, // TODO: wire through from spec
        };

        // Step 3: Create pod, run init container, start pipeline container.
        // This call blocks until the git clone completes (or fails).
        let handle: CiPodHandle = create_ci_pod(&self.config.socket_path, &pod_spec)
            .await
            .map_err(BackendError::from)?;

        info!(
            build_id = %spec.build_id,
            pod_id = %handle.pod_id,
            pipeline_container_id = %handle.pipeline_container_id,
            "pipeline pod started"
        );

        // `BackendJobHandle.uid` is the pipeline container ID. poll() and
        // logs() both target this via bollard inspect/logs, which are
        // Docker-compat and work the same as before.
        //
        // We also stash the pod_name in `handle.name` so cancel() can
        // reconstruct the pod name for pod-level teardown. The pod name is
        // deterministically derived from build_id so this is redundant, but
        // being explicit avoids a hidden coupling in cancel().
        let backend_handle = BackendJobHandle {
            name: handle.pod_name.clone(),
            uid: handle.pipeline_container_id.clone(),
        };

        let graph_identity = JobGraphIdentity {
            node_label: "PodmanPod".into(),
            display_name: handle.pod_name.clone(),
            identity_props: vec![
                ("pod_id".into(), handle.pod_id.clone()),
                (
                    "pipeline_container_id".into(),
                    handle.pipeline_container_id.clone(),
                ),
                ("build_id".into(), spec.build_id.to_string()),
            ],
        };

        Ok(SubmittedJob {
            handle: backend_handle,
            graph_identity,
        })
    }

    /// Stop and remove the container.
    ///
    /// Best-effort. If the container is already stopped or not found, log and
    /// return Ok — the important thing is that the handle is invalidated from
    /// the orchestrator's perspective.
    #[instrument(skip(self), fields(handle = %handle))]
    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), BackendError> {
        // handle.name is the pod name (set in submit above).
        // Reconstruct the CiPodHandle minimally — we only need pod_name and
        // workspace_volume_name for remove_ci_pod.
        //
        // workspace_volume_name is deterministically `cyclops-ws-{build_id}`.
        // Extract the build_id suffix from the pod name `cyclops-build-{uuid}`.
        let build_id_str = handle
            .name
            .strip_prefix("cyclops-build-")
            .unwrap_or(&handle.uid); // fallback: should never happen

        let pod_handle = CiPodHandle {
            pod_id: String::new(), // not needed for removal
            pod_name: handle.name.clone(),
            pipeline_container_id: handle.uid.clone(),
            init_container_id: String::new(), // not needed for removal
            workspace_volume_name: format!("cyclops-ws-{build_id_str}"),
        };

        match remove_ci_pod(&self.config.socket_path, &pod_handle).await {
            Ok(()) => Ok(()),
            Err(PodmanError::UnexpectedResponse(ref msg)) if msg.contains("404") => {
                warn!(handle = %handle, "pod already gone during cancel, treating as success");
                Ok(())
            }
            Err(e) => {
                let err = format!(
                    "Failed to remove pod {} for job {}",
                    handle.name, handle.uid
                );
                tracing::error!(handle = %handle, error = %e, "{err}");
                Err(BackendError::CancellationFailed(err))
            }
        }
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
