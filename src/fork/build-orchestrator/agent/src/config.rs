use serde::Deserialize;

use crate::client::StorageConfig;

#[derive(Debug, Deserialize, Clone)]
pub struct OrchestratorConfig {
    pub backend: BackendConfig,
    /// Which repositories are built, and with what. Held in memory: the runtime section `repo_mappings`
    /// (a signed document) replaces it whole whenever one arrives, so the file's list is only a start.
    #[serde(default)]
    pub repo_mappings: Vec<RepoMapping>,
    pub storage: StorageConfig,
    /// The most builds run at once. Each is a Job that may use several CPUs and gigabytes, so the number is
    /// capped; a request over it waits its turn. With jobs that have certificates it is also held to the number of
    /// `backend.kubernetes.accounts` (see `admission`). One unless set.
    #[serde(default = "one")]
    pub max_concurrent_builds: usize,
}

fn one() -> usize {
    1
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

    /// `Always`, `IfNotPresent` or `Never`, for the clone and pipeline images.
    #[serde(default)]
    pub image_pull_policy: Option<String>,

    /// The pod additions that give each job a certificate of its own, as JSON text (see
    /// `orchestrator_backend_k8s::job::JobIdentity`). Unset, jobs share the `client-tls` Secret, and
    /// builds that overlap supersede each other on the broker.
    #[serde(default)]
    pub identity: Option<String>,

    /// The ServiceAccounts a job's pod may run under, which the deployment makes: each is a workload in the
    /// authority model, and the broker allows one session per identity, so a job takes one that no running job
    /// holds. Needed with an `identity`, and no more builds run at once than there are.
    #[serde(default)]
    pub accounts: Vec<String>,

    /// Leave the repository's `.git` in each job's workspace, for a pipeline that records which commit it built.
    #[serde(default)]
    pub keep_git: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ResourceConfig {
    pub cpu_limit: Option<String>,
    pub memory_limit: Option<String>,
    pub cpu_request: Option<String>,
    pub memory_request: Option<String>,
}

impl OrchestratorConfig {
    /// How many builds may run at once: the configured cap, and no more than there are ServiceAccounts for jobs to
    /// hold when their certificates come from those.
    pub fn build_cap(&self) -> usize {
        let cap = self.max_concurrent_builds.max(1);
        match self.backend.kubernetes.as_ref() {
            Some(k) if k.identity.is_some() && !k.accounts.is_empty() => cap.min(k.accounts.len()),
            _ => cap,
        }
    }
}

// ── Repo mappings ─────────────────────────────────────────────────────────────

/// Maps a repository URL to its resolved pipeline image.
///
/// This is the static startup configuration. In future this will be pushed
/// dynamically by the framework-native scheduler. For now it is loaded from
/// YAML and held in memory — the orchestrator updates its in-memory state
/// when a bootstrap build produces a new image, but does not write back to disk.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct RepoMapping {
    /// Full HTTPS or SSH URL. Must match exactly what appears in BuildRequest.repo_url.
    pub repo_url: String,

    /// Fully qualified pipeline image reference, pinned by digest.
    /// If None, the first build for this repo will trigger a bootstrap job.
    /// e.g. "registry.internal.example.com/builds/myapp@sha256:..."
    pub pipeline_image: Option<String>,

    /// The pipeline's command for this repository, in place of the backend's.
    #[serde(default)]
    pub command: Option<Vec<String>>,

    /// The credentials a build of this repository may be given, by name. None unless named here: a build
    /// request cannot ask for more.
    #[serde(default)]
    pub credentials: Vec<String>,
}

// ── Loader ────────────────────────────────────────────────────────────────────

impl OrchestratorConfig {
    /// Load configuration from a YAML file produced by Dhall compilation.
    ///
    /// Resolution order (later sources win):
    ///   1. YAML file at the path given by ORCHESTRATOR_CONFIG_FILE (default: build-orchestrator.yaml)
    ///   2. Environment variable overrides with BUILD_ORCHESTRATOR__ prefix and __ separator
    pub fn load() -> Result<Self, config::ConfigError> {
        let config_path = std::env::var("ORCHESTRATOR_CONFIG_FILE")
            .unwrap_or_else(|_| "build-orchestrator.yaml".to_string());
        Self::load_from(&config_path)
    }

