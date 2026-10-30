//! Uploads a build log through the orchestrator's own `StorageClient` to a
//! running S3-compatible store: the quickest check that a dev object store
//! (see build-orchestrator/deploy/dev.dhall) is reachable and accepts the
//! orchestrator's requests.
//!
//! Ignored by default because it needs a running store:
//!   cargo test -p build-orchestrator --test object_store -- --ignored
//!
//! Defaults match the dev deployment on OrbStack (NodePort 30900); override
//! with CYCLOPS_S3_ENDPOINT, CYCLOPS_S3_ACCESS_KEY, CYCLOPS_S3_SECRET_KEY and
//! CYCLOPS_S3_BUCKET.

use build_orchestrator::client::{LogContainer, StorageClient, StorageConfig};
use uuid::Uuid;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::test]
#[ignore = "requires a running S3-compatible object store"]
async fn uploads_a_build_log() {
    let config = StorageConfig {
        endpoint_url: env_or("CYCLOPS_S3_ENDPOINT", "http://localhost:30900"),
        access_key: env_or("CYCLOPS_S3_ACCESS_KEY", "cyclops-dev"),
        secret_key: env_or("CYCLOPS_S3_SECRET_KEY", "cyclops-dev-secret"),
        region: "us-east-1".to_string(),
        bucket: env_or("CYCLOPS_S3_BUCKET", "cyclops-build-artifacts"),
    };
    let build_id = Uuid::new_v4();
    let log = format!("object store smoke test for build {build_id}\n");

    StorageClient::new(&config)
        .upload_log(build_id, LogContainer::Pipeline, log.as_bytes())
        .await
        .unwrap_or_else(|e| panic!("upload to {} failed: {e}", config.endpoint_url));
    println!(
        "uploaded builds/{build_id}/pipeline.log to {}",
        config.endpoint_url
    );
}
