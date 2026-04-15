use std::collections::HashMap;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, StatusCode};
use hyper_util::client::legacy::Client;
use hyperlocal::{UnixClientExt, Uri as UnixUri};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use crate::error::PodmanError;

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

pub const MANAGED_LABEL: &str = "cyclops.managed";
pub const MANAGED_LABEL_VALUE: &str = "true";
pub const BUILD_ID_LABEL: &str = "cyclops.build_id";
pub const WORKSPACE_MOUNT: &str = "/workspace";
pub const GIT_CLONER_IMAGE: &str = "cyclops/git-clone:latest";

const INIT_POLL_INTERVAL: Duration = Duration::from_millis(500);
const INIT_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// PodmanNativeClient
//
// Four HTTP primitives. Every libpod operation is built on top of these —
// no bollard involvement anywhere in this file.
// ---------------------------------------------------------------------------

struct PodmanNativeClient {
    socket_path: String,
    inner: Client<hyperlocal::UnixConnector, Full<Bytes>>,
}

impl PodmanNativeClient {
    fn new(socket_path: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.into(),
            inner: Client::unix(),
        }
    }

    async fn post_json<T: Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<(StatusCode, Bytes), PodmanError> {
        let json =
            serde_json::to_vec(body).map_err(|e| PodmanError::UnexpectedResponse(e.to_string()))?;
        self.request(Method::POST, path, Some(json), Some("application/json"))
            .await
    }

    async fn post_empty(&self, path: &str) -> Result<(StatusCode, Bytes), PodmanError> {
        self.request(Method::POST, path, None, None).await
    }

    async fn get(&self, path: &str) -> Result<(StatusCode, Bytes), PodmanError> {
        self.request(Method::GET, path, None, None).await
    }

    async fn delete(&self, path: &str) -> Result<(StatusCode, Bytes), PodmanError> {
        self.request(Method::DELETE, path, None, None).await
    }

    /// Single transport implementation. All four HTTP methods funnel here.
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: Option<&str>,
    ) -> Result<(StatusCode, Bytes), PodmanError> {
        let url = UnixUri::new(&self.socket_path, path);
        let mut builder = Request::builder().method(method).uri(url);
        if let Some(ct) = content_type {
            builder = builder.header("Content-Type", ct);
        }
        let req = builder
            .body(Full::from(body.map(Bytes::from).unwrap_or_else(Bytes::new)))
            .map_err(|e| PodmanError::UnexpectedResponse(e.to_string()))?;
        let resp = self
            .inner
            .request(req)
            .await
            .map_err(|e| PodmanError::UnexpectedResponse(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| PodmanError::UnexpectedResponse(e.to_string()))?
            .to_bytes();
        Ok((status, bytes))
    }
}

// ---------------------------------------------------------------------------
// libpod REST types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct PodCreateRequest {
    name: String,
    volumes: Vec<PodVolume>,
    labels: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
struct PodVolume {
    name: String,
    dest: String,
}

#[derive(Debug, Deserialize)]
struct PodCreateResponse {
    #[serde(rename = "Id")]
    id: String,
}

/// libpod container create spec (subset).
///
/// The `pod` field is the critical difference from the Docker-compat API.
/// It is a first-class field in the libpod spec that correctly attaches the
/// container to the pod's network, IPC, and UTS namespaces and makes the
/// pod's volumes available. There is no equivalent in the Docker-compat layer,
/// which is why we are not using bollard for container creation here.
#[derive(Debug, Serialize)]
struct ContainerCreateRequest {
    name: String,
    image: String,
    pod: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoint: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    /// libpod takes env as a map, not the Docker-compat KEY=VAL vec.
    #[serde(skip_serializing_if = "Option::is_none")]
    env: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ContainerCreateErrorResponse {
    #[serde(default)]
    cause: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    response: i64,
}

#[derive(Debug, Deserialize)]
struct ContainerCreateResponse {
    #[serde(rename(deserialize = "Id"))]
    id: String,

    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ContainerInspectResponse {
    #[serde(rename = "State")]
    state: ContainerState,
}

#[derive(Debug, Deserialize)]
struct ContainerState {
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "ExitCode")]
    exit_code: Option<i64>,
}

// ---------------------------------------------------------------------------
// libpod operations — one method per API call, all reusing the four primitives
// ---------------------------------------------------------------------------

