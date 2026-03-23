use std::sync::Arc;

use anyhow::Context;
use build_orchestrator::{
    actors::supervisor::{OrchestratorSupervisor, SupervisorArguments, SupervisorMessage},
    cassini::LoggingPublisher,
    config::OrchestratorConfig,
};
use orchestrator_backend_k8s::backend::KubernetesBackend;
use ractor::Actor;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Config is loaded first so log format/level is known before the subscriber
    // is initialized. Falls back to sensible defaults if no config is present.
    let config = OrchestratorConfig::load().context("failed to load orchestrator configuration")?;
    polar::init_logging("polar.build.orchestrator".to_string());
    // init_tracing(&config.log.format, &config.log.level);

    match rustls::crypto::aws_lc_rs::default_provider().install_default() {
        Ok(_) => tracing::warn!("Default crypto provider installed (this should only happen once)"),
        Err(_) => tracing::debug!("Crypto provider already configured"),
    }

    tracing::info!(
        backend = %config.backend.driver,
        broker = %config.cassini.broker_url,
        "cyclops orchestrator starting"
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
            other => anyhow::bail!("unsupported backend driver: {other}"),
        };

    // In v1 we use the logging publisher stub.
    // This is replaced by a real Cassini client actor ref once the integration
    // layer is wired in.
    let publisher = Arc::new(LoggingPublisher);

    let config = Arc::new(config);

    let args = SupervisorArguments {
        backend,
        publisher,
        config: Arc::clone(&config),
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

// fn init_tracing(format: &str, level: &str) {
//     let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

//     match format {
//         "json" => {
//             tracing_subscriber::fmt()
//                 .json()
//                 .with_env_filter(filter)
//                 .with_current_span(true)
//                 .init();
//         }
//         _ => {
//             tracing_subscriber::fmt()
//                 .pretty()
//                 .with_env_filter(filter)
//                 .init();
//         }
//     }
// }

/// Injects a synthetic BuildRequest when the CYCLOPS_DEV_MODE env var is set.
/// Lets you exercise the full actor path without a live Cassini broker.
async fn inject_test_build_if_dev(supervisor: &ractor::ActorRef<SupervisorMessage>) {
    if std::env::var("CYCLOPS_DEV_MODE").is_err() {
        return;
    }

    tracing::warn!("CYCLOPS_DEV_MODE is set — injecting synthetic build request");

    let request = orchestrator_core::types::BuildRequest {
        build_id: uuid::Uuid::new_v4(),
        repo_url: "https://github.com/cmu-sei/Polar.git".to_string(),
        commit_sha: "8b0214f218abb26cdfe2ade8f117abb0cf4b13e6".to_string(),
        requested_by: "dev-mode-injection".to_string(),
        requested_at: chrono::Utc::now(),
        target_registry: "registry.internal.example.com/builds".to_string(),
        metadata: std::collections::HashMap::new(),
    };

    if let Err(e) = supervisor.send_message(SupervisorMessage::BuildRequested(request)) {
        tracing::error!(error = %e, "failed to inject synthetic build request");
    }
}
