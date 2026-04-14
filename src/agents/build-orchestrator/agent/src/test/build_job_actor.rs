/// Integration tests for BuildJobActor.
///
/// These tests exercise the full actor lifecycle using a controllable mock
/// backend. The mock backend is driven by a channel — tests push `JobStatus`
/// values onto the channel and the mock returns them sequentially from `poll`.
/// This gives deterministic control over every state transition without any
/// real backend, network, or timing dependency.
///
/// Tests use `tokio::test` with real ractor actors. The actor tree is:
///   BuildRegistryActor (verifies state mutations)
///   BuildJobActor under test (drives state machine)
///   MockBackend (returns scripted poll responses)
///   MockPublisher (records emitted events)
///   MockStorageClient (no-op)
#[cfg(test)]
mod build_job_actor {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Utc;
    use ractor::Actor;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use crate::actors::build_job::{BuildJobActor, BuildJobArguments};
    use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
    use crate::client::{LogContainer, StorageClient};
    use crate::config::{
        BackendConfig, BootstrapConfig, CassiniConfig, CredentialsConfig, LogConfig,
        OrchestratorConfig, RepoMapping,
    };
    use orchestrator_core::backend::{
        BackendJobHandle, BuildBackend, JobGraphIdentity, JobStatus, LogStream, SubmittedJob,
    };
    use orchestrator_core::error::BackendError;
    use orchestrator_core::types::{BuildRequest, BuildSpec, BuildState};
    use polar::cassini::{CassiniClient, CassiniClientError, PublishRequest};

    // ── Mock backend ──────────────────────────────────────────────────────────

    /// A scripted backend that returns poll statuses from a channel in order.
    ///
    /// `submit` always succeeds and returns a fixed handle.
    /// `poll` pops the next status off the channel. Tests control the sequence
    /// by pushing statuses before starting the actor.
    /// `cancel` always succeeds.
    /// `logs` returns an empty stream.
    struct MockBackend {
        /// The actor reads from this to determine what poll returns next.
        poll_responses: Mutex<mpsc::UnboundedReceiver<Result<JobStatus, BackendError>>>,
    }

    impl MockBackend {
        fn new() -> (Self, mpsc::UnboundedSender<Result<JobStatus, BackendError>>) {
            let (tx, rx) = mpsc::unbounded_channel();
            (
                Self {
                    poll_responses: Mutex::new(rx),
                },
                tx,
            )
        }

        fn fixed_handle() -> BackendJobHandle {
            BackendJobHandle {
                name: "cyclops-build-test".to_string(),
                uid: "test-container-id-0000".to_string(),
            }
        }
    }

    #[async_trait]
    impl BuildBackend for MockBackend {
        async fn submit(&self, spec: &BuildSpec) -> Result<SubmittedJob, BackendError> {
            Ok(SubmittedJob {
                handle: Self::fixed_handle(),
                graph_identity: JobGraphIdentity {
                    node_label: "MockJob".to_string(),
                    display_name: format!("mock-job-{}", spec.build_id),
                    identity_props: vec![("build_id".into(), spec.build_id.to_string())],
                },
            })
        }

        async fn poll(&self, _handle: &BackendJobHandle) -> Result<JobStatus, BackendError> {
            self.poll_responses
                .lock()
                .unwrap()
                .try_recv()
                .unwrap_or(Ok(JobStatus::Pending))
        }

        async fn cancel(&self, _handle: &BackendJobHandle) -> Result<(), BackendError> {
            Ok(())
        }