impl PodmanNativeClient {
    async fn create_pod(&self, req: &PodCreateRequest) -> Result<String, PodmanError> {
        let (status, body) = self.post_json("/v4.0.0/libpod/pods/create", req).await?;
        require_success(status, &body, "pod create")?;
        Ok(deserialize::<PodCreateResponse>(&body, "pod create")?.id)
    }

    async fn create_container(&self, req: &ContainerCreateRequest) -> Result<String, PodmanError> {
        let (status, body) = self
            .post_json("/v4.0.0/libpod/containers/create", req)
            .await?;
        require_success(status, &body, "container create")?;

        match status {
            StatusCode::CREATED => {
                let resp = deserialize::<ContainerCreateResponse>(&body, "container create")?;

                for w in &resp.warnings {
                    warn!(warning = %w, container = %resp.id, "podman container create warning");
                }
                Ok(resp.id)
            }
            StatusCode::NOT_FOUND
            | StatusCode::BAD_REQUEST
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::CONFLICT => {
                let resp = deserialize::<ContainerCreateErrorResponse>(&body, "container create")?;
                let err = serde_json::to_string_pretty(&resp).unwrap();
                Err(PodmanError::ContainerCreateFailed(err))
            }
            _ => {
                warn!("Unexpected response from server!");
                let resp = deserialize::<ContainerCreateErrorResponse>(&body, "container create")?;
                let err = serde_json::to_string_pretty(&resp).unwrap();
                Err(PodmanError::ContainerCreateFailed(err))
            }
        }
    }

