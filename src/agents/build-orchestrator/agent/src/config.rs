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
    /// Env vars injected into every job's pipeline container — chiefly
    /// BROKER_ADDR and CASSINI_SERVER_NAME, without which neither a CI
    /// pipeline nor an analyzer can publish events.
    ///
    /// A list of {name, value} (the shape of a Kubernetes EnvVar), not a map:
    /// the `config` crate lowercases every key it reads from a file, so a map
    /// would turn BROKER_ADDR into broker_addr. Values are left alone.
    #[serde(default)]
    pub job_env: Vec<JobEnvVar>,
    pub kubernetes: Option<KubernetesBackendConfig>,
    pub podman: Option<PodmanConfig>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct JobEnvVar {
    pub name: String,
    pub value: String,
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

    /// Pipeline container requests and limits. Unset fields keep the
    /// CI-sized defaults (2 CPU / 4Gi limits, 500m / 512Mi requests).
    pub resources: Option<ResourceConfig>,

    /// Size limit of each job's workspace emptyDir, e.g. "2Gi". Unset means no
    /// limit: a large repo or archive can fill the node's disk.
    #[serde(default)]
    pub workspace_size_limit: Option<String>,

    /// Name of a Secret in `namespace` whose `ca-bundle.crt` key replaces the
    /// trust store in every job container. Needed behind a TLS-inspecting
    /// proxy (e.g. Zscaler), where clones and downloads otherwise fail
    /// certificate verification. The bundle must include the public roots,
    /// not only the proxy's CA.
    #[serde(default)]
    pub ca_bundle_secret: Option<String>,
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
    ///   1. YAML file at the path given by ORCHESTRATOR_CONFIG_FILE (default: cyclops.yaml)
    ///   2. Environment variable overrides with CYCLOPS__ prefix and __ separator
    pub fn load() -> Result<Self, config::ConfigError> {
        let config_path = std::env::var("ORCHESTRATOR_CONFIG_FILE")
            .unwrap_or_else(|_| "cyclops.yaml".to_string());
        Self::load_from(&config_path)
    }

    /// Load from a YAML file, then apply CYCLOPS__ environment overrides.
    pub fn load_from(config_path: &str) -> Result<Self, config::ConfigError> {
        config::Config::builder()
            .add_source(config::File::new(config_path, config::FileFormat::Yaml).required(true))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Through the real file loader: job_env names must keep their case
    /// (the `config` crate lowercases map keys, which once turned
    /// BROKER_ADDR into broker_addr in every job).
    #[test]
    fn job_env_names_keep_their_case_through_the_file_loader() {
        let dir = std::env::temp_dir().join(format!("cyclops-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cyclops.yaml");
        std::fs::write(
            &path,
            "backend:\n  driver: kubernetes\n  command: []\n  job_env:\n    - name: BROKER_ADDR\n      value: host.docker.internal:8080\n    - name: CASSINI_SERVER_NAME\n      value: Cassini\n  kubernetes:\n    namespace: ns\n    job_labels: {}\n  podman: null\nrepo_mappings: []\nstorage:\n  endpoint_url: http://localhost:30900\n  bucket: b\n  region: us-east-1\n  access_key: a\n  secret_key: s\n",
        )
        .unwrap();
        let config = OrchestratorConfig::load_from(path.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            config.backend.job_env,
            vec![
                JobEnvVar {
                    name: "BROKER_ADDR".into(),
                    value: "host.docker.internal:8080".into()
                },
                JobEnvVar {
                    name: "CASSINI_SERVER_NAME".into(),
                    value: "Cassini".into()
                },
            ]
        );
    }
}