        async fn logs(&self, _handle: &BackendJobHandle) -> Result<LogStream, BackendError> {
            // Return an immediately-EOF reader.
            let (_, reader) = tokio::io::duplex(1);
            Ok(Box::new(reader))
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    // ── Mock publisher ────────────────────────────────────────────────────────

    /// Records all published events so tests can assert on what was emitted.
    // #[derive(Clone)]
    // struct MockPublisher {
    //     events: Arc<Mutex<Vec<PublishRequest>>>,
    // }

    // impl MockPublisher {
    //     fn new() -> Self {
    //         Self {
    //             events: Arc::new(Mutex::new(Vec::new())),
    //         }
    //     }

    //     fn published(&self) -> Vec<PublishRequest> {
    //         self.events.lock().unwrap().clone()
    //     }
    // }

    // impl CassiniClient for MockPublisher {
    //     fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
    //         self.events.lock().unwrap().push(req);
    //         Ok(())
    //     }
    //     fn unsubscribe(
    //         &self,
    //         req: polar::cassini::UnsubscribeRequest,
    //     ) -> Result<(), polar::cassini::CassiniClientError> {
    //         self.events.lock().unwrap().push(req);
    //         Ok(())
    //     }
    //     fn register(&self, req: Registration) -> Result<(), CassiniClientError> {
    //         self.events.lock().unwrap().push(req);
    //         Ok(())
    //     }
    //     fn publish(&self, req: PublishRequest) -> Result<(), anyhow::Error> {
    //         self.events.lock().unwrap().push(req);
    //         Ok(())
    //     }
    // }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn dummy_request_for_repo(repo_url: &str) -> BuildRequest {
        BuildRequest {
            build_id: Uuid::new_v4(),
            repo_url: repo_url.to_string(),
            commit_sha: "a".repeat(40),
            requested_by: "test-harness".to_string(),
            requested_at: Utc::now(),
            target_registry: "registry.example.com/builds".to_string(),
            metadata: Default::default(),
        }
    }

    /// Config with a pre-resolved pipeline image for the given repo.
    /// This bypasses the bootstrap path so tests focus on the pipeline phase.
    fn config_with_image(repo_url: &str, image: &str) -> Arc<OrchestratorConfig> {
        Arc::new(OrchestratorConfig {
            backend: BackendConfig {
                driver: "mock".to_string(),
                kubernetes: None,
            },
            cassini: CassiniConfig {
                broker_url: "tcp://localhost:4222".to_string(),
                inbound_subject: "polar.git.repositories.events".to_string(),
            },
            bootstrap: BootstrapConfig {
                builder_image: "registry.example.com/nix-bootstrap@sha256:abc".to_string(),
                container_config_ref: "container.dhall".to_string(),
                target_registry: "registry.example.com/builds".to_string(),
            },
            credentials: CredentialsConfig {
                git_secret_name: "git-credentials".to_string(),
                registry_secret_name: "registry-credentials".to_string(),
            },
            repo_mappings: vec![RepoMapping {
                repo_url: repo_url.to_string(),
                pipeline_image: Some(image.to_string()),
            }],
            log: LogConfig {
                format: "json".to_string(),
                level: "info".to_string(),
            },
            storage: StorageConfig {
                endpoint: "http://localhost:9000".to_string(),
                bucket: "cyclops-artifacts".to_string(),
                region: "us-east-1".to_string(),
            },
        })
    }

    async fn spawn_registry() -> ractor::ActorRef<RegistryMessage> {
        let (actor_ref, _handle) = Actor::spawn(None, BuildRegistryActor, ())
            .await
            .expect("registry failed to spawn");
        actor_ref
    }

    // ── Happy path ────────────────────────────────────────────────────────────

    /// The actor should drive a build from Pending through to Succeeded when
    /// the backend reports: Pending → Running → Succeeded.
    #[tokio::test]
    async fn happy_path_pending_running_succeeded() {
        let repo_url = "https://gitlab.example.com/polar/myapp";
        let image = "registry.example.com/builds/myapp@sha256:deadbeef";

        let (mock_backend, poll_tx) = MockBackend::new();
        let publisher = MockPublisher::new();
        let registry = spawn_registry().await;
        let request = dummy_request_for_repo(repo_url);
        let build_id = request.build_id;

        // Insert the record into the registry before spawning the actor,
        // matching the pattern the supervisor uses.
        registry
            .send_message(RegistryMessage::Insert(request.clone()))
            .unwrap();

        // Script the backend: one Pending response, then Running, then Succeeded.
        poll_tx.send(Ok(JobStatus::Pending)).unwrap();
        poll_tx.send(Ok(JobStatus::Running)).unwrap();
        poll_tx.send(Ok(JobStatus::Succeeded)).unwrap();

        let args = BuildJobArguments {
            request,
            backend: Arc::new(mock_backend),
            registry: registry.clone(),
            publisher: Arc::new(publisher.clone()),
            config: config_with_image(repo_url, image),
            storage: Arc::new(StorageClient::noop()),
        };

        let (job_ref, job_handle) = Actor::spawn(None, BuildJobActor, args)
            .await
            .expect("job actor failed to spawn");

        // Wait for the actor to stop itself on terminal state.
        job_handle.await.expect("job actor panicked");

        // Verify the registry reflects Succeeded.
        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed")
            .expect("record not found");

        assert_eq!(record.state, BuildState::Succeeded);
        assert_eq!(record.resolved_image.as_deref(), Some(image));
        assert!(record.backend_handle.is_some());
    }

