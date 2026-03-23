use async_trait::async_trait;
use kube::{Client, Config};
use orchestrator_core::backend::{BackendJobHandle, BuildBackend, JobStatus, LogStream};
use orchestrator_core::error::BackendError;
use orchestrator_core::types::{BootstrapSpec, BuildSpec};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{error, info, instrument};

use crate::job::{
    bootstrap_job_manifest, build_job_manifest, job_name_for_bootstrap, job_name_for_build,
};

pub struct KubernetesBackend {
    client: Client,
    namespace: String,
}

impl KubernetesBackend {
    #[instrument]
    /// Attempt to resolve kube configuration by trying to infer from the cluster environment OR infer from local environment settings
    /// If both fail, return NONE and handle
    pub async fn get_kube_config() -> Option<Config> {
        // detect deployed environment, otherwise, try to infer configuration from the environment
        if let Ok(kube_config) = kube::Config::incluster() {
            info!("Inferred kube configuration from pod environment...");
            Some(kube_config)
        } else if let Ok(kube_config) = kube::Config::infer().await {
            info!("Inferred kube configuration from local environment...");
            Some(kube_config)
        } else {
            error!("Failed to infer kubeernetes configuration!");
            None
        }
    }
    /// Construct a new KubernetesBackend.`
    ///
    pub async fn new(namespace: String) -> anyhow::Result<Self> {
        let client = {
            info!("Setting up kubernetes client");
            if let Some(config) = Self::get_kube_config().await {
                Client::try_from(config)?
            } else {
                Client::try_default().await?
            }
        };
        Ok(Self { client, namespace })
    }
}

#[async_trait]
impl BuildBackend for KubernetesBackend {
    #[instrument(skip(self, spec), fields(build_id = %spec.build_id))]
    async fn submit(&self, spec: &BuildSpec) -> Result<BackendJobHandle, BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::{Api, PostParams};

        let job_name = job_name_for_build(spec.build_id);
        let manifest = build_job_manifest(spec, &self.namespace);

        tracing::debug!(
            "Deploying job manifest \n {}",
            k8s_openapi::serde_json::to_string_pretty(&manifest).unwrap()
        );

        // TODO: We might want to use the final job manifest later, so maybe we Write manifest back to the supervisor ?
        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.namespace);

        jobs.create(&PostParams::default(), &manifest)
            .await
            .map_err(|e| BackendError::SubmissionFailed(e.to_string()))?;

        tracing::info!(
            build_id = %spec.build_id,
            job_name = %job_name,
            namespace = %self.namespace,
            "pipeline Job created"
        );

        Ok(BackendJobHandle(job_name))
    }

    #[instrument(skip(self, spec), fields(build_id = %spec.build_id))]
    async fn submit_bootstrap(
        &self,
        spec: &BootstrapSpec,
    ) -> Result<BackendJobHandle, BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::{Api, PostParams};

        let job_name = job_name_for_bootstrap(spec.build_id);
        let manifest = bootstrap_job_manifest(spec, &self.namespace);

        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.namespace);

        jobs.create(&PostParams::default(), &manifest)
            .await
            .map_err(|e| BackendError::SubmissionFailed(e.to_string()))?;

        tracing::info!(
            build_id = %spec.build_id,
            job_name = %job_name,
            namespace = %self.namespace,
            "bootstrap Job created"
        );

        Ok(BackendJobHandle(job_name))
    }

    #[instrument(skip(self), fields(handle = %handle))]
    async fn poll(&self, handle: &BackendJobHandle) -> Result<JobStatus, BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::Api;

        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.namespace);

        let job = match jobs.get_opt(&handle.0).await {
            Ok(Some(j)) => j,
            Ok(None) => return Ok(JobStatus::Unknown),
            Err(e) => return Err(BackendError::PollFailed(e.to_string())),
        };

        Ok(interpret_job_status(&job))
    }

    #[instrument(skip(self), fields(handle = %handle))]
    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::{Api, DeleteParams, PropagationPolicy};

        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.namespace);

        let dp = DeleteParams {
            propagation_policy: Some(PropagationPolicy::Foreground),
            ..Default::default()
        };

        jobs.delete(&handle.0, &dp)
            .await
            .map_err(|e| BackendError::CancellationFailed(e.to_string()))?;

        tracing::info!(handle = %handle, "kubernetes Job deleted for cancellation");
        Ok(())
    }

    async fn logs(&self, handle: &BackendJobHandle) -> Result<LogStream, BackendError> {
        use k8s_openapi::api::core::v1::Pod;
        use kube::api::{Api, ListParams, LogParams};

        // Jobs don't have a log API directly — we have to find the pod the Job
        // created and stream from it. The Job controller sets job-name label on
        // pods it owns.
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &self.namespace);
        let lp = ListParams::default().labels(&format!("job-name={}", handle.0));

        let pod_list = pods
            .list(&lp)
            .await
            .map_err(|e| BackendError::LogStreamUnavailable(e.to_string()))?;

        let pod = pod_list.items.into_iter().next().ok_or_else(|| {
            BackendError::LogStreamUnavailable(format!("no pod found for job {}", handle.0))
        })?;

        let pod_name = pod
            .metadata
            .name
            .ok_or_else(|| BackendError::LogStreamUnavailable("pod has no name".to_string()))?;

        let log_params = LogParams {
            follow: true,
            ..Default::default()
        };

        // log_stream returns impl AsyncBufRead, which satisfies AsyncRead already.
        // No stream adaptor needed — box it directly.
        let stream = pods
            .log_stream(&pod_name, &log_params)
            .await
            .map_err(|e| BackendError::LogStreamUnavailable(e.to_string()))?;
        let stream = stream.compat();
        Ok(Box::new(stream))
    }

    fn name(&self) -> &'static str {
        "kubernetes"
    }
}

/// Interpret the k8s Job status conditions into our JobStatus enum.
/// The k8s Job status model is unfortunately not a clean state machine —
/// conditions are a list of booleans that must be read carefully.
fn interpret_job_status(job: &k8s_openapi::api::batch::v1::Job) -> JobStatus {
    let status = match &job.status {
        Some(s) => s,
        None => return JobStatus::Pending,
    };

    // succeeded > 0 means at least one pod completed with exit 0.
    if status.succeeded.unwrap_or(0) > 0 {
        return JobStatus::Succeeded;
    }

    // failed > 0 and active == 0 means no more retries.
    if status.failed.unwrap_or(0) > 0 && status.active.unwrap_or(0) == 0 {
        let reason = status.conditions.as_ref().and_then(|conditions| {
            conditions
                .iter()
                .find(|c| c.type_ == "Failed")
                .and_then(|c| c.message.clone())
        });
        return JobStatus::Failed { reason };
    }

    // active > 0 means a pod is currently running.
    if status.active.unwrap_or(0) > 0 {
        return JobStatus::Running;
    }

    // start_time set but nothing else — still scheduling.
    if status.start_time.is_some() {
        return JobStatus::Pending;
    }

    JobStatus::Pending
}