    /// Load from a YAML file, then apply BUILD_ORCHESTRATOR__ environment overrides.
    pub fn load_from(config_path: &str) -> Result<Self, config::ConfigError> {
        config::Config::builder()
            .add_source(config::File::new(config_path, config::FileFormat::Yaml).required(true))
            .add_source(config::Environment::with_prefix("BUILD_ORCHESTRATOR").separator("__"))
            .build()?
            .try_deserialize()
    }

    /// The credentials a build of this repository may be given, by name: none unless its mapping names them.
    pub fn credentials_for(&self, repo_url: &str) -> &[String] {
        self.repo_mappings
            .iter()
            .find(|m| m.repo_url == repo_url)
            .map(|m| m.credentials.as_slice())
            .unwrap_or(&[])
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
        let dir = std::env::temp_dir().join(format!("build-orchestrator-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("build-orchestrator.yaml");
        std::fs::write(
            &path,
            "backend:\n  driver: kubernetes\n  command: []\n  job_env:\n    - name: BROKER_ADDR\n      value: host.docker.internal:8080\n    - name: CASSINI_SERVER_NAME\n      value: Cassini\n  kubernetes:\n    namespace: ns\n    job_labels: {}\nrepo_mappings: []\nstorage:\n  endpoint_url: http://localhost:30900\n  bucket: b\n  region: us-east-1\n  access_key: a\n  secret_key: s\n",
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

#[cfg(test)]
mod build_cap {
    use super::*;

    fn config(cap: usize, kubernetes: serde_json::Value) -> OrchestratorConfig {
        serde_json::from_value(serde_json::json!({
            "backend": { "driver": "kubernetes", "command": [], "kubernetes": kubernetes },
            "storage": {
                "endpoint_url": "http://localhost:9000", "bucket": "b", "region": "r",
                "access_key": "abcdefghijklmnop", "secret_key": "abcdefghijklmnop"
            },
            "max_concurrent_builds": cap,
        }))
        .expect("a configuration")
    }

    #[test]
    fn the_cap_is_one_unless_set() {
        let c: OrchestratorConfig = serde_json::from_value(serde_json::json!({
            "backend": { "driver": "kubernetes", "command": [] },
            "storage": {
                "endpoint_url": "http://localhost:9000", "bucket": "b", "region": "r",
                "access_key": "abcdefghijklmnop", "secret_key": "abcdefghijklmnop"
            },
        }))
        .unwrap();
        assert_eq!(c.build_cap(), 1);
    }

    #[test]
    fn accounts_for_jobs_hold_the_cap_down() {
        let k = serde_json::json!({ "namespace": "build-jobs", "identity": "{}", "accounts": ["build-job-1", "build-job-2"] });
        assert_eq!(config(8, k.clone()).build_cap(), 2, "one session per identity, two identities");
        assert_eq!(config(1, k).build_cap(), 1, "the cap still applies below the accounts");
    }

    #[test]
    fn without_certificates_for_jobs_the_cap_is_the_setting() {
        let k = serde_json::json!({ "namespace": "build-jobs" });
        assert_eq!(config(3, k).build_cap(), 3);
    }

    #[test]
    fn the_accounts_survive_the_loader() {
        let dir = std::env::temp_dir().join(format!("bo-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("build-orchestrator.yaml");
        std::fs::write(
            &file,
            r#"{"max_concurrent_builds": 2,
                "backend": {"driver": "kubernetes", "command": ["nu"],
                  "kubernetes": {"namespace": "build-jobs", "identity": "{}", "accounts": ["build-job-1", "build-job-2"]}},
                "storage": {"endpoint_url": "http://localhost:9000", "bucket": "b", "region": "r", "access_key": "abcdefghijklmnop", "secret_key": "abcdefghijklmnop"}}"#,
        )
        .unwrap();
        let c = OrchestratorConfig::load_from(file.to_str().unwrap()).expect("loads");
        let k = c.backend.kubernetes.as_ref().unwrap();
        assert_eq!(k.accounts, vec!["build-job-1".to_string(), "build-job-2".to_string()]);
        assert_eq!(c.build_cap(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
