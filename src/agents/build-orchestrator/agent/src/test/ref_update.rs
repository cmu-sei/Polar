/// Tests for turning git observer `RefUpdated` messages into build requests.
#[cfg(test)]
mod ref_update {
    use crate::actors::supervisor::{EventConversionError, OrchestratorSupervisor};
    use crate::client::StorageConfig;
    use crate::config::{BackendConfig, OrchestratorConfig, RepoMapping};
    use orchestrator_backend_podman::backend::PodmanConfig;
    use orchestrator_core::types::SourceRef;
    use polar::graph::nodes::git::RepoId;

    const SHA: &str = "8b0214f218abb26cdfe2ade8f117abb0cf4b13e6";

    fn config(repo_url: &str) -> OrchestratorConfig {
        OrchestratorConfig {
            backend: BackendConfig {
                driver: "podman".to_string(),
                command: vec!["nu".to_string(), "scripts/pipeline/ci.nu".to_string()],
                job_env: Default::default(),
                kubernetes: None,
                podman: PodmanConfig::rootless_default().into(),
            },
            repo_mappings: vec![RepoMapping {
                repo_url: repo_url.to_string(),
                pipeline_image: Some("registry.example.com/polar-ci@sha256:abc".to_string()),
            }],
            storage: StorageConfig {
                endpoint_url: "http://localhost:9000".to_string(),
                bucket: "cyclops-artifacts".to_string(),
                region: "us-east-1".to_string(),
                access_key: "abcdefghijklmnop".to_string(),
                secret_key: "abcdefghijklmnop".to_string(),
            },
        }
    }

    #[test]
    fn mapped_repo_becomes_git_build_with_mapping_url() {
        let url = "https://github.com/cmu-sei/Polar.git";
        let request = OrchestratorSupervisor::build_request_from_ref_update(
            &config(url),
            &RepoId::from_url(url),
            "refs/heads/main",
            SHA,
            "1700000000000",
        )
        .unwrap()
        .expect("mapped repo should build");

        assert_eq!(
            request.source,
            SourceRef::Git {
                url: url.to_string(),
                commit_sha: SHA.to_string()
            }
        );
        assert_eq!(request.command, vec!["nu", "scripts/pipeline/ci.nu"]);
        assert_eq!(request.metadata["git_ref"], "refs/heads/main");
        assert!(request.validate().is_ok());
    }

    #[test]
    fn mapping_matches_when_observer_used_normalized_url() {
        let request = OrchestratorSupervisor::build_request_from_ref_update(
            &config("https://GitHub.com/cmu-sei/Polar.git"),
            &RepoId::from_url("https://github.com/cmu-sei/Polar"),
            "refs/heads/main",
            SHA,
            "0",
        )
        .unwrap();
        assert!(request.is_some());
    }

    #[test]
    fn unmapped_repo_is_not_built() {
        let request = OrchestratorSupervisor::build_request_from_ref_update(
            &config("https://github.com/cmu-sei/Polar.git"),
            &RepoId::from_url("https://github.com/someone/else.git"),
            "refs/heads/main",
            SHA,
            "0",
        )
        .unwrap();
        assert!(request.is_none());
    }

    #[test]
    fn invalid_sha_is_rejected() {
        let url = "https://github.com/cmu-sei/Polar.git";
        let result = OrchestratorSupervisor::build_request_from_ref_update(
            &config(url),
            &RepoId::from_url(url),
            "refs/heads/main",
            "not-a-sha",
            "0",
        );
        assert!(matches!(
            result,
            Err(EventConversionError::InvalidSource(_))
        ));
    }
}