    async fn start_container(&self, container_id: &str) -> Result<(), PodmanError> {
        let (status, body) = self
            .post_empty(&format!("/v4.0.0/libpod/containers/{container_id}/start"))
            .await?;
        // 304 = already started, not an error
        if !status.is_success() && status.as_u16() != 304 {
            return Err(PodmanError::UnexpectedResponse(format!(
                "container start failed HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }
        Ok(())
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> Result<ContainerInspectResponse, PodmanError> {
        let (status, body) = self
            .get(&format!("/v4.0.0/libpod/containers/{container_id}/json"))
            .await?;
        if status.as_u16() == 404 {
            return Err(PodmanError::InspectFailed {
                container_id: container_id.to_string(),
                reason: "container not found (404)".to_string(),
            });
        }
        require_success(status, &body, "container inspect")?;
        deserialize(&body, "container inspect")
    }

    async fn stop_pod(&self, pod_name: &str) -> Result<(), PodmanError> {
        let (status, body) = self
            .post_empty(&format!("/v4.0.0/libpod/pods/{pod_name}/stop"))
            .await?;
        if !status.is_success() && status.as_u16() != 304 {
            warn!(
                pod_name,
                http_status = %status,
                body = %String::from_utf8_lossy(&body),
                "pod stop returned unexpected status"
            );
        }
        Ok(())
    }

    async fn remove_pod(&self, pod_name: &str) -> Result<(), PodmanError> {
        let (status, body) = self
            .delete(&format!("/v4.0.0/libpod/pods/{pod_name}?force=true"))
            .await?;
        if !status.is_success() && status.as_u16() != 404 {
            return Err(PodmanError::UnexpectedResponse(format!(
                "pod remove failed HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }
        Ok(())
    }

    async fn remove_volume(&self, volume_name: &str) -> Result<(), PodmanError> {
        let (status, body) = self
            .delete(&format!("/v4.0.0/libpod/volumes/{volume_name}"))
            .await?;
        if !status.is_success() && status.as_u16() != 404 {
            return Err(PodmanError::UnexpectedResponse(format!(
                "volume remove failed HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn require_success(status: StatusCode, body: &Bytes, op: &str) -> Result<(), PodmanError> {
    if !status.is_success() {
        return Err(PodmanError::UnexpectedResponse(format!(
            "{op} failed HTTP {status}: {}",
            String::from_utf8_lossy(body)
        )));
    }
    Ok(())
}

fn deserialize<'a, T: Deserialize<'a>>(body: &'a Bytes, op: &str) -> Result<T, PodmanError> {
    serde_json::from_slice(body)
        .map_err(|e| PodmanError::UnexpectedResponse(format!("{op} deserialize: {e}")))
}

fn build_labels(build_id: &str) -> HashMap<String, String> {
    [
        (MANAGED_LABEL.to_string(), MANAGED_LABEL_VALUE.to_string()),
        (BUILD_ID_LABEL.to_string(), build_id.to_string()),
    ]
    .into_iter()
    .collect()
}

fn build_cloner_env(spec: &CiPodSpec<'_>) -> HashMap<String, String> {
    let mut env = HashMap::from([
        ("CYCLOPS_REPO_URL".to_string(), spec.repo_url.to_string()),
        (
            "CYCLOPS_COMMIT_SHA".to_string(),
            spec.commit_sha.to_string(),
        ),
        ("CYCLOPS_WORKSPACE".to_string(), WORKSPACE_MOUNT.to_string()),
        ("GIT_USERNAME".to_string(), "username".to_string()),
        ("GIT_PASSWORD".to_string(), "somepassword".to_string()),
    ]);
    // if let Some(creds) = &spec.git_credentials {
    //     match creds {
    //         GitCredentials::UsernamePassword { username, password } => {
    //             env.insert("GIT_USERNAME".to_string(), username.clone());
    //             env.insert("GIT_PASSWORD".to_string(), password.clone());
    //         }
    //         GitCredentials::SshKeyPath(path) => {
    //             env.insert(
    //                 "GIT_SSH_COMMAND".to_string(),
    //                 format!("ssh -i {path} -o StrictHostKeyChecking=no"),
    //             );
    //         }
    //     }
    // }
    env
}

/// Poll until the container reaches a terminal state, returning the exit code.
///
/// Uses the libpod inspect endpoint directly — no bollard. The production
/// improvement here is to use POST /containers/{id}/wait instead of polling,
/// which blocks server-side until the container exits and avoids the latency
/// of up to INIT_POLL_INTERVAL after clone completion.
async fn wait_for_container_exit(
    native: &PodmanNativeClient,
    container_id: &str,
) -> Result<i64, PodmanError> {
    let deadline = tokio::time::Instant::now() + INIT_TIMEOUT;
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(PodmanError::UnexpectedResponse(format!(
                "init container {container_id} did not exit within {INIT_TIMEOUT:?}"
            )));
        }
        let info = native.inspect_container(container_id).await?;
        match info.state.status.as_str() {
            "exited" | "dead" => {
                let code = info.state.exit_code.unwrap_or(-1);
                debug!(container_id, exit_code = code, "init container exited");
                return Ok(code);
            }
            "running" | "created" | "restarting" => {
                sleep(INIT_POLL_INTERVAL).await;
            }
            other => {
                return Err(PodmanError::UnexpectedResponse(format!(
                    "init container entered unexpected state: {other}"
                )));
            }
        }
    }
}

fn map_container_status(state: &ContainerState) -> orchestrator_core::backend::JobStatus {
    use orchestrator_core::backend::JobStatus;
    match state.status.as_str() {
        "running" | "restarting" => JobStatus::Running,
        "created" | "paused" => JobStatus::Pending,
        "exited" => {
            let code = state.exit_code.unwrap_or(-1);
            if code == 0 {
                JobStatus::Succeeded
            } else {
                JobStatus::Failed {
                    reason: Some(format!("exited with code {code}")),
                }
            }
        }
        "dead" => JobStatus::Failed {
            reason: Some("container entered dead state".to_string()),
        },
        "removing" => JobStatus::Failed {
            reason: Some("container is being removed".to_string()),
        },
        _ => JobStatus::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub enum GitCredentials {
    UsernamePassword { username: String, password: String },
    SshKeyPath(String),
}

pub struct CiPodSpec<'a> {
    pub build_id: &'a Uuid,
    pub pipeline_image: &'a str,
    pub repo_url: &'a str,
    pub commit_sha: &'a str,
    pub entrypoint: Option<Vec<String>>,
    pub env: &'a HashMap<String, String>,
    pub git_credentials: Option<GitCredentials>,
}

#[derive(Debug)]
pub struct CiPodHandle {
    pub pod_id: String,
    pub pod_name: String,
    pub pipeline_container_id: String,
    pub init_container_id: String,
    pub workspace_volume_name: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a pod, run the git-cloner init container to completion, then start
/// the pipeline container. Pure libpod REST — bollard is not used.
///
/// The `bollard_client: &Docker` parameter has been removed from the signature
/// vs the previous iteration. Update the call site in backend.rs accordingly:
/// `create_ci_pod(&self.config.socket_path, &pod_spec).await`
#[instrument(skip(spec), fields(build_id = %spec.build_id))]
pub async fn create_ci_pod(
    socket_path: &str,
    spec: &CiPodSpec<'_>,
) -> Result<CiPodHandle, PodmanError> {
    let native = PodmanNativeClient::new(socket_path);
    let build_id_str = spec.build_id.to_string();
    let pod_name = format!("cyclops-build-{}", spec.build_id);
    let volume_name = format!("cyclops-ws-{}", spec.build_id);

    // Step 1: Pod with shared workspace volume.
    let pod_id = native
        .create_pod(&PodCreateRequest {
            name: pod_name.clone(),
            volumes: vec![PodVolume {
                name: volume_name.clone(),
                dest: WORKSPACE_MOUNT.to_string(),
            }],
            labels: build_labels(&build_id_str),
        })
        .await?;
    info!(pod_id = %pod_id, pod_name = %pod_name, "pod created");

    // Step 2: Init container. The `pod` field does the right thing.
    let init_container_id = native
        .create_container(&ContainerCreateRequest {
            name: format!("cyclops-init-{}", spec.build_id),
            image: GIT_CLONER_IMAGE.to_string(),
            pod: pod_name.clone(),
            entrypoint: None, // use the image's own entrypoint
            command: None,
            env: Some(build_cloner_env(spec)),
            labels: Some(build_labels(&build_id_str)),
            user: Some("0:0".to_string()),
        })
        .await?;
    info!(init_container_id = %init_container_id, "init container created");

    // Step 3: Start and wait.
    native.start_container(&init_container_id).await?;
    info!(init_container_id = %init_container_id, "init container started, waiting for clone");

    let init_exit_code = wait_for_container_exit(&native, &init_container_id).await?;

    // Step 4: Abort on clone failure, clean up the pod before returning.
    if init_exit_code != 0 {
        warn!(
            init_container_id = %init_container_id,
            exit_code = init_exit_code,
            "git clone failed, tearing down pod"
        );
        let _ = native.stop_pod(&pod_name).await;
        let _ = native.remove_pod(&pod_name).await;
        let _ = native.remove_volume(&volume_name).await;
        return Err(PodmanError::InitContainerFailed {
            container_id: init_container_id,
            exit_code: init_exit_code,
        });
    }
    info!(init_container_id = %init_container_id, "git clone succeeded");

    // Step 5: Pipeline container.
    let mut pipeline_env = spec.env.clone();
    pipeline_env.insert("POLAR_BUILD_ID".into(), build_id_str.clone());
    pipeline_env.insert("CI_COMMIT_SHORT_SHA".into(), spec.commit_sha.to_string());
    pipeline_env.insert("BUILD_WORKSPACE".into(), WORKSPACE_MOUNT.to_string());

    let pipeline_container_id = native
        .create_container(&ContainerCreateRequest {
            name: format!("cyclops-pipeline-{}", spec.build_id),
            image: spec.pipeline_image.to_string(),
            pod: pod_name.clone(),
            entrypoint: spec.entrypoint.clone(),
            command: None,
            env: Some(pipeline_env),
            labels: Some(build_labels(&build_id_str)),
            user: None,
        })
        .await?;

    native.start_container(&pipeline_container_id).await?;
    info!(
        pipeline_container_id = %pipeline_container_id,
        pod_id = %pod_id,
        "pipeline container started"
    );

    Ok(CiPodHandle {
        pod_id,
        pod_name,
        pipeline_container_id,
        init_container_id,
        workspace_volume_name: volume_name,
    })
}

/// Stop the pod, remove it, and prune the workspace volume.
#[instrument(fields(pod_name = %handle.pod_name))]
pub async fn remove_ci_pod(socket_path: &str, handle: &CiPodHandle) -> Result<(), PodmanError> {
    let native = PodmanNativeClient::new(socket_path);
    native.stop_pod(&handle.pod_name).await?;
    native.remove_pod(&handle.pod_name).await?;
    native.remove_volume(&handle.workspace_volume_name).await?;
    info!(pod_name = %handle.pod_name, "pod and workspace volume removed");
    Ok(())
}

/// Poll the pipeline container status. Called from PodmanBackend::poll().
///
/// Replaces the old bollard inspect_container call. The handle.uid is still
/// the pipeline container ID — that contract is unchanged.
pub async fn poll_pipeline_container(
    socket_path: &str,
    container_id: &str,
) -> Result<orchestrator_core::backend::JobStatus, PodmanError> {
    let native = PodmanNativeClient::new(socket_path);
    match native.inspect_container(container_id).await {
        Ok(info) => Ok(map_container_status(&info.state)),
        Err(PodmanError::InspectFailed { .. }) => {
            warn!(
                container_id,
                "container not found during poll, returning Unknown"
            );
            Ok(orchestrator_core::backend::JobStatus::Unknown)
        }
        Err(e) => Err(e),
    }
}
