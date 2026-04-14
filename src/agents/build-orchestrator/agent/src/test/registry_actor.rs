/// Integration tests for BuildRegistryActor.
///
/// These tests spin up a real actor using ractor's runtime and drive it
/// with messages, verifying state mutations and RPC query responses.
/// No external I/O — the registry is a pure in-memory state machine.
/// Each test gets its own actor instance. ractor actors are lightweight
/// enough that spawning one per test is not a meaningful overhead.
#[cfg(test)]
mod registry_actor {
    use chrono::Utc;
    use ractor::Actor;
    use uuid::Uuid;

    use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
    use orchestrator_core::types::{BuildRequest, BuildState};

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

    async fn spawn_registry() -> ractor::ActorRef<RegistryMessage> {
        let (actor_ref, _handle) = Actor::spawn(None, BuildRegistryActor, ())
            .await
            .expect("registry actor failed to spawn");
        actor_ref
    }

    // ── Insert ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn inserted_record_is_retrievable_in_pending_state() {
        let registry = spawn_registry().await;
        let request = dummy_request();
        let build_id = request.build_id;

        registry
            .send_message(RegistryMessage::Insert(request))
            .unwrap();

        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed")
            .expect("record not found");

        assert_eq!(record.build_id, build_id);
        assert_eq!(record.state, BuildState::Pending);
    }

    #[tokio::test]
    async fn get_for_unknown_build_id_returns_none() {
        let registry = spawn_registry().await;

        let result =
            ractor::call!(registry, RegistryMessage::Get, Uuid::new_v4()).expect("RPC failed");

        assert!(result.is_none());
    }

    // ── Transition ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn valid_transition_updates_state() {
        let registry = spawn_registry().await;
        let request = dummy_request();
        let build_id = request.build_id;

        registry
            .send_message(RegistryMessage::Insert(request))
            .unwrap();

        registry
            .send_message(RegistryMessage::Transition {
                build_id,
                next_state: BuildState::ResolvingImage,
                backend_handle: None,
                resolved_image: None,
                failure_reason: None,
            })
            .unwrap();

        // Allow the actor to process the message.
        tokio::task::yield_now().await;

        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed")
            .expect("record not found");

        assert_eq!(record.state, BuildState::ResolvingImage);
    }

    #[tokio::test]
    async fn transition_sets_backend_handle_when_provided() {
        let registry = spawn_registry().await;
        let request = dummy_request();
        let build_id = request.build_id;

        registry
            .send_message(RegistryMessage::Insert(request))
            .unwrap();

        // Advance to a state where Scheduled is a valid next transition.
        registry
            .send_message(RegistryMessage::Transition {
                build_id,
                next_state: BuildState::ResolvingImage,
                backend_handle: None,
                resolved_image: None,
                failure_reason: None,
            })
            .unwrap();

        registry
            .send_message(RegistryMessage::Transition {
                build_id,
                next_state: BuildState::Scheduled,
                backend_handle: Some("cyclops-build-abc123".to_string()),
                resolved_image: Some(
                    "registry.example.com/builds/myapp@sha256:deadbeef".to_string(),
                ),
                failure_reason: None,
            })
            .unwrap();

        tokio::task::yield_now().await;

        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed")
            .expect("record not found");

        assert_eq!(record.state, BuildState::Scheduled);
        assert_eq!(
            record.backend_handle.as_deref(),
            Some("cyclops-build-abc123")
        );
        assert_eq!(
            record.resolved_image.as_deref(),
            Some("registry.example.com/builds/myapp@sha256:deadbeef")
        );
    }

    #[tokio::test]
    async fn transition_with_none_fields_does_not_overwrite_existing_values() {
        let registry = spawn_registry().await;
        let request = dummy_request();
        let build_id = request.build_id;

        registry
            .send_message(RegistryMessage::Insert(request))
            .unwrap();

        registry
            .send_message(RegistryMessage::Transition {
                build_id,
                next_state: BuildState::ResolvingImage,
                backend_handle: None,
                resolved_image: None,
                failure_reason: None,
            })
            .unwrap();

        registry
            .send_message(RegistryMessage::Transition {
                build_id,
                next_state: BuildState::Scheduled,
                backend_handle: Some("cyclops-build-abc123".to_string()),
                resolved_image: Some(
                    "registry.example.com/builds/myapp@sha256:deadbeef".to_string(),
                ),
                failure_reason: None,
            })
            .unwrap();

        // Transition to Running without providing handle or image — existing values
        // should be preserved.
        registry
            .send_message(RegistryMessage::Transition {
                build_id,
                next_state: BuildState::Running,
                backend_handle: None,
                resolved_image: None,
                failure_reason: None,
            })
            .unwrap();

        tokio::task::yield_now().await;

        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed")
            .expect("record not found");

        assert_eq!(record.state, BuildState::Running);
        // Prior values must survive.
        assert_eq!(
            record.backend_handle.as_deref(),
            Some("cyclops-build-abc123")
        );
        assert_eq!(
            record.resolved_image.as_deref(),
            Some("registry.example.com/builds/myapp@sha256:deadbeef")
        );
    }

    #[tokio::test]
    async fn illegal_transition_is_ignored_and_does_not_crash_actor() {
        let registry = spawn_registry().await;
        let request = dummy_request();
        let build_id = request.build_id;

        registry
            .send_message(RegistryMessage::Insert(request))
            .unwrap();

        // Pending → Succeeded is not a valid transition.
        registry
            .send_message(RegistryMessage::Transition {
                build_id,
                next_state: BuildState::Succeeded,
                backend_handle: None,
                resolved_image: None,
                failure_reason: None,
            })
            .unwrap();

        tokio::task::yield_now().await;

        // Actor must still be alive and the state must be unchanged.
        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed — actor may have crashed")
            .expect("record not found");

        assert_eq!(record.state, BuildState::Pending);
    }

    #[tokio::test]
    async fn transition_for_unknown_build_id_does_not_crash_actor() {
        let registry = spawn_registry().await;

        // Send a transition for a build_id that was never inserted.
        registry
            .send_message(RegistryMessage::Transition {
                build_id: Uuid::new_v4(),
                next_state: BuildState::Running,
                backend_handle: None,
                resolved_image: None,
                failure_reason: None,
            })
            .unwrap();

        tokio::task::yield_now().await;

        // Verify the actor is still responsive.
        let result = ractor::call!(registry, RegistryMessage::Get, Uuid::new_v4())
            .expect("RPC failed — actor crashed on unknown build_id");

        assert!(result.is_none());
    }

    // ── ListByState ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_by_state_returns_matching_records_only() {
        let registry = spawn_registry().await;

        let req_a = dummy_request();
        let req_b = dummy_request();
        let req_c = dummy_request();
        let id_a = req_a.build_id;
        let id_b = req_b.build_id;

        registry
            .send_message(RegistryMessage::Insert(req_a))
            .unwrap();
        registry
            .send_message(RegistryMessage::Insert(req_b))
            .unwrap();
        registry
            .send_message(RegistryMessage::Insert(req_c))
            .unwrap();

        // Advance A and B to ResolvingImage, leave C in Pending.
        for id in [id_a, id_b] {
            registry
                .send_message(RegistryMessage::Transition {
                    build_id: id,
                    next_state: BuildState::ResolvingImage,
                    backend_handle: None,
                    resolved_image: None,
                    failure_reason: None,
                })
                .unwrap();
        }

        tokio::task::yield_now().await;

        let resolving = ractor::call!(
            registry,
            RegistryMessage::ListByState,
            BuildState::ResolvingImage
        )
        .expect("RPC failed");

        assert_eq!(resolving.len(), 2);

        let pending = ractor::call!(registry, RegistryMessage::ListByState, BuildState::Pending)
            .expect("RPC failed");

        assert_eq!(pending.len(), 1);
    }

    #[tokio::test]
    async fn list_by_state_returns_empty_vec_when_none_match() {
        let registry = spawn_registry().await;

        let result = ractor::call!(
            registry,
            RegistryMessage::ListByState,
            BuildState::Succeeded
        )
        .expect("RPC failed");

        assert!(result.is_empty());
    }
}
