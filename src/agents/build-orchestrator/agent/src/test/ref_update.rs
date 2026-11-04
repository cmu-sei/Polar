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

/// Tests for turning git observer `RefUpdated` messages into build requests.
#[cfg(test)]
#[allow(clippy::module_inception)]
mod ref_update {
    use crate::actors::supervisor::{EventConversionError, OrchestratorSupervisor};
    use crate::client::StorageConfig;
    use crate::config::{BackendConfig, OrchestratorConfig, RepoMapping};
    use orchestrator_core::types::SourceRef;
    use polar::graph::nodes::git::RepoId;

    const SHA: &str = "8b0214f218abb26cdfe2ade8f117abb0cf4b13e6";

    fn config(repo_url: &str) -> OrchestratorConfig {
        OrchestratorConfig {
            backend: BackendConfig {
                driver: "kubernetes".to_string(),
                command: vec!["nu".to_string(), "scripts/pipeline/ci.nu".to_string()],
                job_env: Default::default(),
                kubernetes: None,
            },
            repo_mappings: vec![RepoMapping {
                repo_url: repo_url.to_string(),
                pipeline_image: Some("registry.example.com/polar-ci@sha256:abc".to_string()),
                ..Default::default()
            }],
            storage: StorageConfig {
                endpoint_url: "http://localhost:9000".to_string(),
                bucket: "build-artifacts".to_string(),
                region: "us-east-1".to_string(),
                access_key: "abcdefghijklmnop".to_string(),
                secret_key: "abcdefghijklmnop".to_string(),
            },
            max_concurrent_builds: 1,
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

    fn request_with_command(url: &str, command: &[&str]) -> orchestrator_core::types::BuildRequest {
        let mut request = OrchestratorSupervisor::build_request_from_ref_update(
            &config(url),
            &RepoId::from_url(url),
            "refs/heads/main",
            SHA,
            "1700000000000",
        )
        .unwrap()
        .unwrap();
        request.command = command.iter().map(|s| s.to_string()).collect();
        request
    }

    #[test]
    fn a_mapped_repositorys_command_is_the_mappings_and_never_the_requests() {
        let url = "https://git.example.org/a.git";
        let default = vec!["nu".to_string(), "default.nu".to_string()];

        // The mapping gives its own command.
        let mapping = RepoMapping {
            repo_url: url.to_string(),
            command: Some(vec!["sh".into(), "-c".into(), "mapped".into()]),
            ..Default::default()
        };
        let mut request = request_with_command(url, &["sh", "-c", "from the request"]);
        OrchestratorSupervisor::command_from_mapping(&[mapping], &default, &mut request);
        assert_eq!(request.command, ["sh", "-c", "mapped"]);

        // The mapping gives none: the configuration file's.
        let bare = RepoMapping {
            repo_url: url.to_string(),
            ..Default::default()
        };
        let mut request = request_with_command(url, &["sh", "-c", "from the request"]);
        OrchestratorSupervisor::command_from_mapping(&[bare], &default, &mut request);
        assert_eq!(request.command, default);

        // A repository with no mapping has nothing but the request's.
        let mut request = request_with_command(url, &["sh", "-c", "from the request"]);
        OrchestratorSupervisor::command_from_mapping(&[], &default, &mut request);
        assert_eq!(request.command, ["sh", "-c", "from the request"]);
    }
}
