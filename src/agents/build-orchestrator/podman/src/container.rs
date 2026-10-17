use std::collections::HashMap;

use bollard::Docker;
use bollard::models::HostConfig;

use bollard::plugin::{ContainerCreateBody, ContainerStateStatusEnum};
use bollard::query_parameters::{CreateContainerOptionsBuilder, StopContainerOptionsBuilder};
use bollard::{
    container::LogOutput,
    query_parameters::{LogsOptions, RemoveContainerOptions},
};
use futures::StreamExt;
use orchestrator_core::backend::JobStatus;
use orchestrator_core::{
    WORKSPACE_PATH, tls_ca_cert_path, tls_client_cert_path, tls_client_key_path,
};
use tokio::io::AsyncRead;
use tracing::{debug, info, instrument};

use crate::error::PodmanError;

/// The label applied to every container this backend creates.
/// Used for reconciliation sweeps on startup: any container with this label
/// that is in a non-terminal state is a candidate for `Unreconciled` recovery.
pub const MANAGED_LABEL: &str = "cyclops.managed";
pub const MANAGED_LABEL_VALUE: &str = "true";

/// The label carrying the build UUID. Enables correlation between a container
/// and its `BuildRecord` without parsing the container name.
pub const BUILD_ID_LABEL: &str = "cyclops.build_id";

/// Parameters for creating a pipeline container.
///
/// Deliberately flat — this is not `BuildSpec`. By the time we construct
/// `ContainerSpec`, we have already resolved the image, extracted what the
/// Podman backend actually needs, and discarded the Kubernetes-specific fields
/// (Secret references, init container config, etc.) that are meaningless here.
pub struct ContainerSpec<'a> {
    /// Build UUID, used for naming and labelling.
    pub build_id: &'a uuid::Uuid,

    /// Fully qualified image reference. Must be present locally before calling
    /// `create_container` — call `ensure_image` first.
    pub image: &'a str,

    /// Entrypoint override. None uses the image's own CMD.
    pub entrypoint: Option<Vec<String>>,

    /// Environment variables injected into the pipeline container.
    /// Standard Cyclops vars (CYCLOPS_BUILD_ID, CYCLOPS_REPO_URL,
    /// CYCLOPS_COMMIT_SHA) are always injected by this function and must not
    /// be duplicated in this map.
    pub env: &'a HashMap<String, String>,

    /// Source repository URL. Injected as CYCLOPS_REPO_URL.
    pub repo_url: &'a str,

    /// Full commit SHA. Injected as CYCLOPS_COMMIT_SHA.
    pub commit_sha: &'a str,
}