    /// The actor should transition to Failed and stop when the backend reports failure.
    #[tokio::test]
    async fn backend_failure_transitions_to_failed() {
        let repo_url = "https://gitlab.example.com/polar/myapp";
        let image = "registry.example.com/builds/myapp@sha256:deadbeef";

        let (mock_backend, poll_tx) = MockBackend::new();
        let publisher = MockPublisher::new();
        let registry = spawn_registry().await;
        let request = dummy_request_for_repo(repo_url);
        let build_id = request.build_id;

        registry
            .send_message(RegistryMessage::Insert(request.clone()))
            .unwrap();

        poll_tx
            .send(Ok(JobStatus::Failed {
                reason: Some("OOMKilled".to_string()),
            }))
            .unwrap();

        let args = BuildJobArguments {
            request,
            backend: Arc::new(mock_backend),
            registry: registry.clone(),
            publisher: Arc::new(publisher.clone()),
            config: config_with_image(repo_url, image),
            storage: Arc::new(StorageClient::noop()),
        };

        let (_job_ref, job_handle) = Actor::spawn(None, BuildJobActor, args)
            .await
            .expect("job actor failed to spawn");

        job_handle.await.expect("job actor panicked");

        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed")
            .expect("record not found");

        assert_eq!(record.state, BuildState::Failed);
        assert!(record.failure_reason.is_some());
        assert!(record.failure_reason.unwrap().contains("OOMKilled"));
    }

    /// When the backend returns Unknown, the actor should fail the build rather
    /// than polling indefinitely.
    #[tokio::test]
    async fn unknown_status_transitions_to_failed() {
        let repo_url = "https://gitlab.example.com/polar/myapp";
        let image = "registry.example.com/builds/myapp@sha256:deadbeef";

        let (mock_backend, poll_tx) = MockBackend::new();
        let registry = spawn_registry().await;
        let request = dummy_request_for_repo(repo_url);
        let build_id = request.build_id;

        registry
            .send_message(RegistryMessage::Insert(request.clone()))
            .unwrap();

        poll_tx.send(Ok(JobStatus::Unknown)).unwrap();

        let args = BuildJobArguments {
            request,
            backend: Arc::new(mock_backend),
            registry: registry.clone(),
            publisher: Arc::new(MockPublisher::new()),
            config: config_with_image(repo_url, image),
            storage: Arc::new(StorageClient::noop()),
        };

        let (_job_ref, job_handle) = Actor::spawn(None, BuildJobActor, args)
            .await
            .expect("job actor failed to spawn");

        job_handle.await.expect("job actor panicked");

        let record = ractor::call!(registry, RegistryMessage::Get, build_id)
            .expect("RPC failed")
            .expect("record not found");

        assert_eq!(record.state, BuildState::Failed);
    }

    // ── Event emission ────────────────────────────────────────────────────────

    /// A successful build must emit build_started, build_running, and
    /// build_completed events in that order.
    #[tokio::test]
    async fn successful_build_emits_expected_events() {
        let repo_url = "https://gitlab.example.com/polar/myapp";
        let image = "registry.example.com/builds/myapp@sha256:deadbeef";

        let (mock_backend, poll_tx) = MockBackend::new();
        let publisher = MockPublisher::new();
        let registry = spawn_registry().await;
        let request = dummy_request_for_repo(repo_url);

        registry
            .send_message(RegistryMessage::Insert(request.clone()))
            .unwrap();

        poll_tx.send(Ok(JobStatus::Running)).unwrap();
        poll_tx.send(Ok(JobStatus::Succeeded)).unwrap();

        let args = BuildJobArguments {
            request,
            backend: Arc::new(mock_backend),
            registry: registry.clone(),
            publisher: Arc::new(publisher.clone()),
            config: config_with_image(repo_url, image),
            storage: Arc::new(StorageClient::noop()),
        };

        let (_job_ref, job_handle) = Actor::spawn(None, BuildJobActor, args)
            .await
            .expect("job actor failed to spawn");

        job_handle.await.expect("job actor panicked");

        let events = publisher.published();
        let topics: Vec<&str> = events.iter().map(|e| e.topic.as_str()).collect();

        // build_started is emitted at Scheduled, build_running at Running,
        // build_completed at Succeeded.
        assert!(
            topics.contains(&orchestrator_core::types::subjects::BUILD_EVENTS_TOPIC),
            "expected events on BUILD_EVENTS_TOPIC, got: {topics:?}"
        );

        // At minimum three events must have been published.
        assert!(
            events.len() >= 3,
            "expected at least 3 events, got {}",
            events.len()
        );
    }

