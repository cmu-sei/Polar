use orchestrator_backend_podman::backend::{PodmanBackend, PodmanConfig};
use serde::Deserialize;

use crate::client::StorageConfig;

#[derive(Debug, Deserialize, Clone)]
pub struct OrchestratorConfig {
    pub backend: BackendConfig,
    pub repo_mappings: Vec<RepoMapping>,
    pub storage: StorageConfig,
}

// ── Backend ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    /// Which backend to use. Currently only "kubernetes" is supported.
    pub driver: String,
    /// The command entrypoint to use to trigger the job
    pub command: Vec<String>,
    pub kubernetes: Option<KubernetesBackendConfig>,
    pub podman: Option<PodmanConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct KubernetesBackendConfig {
    /// Namespace where build Jobs are created.
    pub namespace: String,

    /// If None, uses in-cluster config. If Some, loads from this kubeconfig path.
    pub kubeconfig_path: Option<String>,

    /// Labels applied to all spawned Jobs.
    #[serde(default)]
    pub job_labels: std::collections::HashMap<String, String>,

    pub resources: Option<ResourceConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ResourceConfig {
    pub cpu_limit: Option<String>,
    pub memory_limit: Option<String>,
    pub cpu_request: Option<String>,
    pub memory_request: Option<String>,
}

// ── Repo mappings ─────────────────────────────────────────────────────────────

/// Maps a repository URL to its resolved pipeline image.
///
/// This is the static startup configuration. In future this will be pushed
/// dynamically by the framework-native scheduler. For now it is loaded from
/// YAML and held in memory — the orchestrator updates its in-memory state
/// when a bootstrap build produces a new image, but does not write back to disk.
#[derive(Debug, Deserialize, Clone)]
pub struct RepoMapping {
    /// Full HTTPS or SSH URL. Must match exactly what appears in BuildRequest.repo_url.
    pub repo_url: String,

    /// Fully qualified pipeline image reference, pinned by digest.
    /// If None, the first build for this repo will trigger a bootstrap job.
    /// e.g. "registry.internal.example.com/builds/myapp@sha256:..."
    pub pipeline_image: Option<String>,
}

// ── Loader ────────────────────────────────────────────────────────────────────

impl OrchestratorConfig {
    /// Load configuration from a YAML file produced by Dhall compilation.
    ///
    /// Resolution order (later sources win):
    ///   1. YAML file at the path given by CYCLOPS_CONFIG (default: cyclops.yaml)
    ///   2. Environment variable overrides with CYCLOPS__ prefix and __ separator
    pub fn load() -> Result<Self, config::ConfigError> {
        let config_path = std::env::var("ORCHESTRATOR_CONFIG_FILE")
            .unwrap_or_else(|_| "cyclops.yaml".to_string());

        config::Config::builder()
            .add_source(config::File::new(&config_path, config::FileFormat::Yaml).required(true))
            .add_source(config::Environment::with_prefix("CYCLOPS").separator("__"))
            .build()?
            .try_deserialize()
    }

    /// Look up the pipeline image for a given repo URL.
    /// Returns None if no mapping exists or the image has not been set yet.
    pub fn pipeline_image_for(&self, repo_url: &str) -> Option<&str> {
        self.repo_mappings
            .iter()
            .find(|m| m.repo_url == repo_url)
            .and_then(|m| m.pipeline_image.as_deref())
    }
}
