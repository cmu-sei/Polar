/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use async_trait::async_trait;
use kube::{Client, Config};
use orchestrator_core::backend::{
    BackendJobHandle, BuildBackend, JobGraphIdentity, JobStatus, LogStream, Retention, SubmittedJob,
};
use orchestrator_core::error::BackendError;
use orchestrator_core::types::{BuildSpec, SourceRef};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{error, info, instrument};

use crate::job::{
    JobOptions, build_job_manifest_as, choose_account, expired, git_secret_name_for_build,
    job_name_for_build,
};

pub struct KubernetesBackend {
    client: Client,
    /// Deployment-level job settings: namespace, CA bundle, resources.
    options: JobOptions,
    /// Held from choosing an account to the Job existing, so two builds submitted together cannot be given the
    /// same one. What holds an account is the Jobs in the cluster, which also survive this process.
    accounts: tokio::sync::Mutex<()>,
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
        Ok(Self {
            client,
            options,
            accounts: tokio::sync::Mutex::new(()),
        })
    }
}

#[async_trait]
impl BuildBackend for KubernetesBackend {
    #[instrument(skip(self, spec), fields(build_id = %spec.build_id))]
    async fn submit(&self, spec: &BuildSpec) -> Result<SubmittedJob, BackendError> {
        use k8s_openapi::ByteString;
        use k8s_openapi::api::batch::v1::Job;
        use k8s_openapi::api::core::v1::Secret;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
        use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
        use std::collections::BTreeMap;

        let job_name = job_name_for_build(spec.build_id);
        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.options.namespace);

        // The pod runs under the one ServiceAccount of a multi-instance workload, or under one of a fixed set the
        // deployment made, the first no running job holds (the broker allows one session per identity there). The
        // jobs the cluster has say which are held, so only the fixed set needs them listed.
        let _account_choice = self.accounts.lock().await;
        let exclusive = self.options.identity.is_some() && self.options.shared_account.is_none();
        let running = if exclusive {
            jobs.list(&ListParams::default())
                .await
                .map_err(|e| {
                    BackendError::SubmissionFailed(format!(
                        "could not list the jobs to find a free account: {e}"
                    ))
                })?
                .items
        } else {
            Vec::new()
        };
        let account = choose_account(&self.options, &running).map_err(|held| {
            BackendError::SubmissionFailed(format!(
                "every one of the {held} ServiceAccounts for jobs is held by a running job"
            ))
        })?;
        let manifest = build_job_manifest_as(spec, &self.options, account.as_deref());

        tracing::debug!(
            "Deploying job manifest \n {}",
            k8s_openapi::serde_json::to_string_pretty(&manifest).unwrap()
        );

        // TODO: We might want to use the final job manifest later, so maybe we Write manifest back to the supervisor ?

        // A git credential is a Secret of the build's own, made before the job so the pod does not wait for it.
        let secrets: Api<Secret> = Api::namespaced(self.client.clone(), &self.options.namespace);
        let git_secret = match (&spec.git_secret, &spec.source) {
            (Some(credential), SourceRef::Git { .. }) => {
                let name = git_secret_name_for_build(spec.build_id);
                let secret = Secret {
                    metadata: ObjectMeta {
                        name: Some(name.clone()),
                        labels: Some(BTreeMap::from([(
                            "cyclops/build-id".to_string(),
                            spec.build_id.to_string(),
                        )])),
                        ..Default::default()
                    },
                    type_: Some("Opaque".to_string()),
                    data: Some(BTreeMap::from([
                        (
                            "username".to_string(),
                            ByteString(credential.username.clone().into_bytes()),
                        ),
                        (
                            "password".to_string(),
                            ByteString(credential.password.clone().into_bytes()),
                        ),
                    ])),
                    ..Default::default()
                };
                if let Err(e) = secrets.create(&PostParams::default(), &secret).await {
                    // The error is the API server's about the object, which does not repeat its data.
                    return Err(BackendError::SubmissionFailed(format!(
                        "could not make the build's git credential Secret: {e}"
                    )));
                }
                Some(name)
            }
            _ => None,
        };

        let job = match jobs.create(&PostParams::default(), &manifest).await {
            Ok(job) => job,
            Err(e) => {
                if let Some(name) = &git_secret {
                    let _ = secrets.delete(name, &DeleteParams::default()).await;
                }
                return Err(BackendError::SubmissionFailed(e.to_string()));
            }
        };

        // The Secret goes with the job: nothing outlives the build.
        if let (Some(name), Some(job_uid), Some(job_name)) =
            (&git_secret, &job.metadata.uid, &job.metadata.name)
        {
            let owner = serde_json::json!({"metadata": {"ownerReferences": [{
                "apiVersion": "batch/v1", "kind": "Job", "name": job_name, "uid": job_uid
            }]}});
            if let Err(e) = secrets
                .patch(name, &PatchParams::default(), &Patch::Merge(&owner))
                .await
            {
                // A credential left behind is worse than a build that fails: take it away now.
                tracing::error!(error = %e, secret = %name, "the build's credential could not be given to its job; deleting it");
                let _ = secrets.delete(name, &DeleteParams::default()).await;
            }
        }

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
            account = ?account,
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

    #[instrument(skip(self))]
    async fn reap(&self, retention: &Retention) -> Result<Vec<String>, BackendError> {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::{Api, DeleteParams, ListParams, Preconditions};

        let jobs: Api<Job> = Api::namespaced(self.client.clone(), &self.options.namespace);
        // Only this orchestrator's own jobs: whatever else is in the namespace is not its to remove.
        let listed = jobs
            .list(&ListParams::default().labels("app.kubernetes.io/managed-by=build-orchestrator"))
            .await
            .map_err(|e| {
                BackendError::Unreachable(format!(
                    "could not list the jobs to remove the old ones: {e}"
                ))
            })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or_default();

        let mut removed = Vec::new();
        for job in expired(&listed.items, now, retention) {
            let (Some(name), Some(uid)) = (&job.metadata.name, &job.metadata.uid) else {
                continue;
            };
            // Background, so that the Job's pods go with it (a Job deleted through the API alone leaves them), and
            // the uid, so that a Job made again under the same name is not the one removed.
            let params = DeleteParams {
                preconditions: Some(Preconditions {
                    uid: Some(uid.clone()),
                    resource_version: None,
                }),
                ..DeleteParams::background()
            };
            match jobs.delete(name, &params).await {
                Ok(_) => {
                    info!(job = %name, "removed a finished job that is past its retention");
                    removed.push(name.clone());
                }
                // Something else removed it first, which is the outcome wanted.
                Err(kube::Error::Api(status)) if status.code == 404 || status.code == 409 => {}
                Err(e) => {
                    tracing::warn!(job = %name, error = %e, "could not remove a finished job")
                }
            }
        }
        Ok(removed)
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
        let msg = "Error creating: pods \"build-x\" is forbidden: [maximum cpu usage per Container is 1, but limit is 2]".to_string();
        assert_eq!(permanent_refusal(std::slice::from_ref(&msg)), Some(msg));
    }

    #[test]
    fn quota_refusal_is_a_wait() {
        let msg = "Error creating: pods \"build-x\" is forbidden: exceeded quota: jobs-quota, requested: pods=1, used: pods=4, limited: pods=4".to_string();
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
