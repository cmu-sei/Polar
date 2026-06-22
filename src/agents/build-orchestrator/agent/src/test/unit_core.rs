/// Tests for BuildState transition logic and BuildRecord mutation semantics.
///
/// These are pure unit tests with no I/O, no actors, no backends.
/// They should run in microseconds and never flake.
#[cfg(test)]
mod build_state_transitions {
    use chrono::Utc;
    use orchestrator_core::types::{BuildRecord, BuildRequest, BuildState};
    use uuid::Uuid;

    fn dummy_request() -> BuildRequest {
        BuildRequest {
            build_id: Uuid::new_v4(),
            repo_url: "https://gitlab.example.com/polar/myapp".to_string(),
            commit_sha: "a".repeat(40),
            requested_by: "test-harness".to_string(),
            requested_at: Utc::now(),
            target_registry: "registry.example.com/builds".to_string(),
            metadata: Default::default(),
            command: vec![],
        }
    }

    // ── Forward path ──────────────────────────────────────────────────────────

    #[test]
    fn pending_to_resolving_image_is_valid() {
        assert!(BuildState::Pending.can_transition_to(&BuildState::ResolvingImage));
    }

    #[test]
    fn resolving_image_to_scheduled_is_valid() {
        // Image was already present — skip bootstrap entirely.
        assert!(BuildState::ResolvingImage.can_transition_to(&BuildState::Scheduled));
    }

    #[test]
    fn resolving_image_to_bootstrapping_is_valid() {
        assert!(BuildState::ResolvingImage.can_transition_to(&BuildState::Bootstrapping));
    }

    #[test]
    fn bootstrapping_to_scheduled_is_valid() {
        assert!(BuildState::Bootstrapping.can_transition_to(&BuildState::Scheduled));
    }

    #[test]
    fn scheduled_to_running_is_valid() {
        assert!(BuildState::Scheduled.can_transition_to(&BuildState::Running));
    }

    #[test]
    fn running_to_succeeded_is_valid() {
        assert!(BuildState::Running.can_transition_to(&BuildState::Succeeded));
    }

    // ── Failure paths ─────────────────────────────────────────────────────────

    #[test]
    fn resolving_image_to_failed_is_valid() {
        assert!(BuildState::ResolvingImage.can_transition_to(&BuildState::Failed));
    }

    #[test]
    fn bootstrapping_to_failed_is_valid() {
        assert!(BuildState::Bootstrapping.can_transition_to(&BuildState::Failed));
    }

    #[test]
    fn scheduled_to_failed_is_valid() {
        assert!(BuildState::Scheduled.can_transition_to(&BuildState::Failed));
    }

    #[test]
    fn running_to_failed_is_valid() {
        assert!(BuildState::Running.can_transition_to(&BuildState::Failed));
    }

    // ── Cancellation paths ────────────────────────────────────────────────────

    #[test]
    fn cancelled_is_reachable_from_all_non_terminal_states() {
        let non_terminal = [
            BuildState::Pending,
            BuildState::ResolvingImage,
            BuildState::Bootstrapping,
            BuildState::Scheduled,
            BuildState::Running,
        ];
        for state in &non_terminal {
            assert!(
                state.can_transition_to(&BuildState::Cancelled),
                "{state} should be able to transition to Cancelled"
            );
        }
    }

    // ── Illegal transitions ───────────────────────────────────────────────────

    #[test]
    fn terminal_states_cannot_transition_forward() {
        let terminal = [
            BuildState::Succeeded,
            BuildState::Failed,
            BuildState::Cancelled,
        ];
        let any_state = BuildState::Running;
        for state in &terminal {
            assert!(
                !state.can_transition_to(&any_state),
                "{state} should not be able to transition to {any_state}"
            );
        }
    }

    #[test]
    fn pending_cannot_skip_to_running() {
        assert!(!BuildState::Pending.can_transition_to(&BuildState::Running));
    }

    #[test]
    fn pending_cannot_skip_to_succeeded() {
        assert!(!BuildState::Pending.can_transition_to(&BuildState::Succeeded));
    }

    #[test]
    fn running_cannot_go_back_to_scheduled() {
        assert!(!BuildState::Running.can_transition_to(&BuildState::Scheduled));
    }

    #[test]
    fn succeeded_cannot_transition_to_failed() {
        assert!(!BuildState::Succeeded.can_transition_to(&BuildState::Failed));
    }

    // ── Unreconciled ──────────────────────────────────────────────────────────

    #[test]
    fn unreconciled_is_reachable_from_active_states() {
        // These are the states where a connectivity loss can occur mid-flight.
        let active = [
            BuildState::Scheduled,
            BuildState::Bootstrapping,
            BuildState::Running,
        ];
        for state in &active {
            assert!(
                state.can_transition_to(&BuildState::Unreconciled),
                "{state} should be able to transition to Unreconciled"
            );
        }
    }

    #[test]
    fn unreconciled_is_not_reachable_from_pre_execution_states() {
        // Pending and ResolvingImage haven't submitted anything to the backend
        // yet — there's nothing to reconcile.
        let pre_exec = [BuildState::Pending, BuildState::ResolvingImage];
        for state in &pre_exec {
            assert!(
                !state.can_transition_to(&BuildState::Unreconciled),
                "{state} should not be able to transition to Unreconciled"
            );
        }
    }

    // ── BuildRecord::transition ───────────────────────────────────────────────