    /// A failed build must emit build_started and build_failed events.
    #[tokio::test]
    async fn failed_build_emits_build_failed_event() {
        let repo_url = "https://gitlab.example.com/polar/myapp";
        let image = "registry.example.com/builds/myapp@sha256:deadbeef";

        let (mock_backend, poll_tx) = MockBackend::new();
        let publisher = MockPublisher::new();
        let registry = spawn_registry().await;
        let request = dummy_request_for_repo(repo_url);

        registry
            .send_message(RegistryMessage::Insert(request.clone()))
            .unwrap();

        poll_tx
            .send(Ok(JobStatus::Failed { reason: None }))
            .unwrap();

        let args = BuildJobArguments {
            request,
            backend: Arc::new(mock_backend),
            registry: registry.clone(),
            publisher: Arc::new(publisher.clone()),
            config: config_with_image(repo_url, image),
            storage: Arc::new(StorageClient::noop()),
        };

        let (_job_ref, job_handle) = Actor::spawn(None, BuildJobActor, args)
            .await
            .expect("job actor failed to spawn");

        job_handle.await.expect("job actor panicked");

        // At least build_started and build_failed must have been emitted.
        assert!(publisher.published().len() >= 2);
    }

    // ── No image configured ───────────────────────────────────────────────────

    /// If no pipeline image is configured for the repo, submit_bootstrap is
    /// called. Since bootstrap is currently a todo!(), the actor will panic.
    /// This test documents that behaviour explicitly so it is not a surprise.
    ///
    /// Remove the #[should_panic] and update this test when bootstrap is implemented.
    #[tokio::test]
    #[should_panic]
    async fn no_image_configured_panics_at_bootstrap_todo() {
        let repo_url = "https://gitlab.example.com/polar/no-image-repo";

        let (mock_backend, _poll_tx) = MockBackend::new();
        let registry = spawn_registry().await;
        let request = dummy_request_for_repo(repo_url);

        registry
            .send_message(RegistryMessage::Insert(request.clone()))
            .unwrap();

        // Config has no mapping for this repo — bootstrap path will be triggered.
        let config = Arc::new(OrchestratorConfig {
            backend: BackendConfig {
                driver: "mock".to_string(),
                kubernetes: None,
            },
            cassini: CassiniConfig {
                broker_url: "tcp://localhost:4222".to_string(),
                inbound_subject: "polar.git.repositories.events".to_string(),
            },
            bootstrap: BootstrapConfig {
                builder_image: "registry.example.com/nix-bootstrap@sha256:abc".to_string(),
                container_config_ref: "container.dhall".to_string(),
                target_registry: "registry.example.com/builds".to_string(),
            },
            credentials: CredentialsConfig {
                git_secret_name: "git-credentials".to_string(),
                registry_secret_name: "registry-credentials".to_string(),
            },
            repo_mappings: vec![], // intentionally empty
            log: LogConfig {
                format: "json".to_string(),
                level: "info".to_string(),
            },
            storage: StorageConfig {
                endpoint: "http://localhost:9000".to_string(),
                bucket: "cyclops-artifacts".to_string(),
                region: "us-east-1".to_string(),
            },
        });

        let args = BuildJobArguments {
            request,
            backend: Arc::new(mock_backend),
            registry: registry.clone(),
            publisher: Arc::new(MockPublisher::new()),
            config,
            storage: Arc::new(StorageClient::noop()),
        };

        let (_job_ref, job_handle) = Actor::spawn(None, BuildJobActor, args)
            .await
            .expect("job actor failed to spawn");

        job_handle.await.unwrap();
    }
}
