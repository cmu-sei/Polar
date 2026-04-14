use crate::error::PodmanError;
use bollard::Docker;
use bollard::query_parameters::CreateImageOptions;
use futures::StreamExt;
use serde::Deserialize;
use tracing::{debug, info};

/// Whether to attempt a registry pull for a given image reference.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub enum ImagePullPolicy {
    /// Always pull, even if the image is present locally. Ensures freshness
    /// but requires registry connectivity — avoid this in enclave-local deployments
    /// where the registry may be unreachable during a partition.
    Always,

    /// Pull only if the image is not already present in the local store.
    /// This is the correct default: images
    /// are pre-positioned before a potential partition and the daemon uses its
    /// local cache without requiring outbound registry access.
    IfNotPresent,

    /// Never pull. If the image is absent, submission fails immediately with
    /// a clear error. Useful in fully air-gapped environments where pulling
    /// would always fail and a missing image is always a configuration error.
    Never,
}

impl std::fmt::Display for ImagePullPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImagePullPolicy::Always => write!(f, "Always"),
            ImagePullPolicy::IfNotPresent => write!(f, "IfNotPresent"),
            ImagePullPolicy::Never => write!(f, "Never"),
        }
    }
}

/// Returns true if the image reference is present in the local Podman image store.
///
/// This is a best-effort check — it queries the local daemon only. An image
/// present in a remote registry but not locally cached will return false.
#[tracing::instrument(skip(client), fields(image = %image_ref))]
pub async fn is_image_present(client: &Docker, image_ref: &str) -> Result<bool, PodmanError> {
    match client.inspect_image(image_ref).await {
        Ok(_) => {
            debug!("image present in local store");
            Ok(true)
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            debug!("image not found in local store");
            Ok(false)
        }
        Err(e) => {
            // Any error other than 404 indicates the daemon itself had a
            // problem responding — treat as socket unreachable.
            Err(PodmanError::SocketUnreachable(e))
        }
    }
}

/// Pull an image from the registry, consuming the progress stream to completion.
///
/// Podman's pull API returns a stream of status events — we must consume it
/// fully for the pull to complete. Individual stream errors are logged as
/// warnings; a final stream termination without a fatal error is considered
/// success. If the stream itself fails to open, that is a hard error.
///
/// No auth is wired here for the spike. Registry auth will be handled in a
/// follow-on once the SPIFFE Workload API integration is in place and we can
/// issue short-lived registry credentials from workload identity.
#[tracing::instrument(skip(client), fields(image = %image_ref))]
pub async fn pull_image(client: &Docker, image_ref: &str) -> Result<(), PodmanError> {
    info!("pulling image");

    let options = CreateImageOptions {
        from_image: Some(image_ref.to_string()),
        ..Default::default()
    };

    let mut stream = client.create_image(Some(options), None, None);

    while let Some(event) = stream.next().await {
        match event {
            Ok(info) => {
                if let Some(status) = &info.status {
                    debug!(status = %status, "pull progress");
                }
            }
            Err(e) => {
                // A stream error mid-pull is a hard failure — the image is
                // likely partially written and unusable.
                return Err(PodmanError::ImagePullFailed {
                    image: image_ref.to_string(),
                    source: e,
                });
            }
        }
    }

    info!("image pull complete");
    Ok(())
}

/// Ensure the image is available locally according to the configured pull policy.
///
/// This is the single call site for image readiness — `submit` calls this
/// before attempting container creation and never duplicates the policy logic.
#[tracing::instrument(skip(client), fields(image = %image_ref, policy = %policy))]
pub async fn ensure_image(
    client: &Docker,
    image_ref: &str,
    policy: &ImagePullPolicy,
) -> Result<(), PodmanError> {
    match policy {
        ImagePullPolicy::Always => {
            pull_image(client, image_ref).await?;
        }
        ImagePullPolicy::IfNotPresent => {
            if !is_image_present(client, image_ref).await? {
                info!("image not present locally, pulling");
                pull_image(client, image_ref).await?;
            } else {
                debug!("image present locally, skipping pull");
            }
        }
        ImagePullPolicy::Never => {
            if !is_image_present(client, image_ref).await? {
                return Err(PodmanError::ImagePullFailed {
                    image: image_ref.to_string(),
                    // Construct a synthetic bollard error. The Never policy
                    // means a missing image is always a configuration error,
                    // not a transient one.
                    source: bollard::errors::Error::DockerResponseServerError {
                        status_code: 404,
                        message: format!(
                            "image {image_ref} not present locally and pull policy is Never"
                        ),
                    },
                });
            }
        }
    }
    Ok(())
}
