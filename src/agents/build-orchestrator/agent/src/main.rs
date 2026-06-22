use std::sync::Arc;

use anyhow::Context;
use build_orchestrator::{
    actors::supervisor::{OrchestratorSupervisor, SupervisorArguments},
    config::OrchestratorConfig,
};
use cassini_types::ClientEvent;
use orchestrator_backend_k8s::backend::KubernetesBackend;
use orchestrator_backend_podman::backend::PodmanBackend;
use polar::{GitRepositoryUpdatedEvent, RkyvError, SupervisorMessage};
use ractor::Actor;
use rkyv::to_bytes;
use uuid::Uuid;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Config is loaded first so log format/level is known before the subscriber
    // is initialized. Falls back to sensible defaults if no config is present.
    let config = OrchestratorConfig::load().context("failed to load orchestrator configuration")?;
    polar::init_logging("polar.build.orchestrator".to_string());

    // TODO: Make configurable, we might eventually need FIPS compliance
    match rustls::crypto::aws_lc_rs::default_provider().install_default() {
        Ok(_) => tracing::warn!("Default crypto provider installed (this should only happen once)"),
        Err(_) => tracing::debug!("Crypto provider already configured"),
    }

    tracing::info!(
        backend = %config.backend.driver,
        "build orchestrator starting"
    );

    // Build the backend.
    let backend: Arc<dyn orchestrator_core::backend::BuildBackend> =
        match config.backend.driver.as_str() {
            "kubernetes" => {
                let k8s_config = config
                    .backend
                    .kubernetes
                    .as_ref()
                    .context("kubernetes backend config missing")?;

                let backend = KubernetesBackend::new(k8s_config.namespace.clone())
                    .await
                    .context("failed to initialize Kubernetes backend")?;

                tracing::info!(namespace = %k8s_config.namespace, "Kubernetes backend initialized");
                Arc::new(backend)
            }
            "podman" => {
                let p_config = config
                    .backend
                    .podman
                    .as_ref()
                    .context("Podman backend missing!")?;

                let backend = PodmanBackend::connect(p_config.to_owned())
                    .expect("Expected to connect to podman socket");

                assert!(backend.ping().await.is_ok());

                tracing::info!("Podman backend initialized");
                Arc::new(backend)
            }
            other => anyhow::bail!("unsupported backend driver: {other}"),
        };

    let args = SupervisorArguments {
        backend,
        config: Arc::new(config),
    };

    let (supervisor, handle) = Actor::spawn(
        Some("orchestrator-supervisor".to_string()),
        OrchestratorSupervisor,
        args,
    )
    .await
    .context("failed to spawn OrchestratorSupervisor")?;

    tracing::info!("actor tree initialized — waiting for build requests");

    // TODO: wire up the Cassini consumer here. For now we send a synthetic
    // BuildRequest so the supervisor can be exercised end-to-end without a
    // live broker connection.
    inject_test_build_if_dev(&supervisor).await;

    // Block until the supervisor exits (which in normal operation means forever,
    // or until a fatal error propagates up the supervision tree).
    let _ = handle.await;

    tracing::info!("orchestrator supervisor exited — shutting down");
    Ok(())
}

/// Injects a synthetic BuildRequest when the CYCLOPS_DEV_MODE env var is set.
/// Lets you exercise the full actor path without a live Cassini broker.
async fn inject_test_build_if_dev(supervisor: &ractor::ActorRef<SupervisorMessage>) {
    if std::env::var("CYCLOPS_DEV_MODE").is_err() {
        return;
    }

    tracing::warn!("CYCLOPS_DEV_MODE is set — injecting synthetic build request");

    let event = GitRepositoryUpdatedEvent {
        event_id: Uuid::new_v4().to_string(),
        http_url: Some("https://github.com/cmu-sei/Polar.git".to_string()),
        commit_sha: "8b0214f218abb26cdfe2ade8f117abb0cf4b13e6".to_string(),
        observed_at: chrono::Utc::now().timestamp(),
        ..Default::default()
    };

    let payload = to_bytes::<RkyvError>(&event).unwrap().to_vec();

    let c_event = ClientEvent::MessagePublished {
        topic: String::default(),
        payload,
        trace_ctx: None,
    };

    if let Err(e) = supervisor.send_message(SupervisorMessage::ClientEvent { event: c_event }) {
        tracing::error!(error = %e, "failed to inject synthetic build request");
    }
}