    #[test]
    fn valid_transition_updates_state_and_timestamp() {
        let mut record = BuildRecord::new(dummy_request());
        let before = record.updated_at;

        // Sleep briefly so the timestamp changes — Utc::now() has ms resolution.
        std::thread::sleep(std::time::Duration::from_millis(2));

        record.transition(BuildState::ResolvingImage).unwrap();

        assert_eq!(record.state, BuildState::ResolvingImage);
        assert!(record.updated_at > before);
    }

    #[test]
    fn invalid_transition_returns_error_and_does_not_mutate_state() {
        let mut record = BuildRecord::new(dummy_request());
        assert_eq!(record.state, BuildState::Pending);

        let result = record.transition(BuildState::Succeeded);

        assert!(result.is_err());
        // State must not have changed.
        assert_eq!(record.state, BuildState::Pending);
    }

    #[test]
    fn is_terminal_is_accurate() {
        assert!(BuildState::Succeeded.is_terminal());
        assert!(BuildState::Failed.is_terminal());
        assert!(BuildState::Cancelled.is_terminal());

        assert!(!BuildState::Pending.is_terminal());
        assert!(!BuildState::ResolvingImage.is_terminal());
        assert!(!BuildState::Bootstrapping.is_terminal());
        assert!(!BuildState::Scheduled.is_terminal());
        assert!(!BuildState::Running.is_terminal());
        // Unreconciled is intentionally non-terminal — reconciliation resolves it.
        assert!(!BuildState::Unreconciled.is_terminal());
    }
}

/// Tests for OrchestratorConfig::pipeline_image_for lookup semantics.
#[cfg(test)]
mod config_image_lookup {
    use orchestrator_backend_podman::backend::PodmanConfig;

    use crate::client::StorageConfig;
    use crate::config::{BackendConfig, OrchestratorConfig, RepoMapping};

    fn config_with_mappings(mappings: Vec<RepoMapping>) -> OrchestratorConfig {
        OrchestratorConfig {
            backend: BackendConfig {
                driver: "podman".to_string(),
                command: vec!["ls".to_string()],
                kubernetes: None,
                podman: PodmanConfig::rootless_default().into(),
            },

            repo_mappings: mappings,

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
    fn returns_image_for_known_repo() {
        let config = config_with_mappings(vec![RepoMapping {
            repo_url: "https://gitlab.example.com/polar/myapp".to_string(),
            pipeline_image: Some("registry.example.com/builds/myapp@sha256:deadbeef".to_string()),
        }]);

        assert_eq!(
            config.pipeline_image_for("https://gitlab.example.com/polar/myapp"),
            Some("registry.example.com/builds/myapp@sha256:deadbeef")
        );
    }

    #[test]
    fn returns_none_for_unknown_repo() {
        let config = config_with_mappings(vec![]);

        assert_eq!(
            config.pipeline_image_for("https://gitlab.example.com/polar/unknown"),
            None
        );
    }

    #[test]
    fn returns_none_when_image_not_yet_set() {
        let config = config_with_mappings(vec![RepoMapping {
            repo_url: "https://gitlab.example.com/polar/myapp".to_string(),
            pipeline_image: None,
        }]);

        assert_eq!(
            config.pipeline_image_for("https://gitlab.example.com/polar/myapp"),
            None
        );
    }

    #[test]
    fn match_is_exact_not_prefix() {
        let config = config_with_mappings(vec![RepoMapping {
            repo_url: "https://gitlab.example.com/polar/myapp".to_string(),
            pipeline_image: Some("registry.example.com/builds/myapp@sha256:abc".to_string()),
        }]);

        // A prefix of the registered URL should not match.
        assert_eq!(
            config.pipeline_image_for("https://gitlab.example.com/polar"),
            None
        );
    }
}

/// Tests for derive_bootstrap_output_image naming convention.
///
/// This function has an implicit contract with the bootstrap job — both sides
/// must agree on the naming convention or the orchestrator will poll a
/// non-existent image after a bootstrap succeeds. These tests document and
/// enforce that contract.
#[cfg(test)]
mod bootstrap_image_derivation {
    // derive_bootstrap_output_image is private to build_job — we test it via
    // a pub(crate) re-export added specifically for testing.
    use crate::actors::build_job::derive_bootstrap_output_image;

    #[test]
    fn derives_expected_image_ref() {
        let result = derive_bootstrap_output_image(
            "https://gitlab.example.com/polar/myapp",
            "abc123def456789000000000",
            "registry.example.com/builds",
        );
        assert_eq!(result, "registry.example.com/builds/myapp:abc123def456");
    }

    #[test]
    fn slugifies_non_alphanumeric_chars_in_repo_name() {
        let result = derive_bootstrap_output_image(
            "https://gitlab.example.com/polar/my.app_v2",
            "abc123def456789000000000",
            "registry.example.com/builds",
        );
        // Dots and underscores become hyphens, result is lowercased.
        assert_eq!(result, "registry.example.com/builds/my-app-v2:abc123def456");
    }

    #[test]
    fn sha_is_truncated_to_12_chars() {
        let sha = "a".repeat(40);
        let result = derive_bootstrap_output_image(
            "https://gitlab.example.com/polar/myapp",
            &sha,
            "registry.example.com/builds",
        );
        // Tag should be the first 12 characters of the SHA.
        assert!(result.ends_with(":aaaaaaaaaaaa"));
    }

    #[test]
    fn short_sha_does_not_panic() {
        // SHA shorter than 12 chars should use the full SHA rather than panicking.
        let result = derive_bootstrap_output_image(
            "https://gitlab.example.com/polar/myapp",
            "abc123",
            "registry.example.com/builds",
        );
        assert!(result.ends_with(":abc123"));
    }
}
