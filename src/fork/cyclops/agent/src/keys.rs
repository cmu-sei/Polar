//! Object key conventions for Cyclops artifact storage.
//!
//! All objects live under a build-scoped prefix so any build's artifacts
//! can be listed, fetched, or deleted as a unit:
//!
//!   builds/<build_id>/pipeline.log    — stdout+stderr from the pipeline container
//!   builds/<build_id>/clone.log       — stdout+stderr from the clone init container
//!   builds/<build_id>/manifest.json   — the k8s Job spec submitted for this build
//!
//! The build_id is a UUID, which makes keys globally unique and directly
//! correlatable with BuildRecord entries in the registry actor and with
//! Polar's knowledge graph once the Cassini integration is wired up.

use uuid::Uuid;

pub fn build_prefix(build_id: Uuid) -> String {
    format!("builds/{}", build_id)
}

pub fn pipeline_log_key(build_id: Uuid) -> String {
    format!("builds/{}/pipeline.log", build_id)
}

pub fn clone_log_key(build_id: Uuid) -> String {
    format!("builds/{}/clone.log", build_id)
}

pub fn manifest_key(build_id: Uuid) -> String {
    format!("builds/{}/manifest.json", build_id)
}
