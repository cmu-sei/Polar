use async_trait::async_trait;
use kube::{Client, Config};
use orchestrator_core::backend::{
    BackendJobHandle, BuildBackend, JobGraphIdentity, JobStatus, LogStream, SubmittedJob,
};
use orchestrator_core::error::BackendError;
use orchestrator_core::types::BuildSpec;
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{error, info, instrument};

use crate::job::{JobOptions, build_job_manifest, job_name_for_build};

pub struct KubernetesBackend {
    client: Client,
    /// Deployment-level job settings: namespace, CA bundle, resources.
    options: JobOptions,
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
    pub async fn new(options: JobOptions) -> anyhow::Result<Self> {
        let client = {
            info!("Setting up kubernetes client");
            if let Some(config) = Self::get_kube_config().await {
                Client::try_from(config)?
            } else {
                Client::try_default().await?
            }
        };
        Ok(Self { client, options })
    }
}

#[async_trait]
impl BuildBackend for KubernetesBackend {
    #[instrument(skip(self, spec), fields(build_id = %spec.build_id))]
    async fn submit(&self, spec: &BuildSpec) -> Result<SubmittedJob, BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::{Api, PostParams};

        let job_name = job_name_for_build(spec.build_id);
        let manifest = build_job_manifest(spec, &self.options);

        tracing::debug!(
            "Deploying job manifest \n {}",
            k8s_openapi::serde_json::to_string_pretty(&manifest).unwrap()
        );

        // TODO: We might want to use the final job manifest later, so maybe we Write manifest back to the supervisor ?
        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.options.namespace);

        let job = jobs
            .create(&PostParams::default(), &manifest)
            .await
            .map_err(|e| BackendError::SubmissionFailed(e.to_string()))?;

        let uid = job
            .metadata
            .uid
            .ok_or_else(|| BackendError::SubmissionFailed("created job has no uid".into()))?;
        let name = job
            .metadata
            .name
            .ok_or_else(|| BackendError::SubmissionFailed("created job has no name".into()))?;

        tracing::info!(
            build_id = %spec.build_id,
            job_name = %job_name,
            namespace = %self.options.namespace,
            "pipeline Job created"
        );

        let handle = BackendJobHandle {
            name: name.clone(),
            uid,
        };

        let graph_identity = JobGraphIdentity {
            node_label: "KubernetesJob".into(),
            display_name: name,
            identity_props: vec![("uid".into(), handle.uid.clone())],
        };

        Ok(SubmittedJob {
            handle,
            graph_identity,
        })
    }

    #[instrument(skip(self), fields(handle = %handle))]
    async fn poll(&self, handle: &BackendJobHandle) -> Result<JobStatus, BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::Api;

        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.options.namespace);

        let job = match jobs.get_opt(&handle.name).await {
            Ok(Some(j)) => j,
            Ok(None) => return Ok(JobStatus::Unknown),
            Err(e) => return Err(BackendError::PollFailed(e.to_string())),
        };

        let status = interpret_job_status(&job);
        if status != JobStatus::Pending {
            return Ok(status);
        }

        // A Job whose pod is refused at admission (LimitRange, PodSecurity,
        // a missing Secret, ...) never leaves Pending: the Job controller just
        // retries creating the pod forever. Its FailedCreate events say why.
        // A full quota is the one refusal that is only a wait for room.
        match self.pod_creation_refusals(&handle.name).await {
            Ok(messages) => match permanent_refusal(&messages) {
                Some(reason) => {
                    tracing::error!(handle = %handle, reason = %reason, "Job's pod was refused; failing the build");
                    // Stop Kubernetes retrying a pod that can never be created.
                    if let Err(e) = self.cancel(handle).await {
                        tracing::warn!(handle = %handle, error = %e, "could not delete refused Job");
                    }
                    Ok(JobStatus::Failed {
                        reason: Some(format!("pod creation refused: {reason}")),
                    })
                }
                None => Ok(status),
            },
            // Not being able to read events is no reason to fail the build.
            Err(e) => {
                tracing::warn!(handle = %handle, error = %e, "could not read Job events");
                Ok(status)
            }
        }
    }

    #[instrument(skip(self), fields(handle = %handle))]
    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::{Api, DeleteParams, PropagationPolicy};

        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.options.namespace);

        let dp = DeleteParams {
            propagation_policy: Some(PropagationPolicy::Foreground),
            ..Default::default()
        };

        jobs.delete(&handle.name, &dp)
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
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &self.options.namespace);
        let lp = ListParams::default().labels(&format!("job-name={}", handle.name));

        let pod_list = pods
            .list(&lp)
            .await
            .map_err(|e| BackendError::LogStreamUnavailable(e.to_string()))?;

        let pod = pod_list.items.into_iter().next().ok_or_else(|| {
            BackendError::LogStreamUnavailable(format!("no pod found for job {}", handle.name))
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

impl KubernetesBackend {
    /// Messages of the Job's FailedCreate events, oldest first.
    async fn pod_creation_refusals(&self, job_name: &str) -> Result<Vec<String>, kube::Error> {
        use k8s_openapi::api::core::v1::Event;
        use kube::api::{Api, ListParams};

        let events: Api<Event> = Api::namespaced(self.client.clone(), &self.options.namespace);
        let selector =
            format!("involvedObject.kind=Job,involvedObject.name={job_name},reason=FailedCreate");
        let mut list = events
            .list(&ListParams::default().fields(&selector))
            .await?
            .items;
        list.sort_by_key(|e| e.last_timestamp.clone().map(|t| t.0));
        Ok(list.into_iter().filter_map(|e| e.message).collect())
    }
}

/// The reason a Job's pod can never be created, if the latest refusal is
/// permanent. A quota refusal ("exceeded quota") is not: the namespace is
/// full and the pod is created once something else finishes.
fn permanent_refusal(messages: &[String]) -> Option<String> {
    messages
        .last()
        .filter(|m| !m.contains("exceeded quota"))
        .cloned()
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

#[cfg(test)]
mod tests {
    use super::permanent_refusal;

    #[test]
    fn limit_range_refusal_is_permanent() {
        let msg = "Error creating: pods \"cyclops-build-x\" is forbidden: [maximum cpu usage per Container is 1, but limit is 2]".to_string();
        assert_eq!(permanent_refusal(&[msg.clone()]), Some(msg));
    }

    #[test]
    fn quota_refusal_is_a_wait() {
        let msg = "Error creating: pods \"cyclops-build-x\" is forbidden: exceeded quota: jobs-quota, requested: pods=1, used: pods=4, limited: pods=4".to_string();
        assert_eq!(permanent_refusal(&[msg]), None);
    }

    #[test]
    fn only_the_latest_refusal_counts() {
        let quota = "forbidden: exceeded quota: jobs-quota".to_string();
        let limit = "forbidden: maximum memory usage per Container is 2Gi".to_string();
        assert_eq!(permanent_refusal(&[limit.clone(), quota.clone()]), None);
        assert_eq!(permanent_refusal(&[quota, limit.clone()]), Some(limit));
        assert_eq!(permanent_refusal(&[]), None);
    }
}
