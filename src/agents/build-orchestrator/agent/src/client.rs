//! StorageClient wraps the AWS SDK S3 client configured for MinIO.
//!
//! # MinIO compatibility
//!
//! The AWS SDK S3 client works against MinIO with two configuration changes:
//! - `endpoint_url` set to the MinIO address (localhost:9000 when port-forwarded)
//! - `force_path_style` set to true — MinIO uses path-style URLs
//!   (http://host/bucket/key) rather than virtual-hosted style
//!   (http://bucket.host/key), which requires DNS wildcard configuration
//!   that is impractical in local dev.
//!
//! # Bucket layout
//!
//! All Cyclops artifacts live in a single configurable bucket.
//! Object keys are structured as:
//!   builds/<build_id>/pipeline.log
//!   builds/<build_id>/clone.log
//!   builds/<build_id>/manifest.json
//!
//! See keys.rs for the full key convention.
//!
//! # Log streaming
//!
//! Logs are streamed from the kube log API and uploaded to S3 in a single
//! PutObject call after the job reaches a terminal state. We buffer into
//! memory rather than using multipart upload because build logs are
//! typically small (< 10MB) and the added complexity of multipart is not
//! justified. If logs grow large enough to cause memory pressure, switch
//! to multipart upload with a 5MB part size.

use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::{config::Builder as S3ConfigBuilder, primitives::ByteStream, Client as S3Client};
use bytes::Bytes;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tracing::instrument;
use uuid::Uuid;

use crate::error::StorageError;
use crate::keys;

/// Configuration for the storage client.
/// Loaded from OrchestratorConfig and passed into the client at construction.
#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    /// MinIO/S3 endpoint URL.
    /// When port-forwarding locally: "http://localhost:9000"
    pub endpoint_url: String,

    /// S3 access key. For local MinIO this is the MINIO_ROOT_USER value.
    pub access_key: String,

    /// S3 secret key. For local MinIO this is the MINIO_ROOT_PASSWORD value.
    pub secret_key: String,

    /// AWS region. MinIO ignores this but the SDK requires it to be set.
    /// "us-east-1" is the conventional placeholder.
    pub region: String,

    /// Bucket where all Cyclops build artifacts are stored.
    /// Must exist before the orchestrator starts — the client does not
    /// create buckets automatically.
    pub bucket: String,
}

/// Thin async wrapper around the S3 client scoped to Cyclops artifact storage.
///
/// Clone is cheap — the inner client is Arc-wrapped by the SDK.
#[derive(Clone)]
pub struct StorageClient {
    s3: S3Client,
    bucket: String,
}

impl StorageClient {
    /// Construct a StorageClient configured for MinIO (or any S3-compatible backend).
    pub fn new(config: &StorageConfig) -> Self {
        let credentials = Credentials::new(
            &config.access_key,
            &config.secret_key,
            None, // session token — not used with MinIO static credentials
            None, // expiry
            "cyclops-static",
        );

        let s3_config = S3ConfigBuilder::new()
            .endpoint_url(&config.endpoint_url)
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            // MinIO requires path-style URLs. Without this the SDK generates
            // virtual-hosted URLs (http://bucket.localhost:9000/key) which
            // don't resolve correctly in local dev.'
            .behavior_version_latest()
            .force_path_style(true)
            .build();

        Self {
            s3: S3Client::from_conf(s3_config),
            bucket: config.bucket.clone(),
        }
    }

    /// Stream logs from a `tokio::io::AsyncRead` source and upload to S3.
    ///
    /// Reads the stream to completion into a memory buffer, then uploads
    /// as a single PutObject. This is appropriate for build logs which are
    /// typically small. The caller is responsible for opening the log stream
    /// from the backend before calling this.
    ///
    /// Key: builds/<build_id>/pipeline.log (or clone.log for init container logs)
    #[instrument(skip(self, log_stream), fields(build_id = %build_id))]
    pub async fn upload_log(
        &self,
        build_id: Uuid,
        container: LogContainer,
        mut log_stream: impl tokio::io::AsyncRead + Unpin,
    ) -> Result<(), StorageError> {
        let key = match container {
            LogContainer::Pipeline => keys::pipeline_log_key(build_id),
            LogContainer::Clone => keys::clone_log_key(build_id),
        };

        // Buffer the full log into memory. See module doc for why we don't
        // use multipart upload here.
        let mut buf = Vec::new();
        log_stream
            .read_to_end(&mut buf)
            .await
            .map_err(StorageError::Io)?;

        let byte_count = buf.len();
        let body = ByteStream::from(Bytes::from(buf));

        tracing::debug!("Uploading logs to bucket: {bucket}", bucket = self.bucket);

        self.s3
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .content_type("text/plain; charset=utf-8")
            .body(body)
            .send()
            .await
            .map_err(|e| StorageError::UploadFailed {
                key: key.clone(),
                reason: e.to_string(),
            })?;

        tracing::info!(
            build_id = %build_id,
            key = %key,
            bytes = byte_count,
            "log uploaded to storage"
        );

        Ok(())
    }

    /// Serialize and upload a k8s Job manifest to S3.
    ///
    /// The manifest is the exact `batch/v1 Job` struct that was submitted
    /// to the k8s API, serialized as pretty-printed JSON. Storing it enables
    /// exact replay of a build without going back through the full request
    /// cycle — hand the manifest back to `kubectl apply` or the k8s API
    /// and get an identical pod.
    ///
    /// Key: builds/<build_id>/manifest.json
    #[instrument(skip(self, manifest), fields(build_id = %build_id))]
    pub async fn upload_manifest<T: serde::Serialize>(
        &self,
        build_id: Uuid,
        manifest: &T,
    ) -> Result<(), StorageError> {
        let key = keys::manifest_key(build_id);

        // Pretty-print for human readability. Build manifests are small
        // (<10KB) so the size overhead versus compact JSON is negligible.
        let json = serde_json::to_vec_pretty(manifest)?;
        let byte_count = json.len();
        let body = ByteStream::from(Bytes::from(json));

        self.s3
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .content_type("application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| StorageError::UploadFailed {
                key: key.clone(),
                reason: e.to_string(),
            })?;

        tracing::info!(
            build_id = %build_id,
            key = %key,
            bytes = byte_count,
            "manifest uploaded to storage"
        );

        Ok(())
    }
}

/// Which container's logs are being uploaded.
#[derive(Debug, Clone, Copy)]
pub enum LogContainer {
    /// The main pipeline container (runs /bin/pipeline-runner or equivalent).
    Pipeline,
    /// The git clone init container.
    Clone,
}
