//! Cyclops as an agent built on `polar-agent-builder`: the builder owns the broker connection, the
//! certificate lifecycle, health and shutdown; this agent keeps the supervisor of builds.

use crate::actors::supervisor::{OrchestratorSupervisor, SupervisorArguments};
use crate::config::OrchestratorConfig;
use crate::mappings::MAPPINGS;
use crate::settings::{CyclopsSettings, SECTION};
use orchestrator_backend_k8s::backend::KubernetesBackend;
use orchestrator_backend_k8s::job::{JobIdentity, JobOptions};
use orchestrator_core::backend::BuildBackend;
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use ractor::Actor;
use std::sync::Arc;
use std::time::Duration;

pub const AGENT_NAME: &str = "cyclops";
pub const SERVICE_NAME: &str = "polar.cyclops";

/// How long a drain may take. Jobs outlive the orchestrator, which finds them again by their labels, so
/// the drain is only to stop taking requests and let a build in the middle of being submitted finish.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(30);

pub struct Cyclops;

impl PolarAgent for Cyclops {
    const NAME: &'static str = AGENT_NAME;
    const PURPOSE: &'static str =
        "The authoritative builder: runs a repository's pipeline as a job against pinned source and reports what ran";
    // It also creates the jobs and, for each, a service account and a secret; the deployment's Role is what
    // says so. The capability is the token it asks the API server with.
    const NEEDS: &'static [Capability] = &[Capability::KubeApiRead];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static polar_env::Section] = &[&SECTION];
    const RUNTIME: &'static [&'static polar_agent_builder::RuntimeSection] = &[&MAPPINGS];
    const HELP: Option<&'static str> = Some(
        "Cyclops builds a repository when the git observer reports that a ref moved, or when a build request \
         arrives on polar.orchestrator.builds.requests, if the `mappings` runtime section (a signed document) \
         names the repository. Each build is a Kubernetes Job in the namespace of the configuration file; \
         what the job is given is decided here, from the section, and never from the request.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);

        // The Kubernetes client is made before the broker's, and rustls will not choose between the providers
        // the dependencies bring in. The fork's own client installs `ring`, so the same one.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let settings = CyclopsSettings::read(configured.config())
            .map_err(|e| format!("could not read Cyclops' settings: {e}"))?;
        let config = OrchestratorConfig::load_from(&settings.config_file)
            .map_err(|e| format!("could not load {}: {e}", settings.config_file))?;

        let backend: Arc<dyn BuildBackend> = match config.backend.driver.as_str() {
            "kubernetes" => {
                let k8s = config
                    .backend
                    .kubernetes
                    .as_ref()
                    .ok_or("the kubernetes backend is selected but not configured")?;
                let mut options = JobOptions::new(k8s.namespace.clone());
                options.ca_bundle_secret = k8s.ca_bundle_secret.clone();
                options.workspace_size_limit = k8s.workspace_size_limit.clone();
                options.image_pull_policy = k8s.image_pull_policy.clone();
                options.identity = k8s
                    .identity
                    .as_deref()
                    .map(JobIdentity::from_json)
                    .transpose()
                    .map_err(|e| format!("the jobs' identity in the configuration does not read: {e}"))?;
                if options.identity.is_none() {
                    tracing::warn!("jobs share one certificate: builds that overlap will supersede each other");
                }
                if let Some(r) = &k8s.resources {
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
                tracing::info!(resources = ?options.resources, namespace = %k8s.namespace, "job options");
                Arc::new(
                    KubernetesBackend::new(options)
                        .await
                        .map_err(|e| format!("the Kubernetes backend did not start: {e}"))?,
                )
            }
            other => return Err(format!("unsupported backend driver: {other}").into()),
        };

        crate::client::StorageClient::new(&config.storage).ensure_bucket().await;

        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            OrchestratorSupervisor,
            SupervisorArguments {
                backend,
                config: Arc::new(config),
                configured,
            },
        )
        .await
        .map_err(|e| format!("the orchestrator's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the orchestrator's supervisor task failed: {e}"))?;
        Err("the orchestrator's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(Cyclops);
}