/// Create a container from the given spec. Does not start it.
///
/// Returns the container ID (64-char hex string) which becomes the opaque
/// `BackendJobHandle.uid`. The human-readable name is derived from the build ID
/// and used as `BackendJobHandle.name`.
#[instrument(skip(client, spec), fields(build_id = %spec.build_id, image = %spec.image))]
pub async fn create_container(
    client: &Docker,
    spec: &ContainerSpec<'_>,
) -> Result<(String, String), PodmanError> {
    let container_name = container_name_for_build(spec.build_id);

    // Build the env vec. Cyclops-standard vars first, then caller-provided.
    // Caller-provided vars cannot override the standard ones — if they try,
    // the standard ones win because we insert them last via extend.
    let mut env_map = spec.env.clone();
    env_map.insert("POLAR_BUILD_ID".into(), spec.build_id.to_string());
    env_map.insert("POLAR_REPO_URL".into(), spec.repo_url.to_string());
    env_map.insert("BUILD_COMMIT_SHA".into(), spec.commit_sha.to_string());
    // Insert paths for cassini client certificates
    env_map.insert("TLS_CA_CERT".into(), tls_ca_cert_path());
    env_map.insert("TLS_CLIENT_CERT".into(), tls_client_cert_path());
    env_map.insert("TLS_CLIENT_KEY".into(), tls_client_key_path());
    // TODO: Get this value from...somewhere else
    env_map.insert("CASSINI_SERVER_NAME".into(), "cassini".to_string());

    env_map.insert("BUILD_WORKSPACE".into(), WORKSPACE_PATH.to_string());

    let env_vec: Vec<String> = env_map.iter().map(|(k, v)| format!("{k}={v}")).collect();

    let build_id = spec.build_id.to_string();

    let labels: HashMap<&str, &str> = [
        (MANAGED_LABEL, MANAGED_LABEL_VALUE),
        (BUILD_ID_LABEL, &build_id),
    ]
    .into_iter()
    .collect();

    // Labels need owned strings for the bollard API.
    let labels_owned: HashMap<String, String> = labels
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let config = ContainerCreateBody {
        image: Some(spec.image.to_string()),
        cmd: None, // pipeline-runner is the image entrypoint; CMD is unused
        entrypoint: spec.entrypoint.clone(),
        env: Some(env_vec),
        labels: Some(labels_owned),
        host_config: Some(HostConfig {
            // Network mode: none by default for pipeline containers.
            // The pipeline runner operates on a cloned workspace; it should
            // not need outbound network access. This is a security default —
            // individual repo configs can override via env if genuinely needed,
            // but that is a future concern requiring policy enforcement.
            //
            // For the spike this is left as the default (bridge) to avoid
            // breaking local testing setups where pulling dependencies inside
            // the container is expected. Set to "none" before production.
            network_mode: Some("bridge".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    // TODO: We're leaving it up to bollard to figure out the host's platform and pull an image that matches it
    // We should probably be explicit though
    let options = CreateContainerOptionsBuilder::new()
        .name(&container_name)
        .build();

    let response = client
        .create_container(Some(options), config)
        .await
        .map_err(|e| PodmanError::ContainerCreateFailed(e.to_string()))?;

    let container_id = response.id;

    if !response.warnings.is_empty() {
        for warning in &response.warnings {
            tracing::warn!(container_id = %container_id, warning = %warning, "podman warning on container create");
        }
    }

    debug!(
        container_id = %container_id,
        name = %container_name,
        "container created"
    );

    Ok((container_name, container_id))
}

/// Start a previously created container by ID.
#[instrument(skip(client), fields(container_id = %container_id))]
pub async fn start_container(client: &Docker, container_id: &str) -> Result<(), PodmanError> {
    client
        .start_container(container_id, None)
        .await
        .map_err(|e| PodmanError::ContainerStartFailed {
            container_id: container_id.to_string(),
            reason: e.to_string(),
        })?;

    info!("container started");
    Ok(())
}

/// Inspect a container and map its state to `JobStatus`.
///
/// Podman retains stopped container records until explicitly removed, so this
/// will return a terminal status even after the process has exited — which is
/// exactly the property we rely on for `Unreconciled` reconciliation.
#[instrument(skip(client), fields(container_id = %container_id))]
pub async fn inspect_container(
    client: &Docker,
    container_id: &str,
) -> Result<JobStatus, PodmanError> {
    let info = client
        .inspect_container(container_id, None)
        .await
        .map_err(|e| match e {
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            } => PodmanError::InspectFailed {
                container_id: container_id.to_string(),
                reason: e.to_string(),
            },
            other => PodmanError::InspectFailed {
                container_id: container_id.to_string(),
                reason: other.to_string(),
            },
        })?;

    let state = info.state.ok_or_else(|| {
        PodmanError::UnexpectedResponse(format!(
            "container {container_id} inspect returned no state"
        ))
    })?;

    // Podman/Docker container states: created, running, paused, restarting,
    // removing, exited, dead. We map these to our `JobStatus` model.
    //
    // "exited" is the key terminal state. Exit code 0 = Succeeded, non-zero = Failed.
    // "dead" is a hard failure state — the container could not be stopped cleanly.
    let status = match state.status {
        Some(ContainerStateStatusEnum::RUNNING) | Some(ContainerStateStatusEnum::RESTARTING) => {
            JobStatus::Running
        }
        Some(ContainerStateStatusEnum::CREATED) | Some(ContainerStateStatusEnum::PAUSED) => {
            JobStatus::Pending
        }
        Some(ContainerStateStatusEnum::EXITED) => {
            let exit_code = state.exit_code.unwrap_or(-1);
            if exit_code == 0 {
                JobStatus::Succeeded
            } else {
                JobStatus::Failed {
                    reason: Some(format!("exited with code {exit_code}")),
                }
            }
        }
        Some(ContainerStateStatusEnum::DEAD) => JobStatus::Failed {
            reason: Some("container entered dead state".to_string()),
        },
        Some(ContainerStateStatusEnum::REMOVING) => {
            // Removing means a cancel was issued and is in progress.
            // Return Failed rather than Pending so the actor does not
            // keep polling a container that is being torn down.
            JobStatus::Failed {
                reason: Some("container is being removed".to_string()),
            }
        }
        // EMPTY is bollard's zero value for an unset discriminant — treat it
        // the same as None. Both indicate the daemon returned a state struct
        // with no meaningful status, which should not happen for a container
        // we just inspected. Return Unknown so the actor transitions to
        // Unreconciled rather than spinning on a permanently ambiguous state.
        Some(ContainerStateStatusEnum::EMPTY) | None => JobStatus::Unknown,
    };

    debug!(container_id = %container_id, ?status, "container inspected");
    Ok(status)
}

/// Stop and remove a container. Best-effort — errors are logged but not fatal.
///
/// Podman's stop sends SIGTERM, waits `timeout_secs`, then sends SIGKILL.
/// The subsequent remove cleans up the container record from the daemon.
/// If remove is not called, the stopped container lingers and will appear
/// in reconciliation sweeps — harmless but noisy.
#[instrument(skip(client), fields(container_id = %container_id))]
pub async fn stop_and_remove_container(
    client: &Docker,
    container_id: &str,
) -> Result<(), PodmanError> {
    // Stop with a 10 second grace period before SIGKILL.
    let options = StopContainerOptionsBuilder::new().t(10).build();
    client
        .stop_container(container_id, Some(options))
        .await
        .map_err(|e| PodmanError::CancellationFailed {
            container_id: container_id.to_string(),
            reason: e.to_string(),
        })?;

    // Remove the stopped container to clean up the daemon's record.
    client
        .remove_container(
            container_id,
            Some(RemoveContainerOptions {
                force: false,
                v: false, // do not remove anonymous volumes
                link: false,
            }),
        )
        .await
        .map_err(|e| PodmanError::CancellationFailed {
            container_id: container_id.to_string(),
            reason: e.to_string(),
        })?;

    info!("container stopped and removed");
    Ok(())
}

/// Open a log stream for a running or recently exited container.
///
/// Returns a boxed `AsyncRead` that the caller can forward to `StorageClient`.
/// The stream follows the container — it will not terminate until the container
/// exits (or is stopped). Callers should run log streaming concurrently with
/// the poll loop rather than awaiting stream EOF before polling.
#[instrument(skip(client), fields(container_id = %container_id))]
pub async fn log_stream(
    client: &Docker,
    container_id: &str,
) -> Result<Box<dyn AsyncRead + Send + Unpin>, PodmanError> {
    let options = LogsOptions {
        follow: true,
        stdout: true,
        stderr: true,
        timestamps: true,
        ..Default::default()
    };

    // bollard's log stream yields `LogOutput` frames (stdout/stderr tagged).
    // We flatten these into a raw byte stream suitable for `AsyncRead`.
    // The framing information (stdout vs stderr) is lost here — if the actor
    // needs separate streams in the future, this is the place to split them.
    let stream = client.logs(container_id, Some(options));

    // Adapt the bollard stream into an `AsyncRead` by collecting frames
    // through an in-memory pipe. This is not zero-copy, but it keeps the
    // log stream type compatible with `LogStream = Box<dyn AsyncRead + ...>`.
    //
    // For the spike this is acceptable. A production implementation should
    // use `tokio::io::duplex` or a custom `Stream`-to-`AsyncRead` adaptor
    // to avoid the intermediate allocation per frame.
    let (writer, reader) = tokio::io::duplex(64 * 1024);

    tokio::spawn(async move {
        let mut stream = stream;
        let mut writer = writer;
        use tokio::io::AsyncWriteExt;

        while let Some(result) = stream.next().await {
            match result {
                Ok(LogOutput::StdOut { message } | LogOutput::StdErr { message }) => {
                    if writer.write_all(&message).await.is_err() {
                        // Reader dropped — consumer is no longer interested.
                        break;
                    }
                }
                Ok(_) => {} // Console or tty frames — not applicable here
                Err(e) => {
                    tracing::warn!(error = %e, "log stream error");
                    break;
                }
            }
        }
        // Writer drop closes the pipe, signalling EOF to the reader.
    });

    Ok(Box::new(reader))
}

/// Derive a deterministic, human-readable container name from a build UUID.
///
/// Podman container names must be unique within the daemon. Using the build ID
/// ensures idempotency: if the orchestrator crashes after creating the container
/// but before recording the handle, a restart can find the existing container
/// by name rather than creating a duplicate.
pub fn container_name_for_build(build_id: &uuid::Uuid) -> String {
    format!("cyclops-build-{build_id}")
}
