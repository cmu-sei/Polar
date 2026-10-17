use std::sync::Arc;

use anyhow::Context;
use build_orchestrator::{
    actors::supervisor::{OrchestratorSupervisor, SupervisorArguments},
    config::OrchestratorConfig,
};
use cassini_types::ClientEvent;
use git_agent_common::GitRepositoryMessage;
use orchestrator_backend_k8s::backend::KubernetesBackend;
use orchestrator_backend_k8s::job::JobOptions;
use orchestrator_backend_podman::backend::PodmanBackend;
use polar::graph::nodes::git::RepoId;
use polar::topics::GIT_REPOSITORY_EVENTS;
use polar::{RkyvError, SupervisorMessage};
use ractor::Actor;
use rkyv::to_bytes;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Config is loaded first so log format/level is known before the subscriber
    // is initialized. Falls back to sensible defaults if no config is present.
    let config = OrchestratorConfig::load().context("failed to load orchestrator configuration")?;
    let dev_repo_url = config.repo_mappings.first().map(|m| m.repo_url.clone());
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
    let backend: Arc<dyn orchestrator_core::backend::BuildBackend> = match config
        .backend
        .driver
        .as_str()
    {
        "kubernetes" => {
            let k8s_config = config
                .backend
                .kubernetes
                .as_ref()
                .context("kubernetes backend config missing")?;

            let mut options = JobOptions::new(k8s_config.namespace.clone());
            options.ca_bundle_secret = k8s_config.ca_bundle_secret.clone();
            options.workspace_size_limit = k8s_config.workspace_size_limit.clone();
            if let Some(r) = &k8s_config.resources {
                let d = &mut options.resources;
                for (field, value) in [
                    (&mut d.cpu_limit, &r.cpu_limit),
                    (&mut d.memory_limit, &r.memory_limit),
                    (&mut d.cpu_request, &r.cpu_request),
                    (&mut d.memory_request, &r.memory_request),
                ] {
                    if let Some(v) = value {
                        *field = v.clone();
                    }
                }
            }
            tracing::info!(resources = ?options.resources, workspace_size_limit = ?options.workspace_size_limit, "job resources");

            let backend = KubernetesBackend::new(options)
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
    inject_test_build_if_dev(&supervisor, &dev_repo_url).await;

    // Block until the supervisor exits (which in normal operation means forever,
    // or until a fatal error propagates up the supervision tree).
    let _ = handle.await;

    tracing::info!("orchestrator supervisor exited — shutting down");
    Ok(())
}

/// Injects a synthetic git ref update when the CYCLOPS_DEV_MODE env var is set.
/// Lets you exercise the full actor path without a live Cassini broker. The
/// repo is the first configured mapping, so the update is always buildable.
async fn inject_test_build_if_dev(
    supervisor: &ractor::ActorRef<SupervisorMessage>,
    repo_url: &Option<String>,
) {
    if std::env::var("CYCLOPS_DEV_MODE").is_err() {
        return;
    }
    let Some(repo_url) = repo_url else {
        tracing::warn!(
            "CYCLOPS_DEV_MODE is set but no repo mappings are configured — nothing to inject"
        );
        return;
    };

    tracing::warn!("CYCLOPS_DEV_MODE is set — injecting synthetic ref update for {repo_url}");

    let event = GitRepositoryMessage::RefUpdated {
        repo: RepoId::from_url(repo_url),
        ref_name: "refs/heads/main".to_string(),
        old: None,
        new: "8b0214f218abb26cdfe2ade8f117abb0cf4b13e6".to_string(),
        observed_at: chrono::Utc::now().timestamp_millis().to_string(),
    };

    let payload = to_bytes::<RkyvError>(&event).unwrap().to_vec();

    let c_event = ClientEvent::MessagePublished {
        topic: GIT_REPOSITORY_EVENTS.to_string(),
        payload,
        trace_ctx: None,
    };

    if let Err(e) = supervisor.send_message(SupervisorMessage::ClientEvent { event: c_event }) {
        tracing::error!(error = %e, "failed to inject synthetic build request");
    }
}
