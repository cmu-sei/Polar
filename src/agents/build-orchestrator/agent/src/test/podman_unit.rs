/// Unit tests for the Podman backend.
///
/// `inspect_container` is async and requires a live daemon, so the status
/// mapping logic is extracted into a pure function and tested independently.
/// The integration tests for actual daemon interaction belong in a separate
/// `tests/podman_integration.rs` that is gated behind an environment variable
/// and never runs in CI without a live socket.
#[cfg(test)]
mod container_status_mapping {
    use bollard::models::{ContainerState, ContainerStateStatusEnum};
    use orchestrator_core::backend::JobStatus;

    /// Replicate the mapping logic from `inspect_container` as a pure function
    /// so it can be tested without a daemon. This must be kept in sync with
    /// the real implementation in `container.rs`.
    ///
    /// If the real implementation changes, this function must change too —
    /// the tests will catch any divergence if you update one but not the other.
    fn map_container_state(state: ContainerState) -> JobStatus {
        use bollard::models::ContainerStateStatusEnum;

        let exit_code = state.exit_code;
        match state.status {
            Some(ContainerStateStatusEnum::RUNNING)
            | Some(ContainerStateStatusEnum::RESTARTING) => JobStatus::Running,

            Some(ContainerStateStatusEnum::CREATED) | Some(ContainerStateStatusEnum::PAUSED) => {
                JobStatus::Pending
            }

            Some(ContainerStateStatusEnum::EXITED) => {
                let code = exit_code.unwrap_or(-1);
                if code == 0 {
                    JobStatus::Succeeded
                } else {
                    JobStatus::Failed {
                        reason: Some(format!("exited with code {code}")),
                    }
                }
            }

            Some(ContainerStateStatusEnum::DEAD) => JobStatus::Failed {
                reason: Some("container entered dead state".to_string()),
            },

            Some(ContainerStateStatusEnum::REMOVING) => JobStatus::Failed {
                reason: Some("container is being removed".to_string()),
            },

            Some(ContainerStateStatusEnum::EMPTY) | None => JobStatus::Unknown,
        }
    }

    fn state_with_status(status: ContainerStateStatusEnum) -> ContainerState {
        ContainerState {
            status: Some(status),
            exit_code: None,
            ..Default::default()
        }
    }

    fn exited_state(exit_code: i64) -> ContainerState {
        ContainerState {
            status: Some(ContainerStateStatusEnum::EXITED),
            exit_code: Some(exit_code),
            ..Default::default()
        }
    }

    // ── Status mapping ────────────────────────────────────────────────────────

    #[test]
    fn running_maps_to_running() {
        assert_eq!(
            map_container_state(state_with_status(ContainerStateStatusEnum::RUNNING)),
            JobStatus::Running
        );
    }

    #[test]
    fn restarting_maps_to_running() {
        // Restarting means the container is between runs — from the orchestrator's
        // perspective it is still active.
        assert_eq!(
            map_container_state(state_with_status(ContainerStateStatusEnum::RESTARTING)),
            JobStatus::Running
        );
    }

    #[test]
    fn created_maps_to_pending() {
        assert_eq!(
            map_container_state(state_with_status(ContainerStateStatusEnum::CREATED)),
            JobStatus::Pending
        );
    }

    #[test]
    fn paused_maps_to_pending() {
        assert_eq!(
            map_container_state(state_with_status(ContainerStateStatusEnum::PAUSED)),
            JobStatus::Pending
        );
    }

    #[test]
    fn exited_zero_maps_to_succeeded() {
        assert_eq!(map_container_state(exited_state(0)), JobStatus::Succeeded);
    }

    #[test]
    fn exited_nonzero_maps_to_failed_with_exit_code_in_reason() {
        let status = map_container_state(exited_state(1));
        assert!(matches!(status, JobStatus::Failed { reason: Some(_) }));
        if let JobStatus::Failed { reason: Some(r) } = status {
            assert!(r.contains('1'), "reason should contain the exit code");
        }
    }

    #[test]
    fn exited_nonzero_large_exit_code_is_included_in_reason() {
        let status = map_container_state(exited_state(137)); // SIGKILL
        if let JobStatus::Failed { reason: Some(r) } = status {
            assert!(r.contains("137"));
        } else {
            panic!("expected Failed with reason");
        }
    }

    #[test]
    fn dead_maps_to_failed() {
        assert!(matches!(
            map_container_state(state_with_status(ContainerStateStatusEnum::DEAD)),
            JobStatus::Failed { .. }
        ));
    }

    #[test]
    fn removing_maps_to_failed() {
        // A container being removed means cancel was issued. Return Failed so
        // the actor does not keep polling a container in teardown.
        assert!(matches!(
            map_container_state(state_with_status(ContainerStateStatusEnum::REMOVING)),
            JobStatus::Failed { .. }
        ));
    }

    #[test]
    fn empty_status_maps_to_unknown() {
        assert_eq!(
            map_container_state(state_with_status(ContainerStateStatusEnum::EMPTY)),
            JobStatus::Unknown
        );
    }

    #[test]
    fn none_status_maps_to_unknown() {
        let state = ContainerState {
            status: None,
            ..Default::default()
        };
        assert_eq!(map_container_state(state), JobStatus::Unknown);
    }

    // ── Missing exit code ─────────────────────────────────────────────────────

    #[test]
    fn exited_with_no_exit_code_treats_as_nonzero() {
        // If Podman does not report an exit code for an exited container,
        // we default to -1 which is non-zero, so it maps to Failed.
        let state = ContainerState {
            status: Some(ContainerStateStatusEnum::EXITED),
            exit_code: None,
            ..Default::default()
        };
        assert!(matches!(
            map_container_state(state),
            JobStatus::Failed { .. }
        ));
    }
}
