//! `polar`: administer a Polar deployment.
//!
//! It runs a plan the consumer declares in Dhall (see `src/deploy/types/plan.dhall`) and reports on
//! the agents the operator keeps alive. It knows nothing about what is being deployed; if a feature
//! needs to, it does not belong here.

pub mod cluster;
pub mod exec;
pub mod kube_cluster;
pub mod plan;
pub mod render;
pub mod secrets;
pub mod status;

use anyhow::{Context, Result};
use plan::Plan;
use std::path::Path;

/// Read a plan: rendered YAML as it is, or a `.dhall` file evaluated first.
pub fn load_plan(path: &Path) -> Result<Plan> {
    let text = if path.extension().is_some_and(|e| e == "dhall") {
        render::dhall_to_yaml(path, false)?
    } else {
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?
    };
    Plan::from_yaml(&text).with_context(|| format!("{} is not a plan", path.display()))
}

/// Fresh random bytes, from the operating system's secure source.
pub fn random_bytes(n: usize) -> Vec<u8> {
    use rand::RngExt;
    let mut bytes = vec![0u8; n];
    rand::rng().fill(&mut bytes[..]);
    bytes
}
