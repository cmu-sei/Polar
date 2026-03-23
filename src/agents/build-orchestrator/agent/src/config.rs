use serde::Deserialize;

use crate::client::StorageConfig;

#[derive(Debug, Deserialize, Clone)]
pub struct OrchestratorConfig {
    pub backend: BackendConfig,
    pub cassini: CassiniConfig,
    pub bootstrap: BootstrapConfig,
    pub credentials: CredentialsConfig,
    pub repo_mappings: Vec<RepoMapping>,
    pub log: LogConfig,
    pub storage: StorageConfig,
}

// ── Backend ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    /// Which backend to use. Currently only "kubernetes" is supported.
    pub driver: String,
    pub kubernetes: Option<KubernetesBackendConfig>,
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

// ── Bootstrap ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct BootstrapConfig {
    /// The well-known Nix builder image used to evaluate container.dhall and
    /// produce repo-specific pipeline images. Must be pinned by digest.
    /// Built manually and almost never changes.
    /// e.g. "registry.internal.example.com/cyclops/nix-bootstrap@sha256:..."
    pub builder_image: String,

    /// Path within every repo to the Dhall config that defines the pipeline
    /// container. Convention-based — the same for all repos.
    #[serde(default = "default_container_config_ref")]
    pub container_config_ref: String,

    /// Registry base URL where bootstrap jobs push produced pipeline images,
    /// and where pipeline jobs pull them from.
    /// e.g. "registry.internal.example.com/builds"
    pub target_registry: String,
}

fn default_container_config_ref() -> String {
    "container.dhall".to_string()
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

// ── Credentials ──────────────────────────────────────────────────────────────

/// Names of Kubernetes Secrets that must exist in the build namespace.
///
/// These are global for now — every build uses the same secrets. Per-repo
/// credential selection will be added when the scheduler owns repo mappings
/// and can carry per-repo credential refs in the build request.
///
/// Both secrets must be created in the build namespace before any job runs.
/// The orchestrator does not provision them.
#[derive(Debug, Deserialize, Clone)]
pub struct CredentialsConfig {
    /// Name of a Secret containing `username` and `password` keys for git
    /// HTTP auth. Works for GitLab deploy tokens, GitHub PATs, etc.
    pub git_secret_name: String,

    /// Name of a Secret of type kubernetes.io/dockerconfigjson used for
    /// pulling pipeline images and pushing bootstrap output.
    pub registry_secret_name: String,
}

// ── Cassini ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct CassiniConfig {
    pub broker_url: String,
    pub inbound_subject: String,
}

// ── Logging ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct LogConfig {
    #[serde(default = "default_log_format")]
    pub format: String,

    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_format() -> String {
    "json".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

// ── Loader ────────────────────────────────────────────────────────────────────

impl OrchestratorConfig {
    /// Load configuration from a YAML file produced by Dhall compilation.
    ///
    /// Resolution order (later sources win):
    ///   1. YAML file at the path given by CYCLOPS_CONFIG (default: cyclops.yaml)
    ///   2. Environment variable overrides with CYCLOPS__ prefix and __ separator
    pub fn load() -> Result<Self, config::ConfigError> {
        let config_path =
            std::env::var("CYCLOPS_CONFIG").unwrap_or_else(|_| "cyclops.yaml".to_string());

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
