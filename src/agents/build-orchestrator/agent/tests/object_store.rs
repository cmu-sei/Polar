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
        access_key: env_or("CYCLOPS_S3_ACCESS_KEY", "build-orchestrator-dev"),
        secret_key: env_or("CYCLOPS_S3_SECRET_KEY", "build-orchestrator-dev-secret"),
        region: "us-east-1".to_string(),
        bucket: env_or("CYCLOPS_S3_BUCKET", "build-artifacts"),
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
