//! The kube-rs loop: observe one agent's pods, ask [`decide`], write the status, act.
//!
//! Status is written before acting, so an ending is on record before its pod is removed,
//! and a start is counted before its pod exists (a pod name is never reused). A create
//! that finds its pod already there means another pass got there first; it is not an error.
//!
//! Each fact the pass produced is then published as a lifecycle event (see `events`).

use crate::crd::PolarAgent;
use crate::events::{self, Publisher};
use crate::lifecycle::{Action as LifecycleAction, Desired, Policy, decide};
use crate::{AGENT_LABEL, MANAGER, pod};
use futures::StreamExt;
use k8s_openapi::api::core::v1::{Namespace, Pod, ServiceAccount};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Api, Client, Resource, ResourceExt};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kubernetes: {0}")]
    Kube(#[from] kube::Error),
    #[error("{0}")]
    Other(String),
}

pub struct Context {
    pub client: Client,
    pub policy: Policy,
    /// The cluster's identity (the kube-system namespace's UID, issue #236), as the kube
    /// agents scope everything they write, so the operator's events meet their nodes.
    pub cluster_uid: String,
    pub publisher: Publisher,
    pub declared: Declared,
}

/// The spec hash last declared for each agent (by resource UID), so that each agent is
/// declared once per operator start, again on every change, and again after
/// [`Declared::forget`] (a broker reconnect).
#[derive(Clone, Default)]
pub struct Declared(Arc<Mutex<HashMap<String, String>>>);

impl Declared {
    /// True when `hash` was not the last one declared for `uid`; records it.
    fn record(&self, uid: &str, hash: &str) -> bool {
        let mut map = self.0.lock().expect("declared lock poisoned");
        map.insert(uid.to_string(), hash.to_string()).as_deref() != Some(hash)
    }

    pub fn forget(&self) {
        self.0.lock().expect("declared lock poisoned").clear();
    }
}

/// How often an agent is looked at with nothing to wake it: a resync, so that a
/// declaration forgotten on reconnect is sent again before long.
const RESYNC: Duration = Duration::from_secs(300);

/// The UID of the cluster's kube-system namespace: the cluster's identity in the graph.
pub async fn cluster_uid(client: &Client) -> anyhow::Result<String> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .get("kube-system")
        .await?
        .metadata
        .uid
        .ok_or_else(|| anyhow::anyhow!("the kube-system namespace has no uid"))
}

/// Runs until the process is told to stop.
pub async fn run(
    client: Client,
    policy: Policy,
    publisher: Publisher,
    declared: Declared,
) -> anyhow::Result<()> {
    let agents: Api<PolarAgent> = Api::all(client.clone());
    let pods: Api<Pod> = Api::all(client.clone());
    // Fails early, and clearly, when the CRD is not installed.
    agents
        .list(&ListParams::default().limit(1))
        .await
        .map_err(|e| anyhow::anyhow!("cannot list PolarAgents (is the CRD installed?): {e}"))?;

    let cluster_uid = cluster_uid(&client).await?;
    let context = Arc::new(Context {
        client,
        policy,
        cluster_uid: cluster_uid.clone(),
        publisher,
        declared,
    });
    info!(cluster_uid = %cluster_uid, "watching PolarAgents in every namespace");
    Controller::new(agents, watcher::Config::default())
        .owns(pods, watcher::Config::default().labels(AGENT_LABEL))
        .shutdown_on_signal()
        .run(reconcile, error_policy, context)
        .for_each(|result| async move {
            if let Err(e) = result {
                warn!("reconcile failed: {e}");
            }
        })
        .await;
    Ok(())
}

async fn reconcile(agent: Arc<PolarAgent>, ctx: Arc<Context>) -> Result<Action, Error> {
    // Being deleted: the pods go with it (they are owned), nothing to decide.
    if agent.meta().deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }
    let namespace = agent
        .namespace()
        .ok_or_else(|| Error::Other("a PolarAgent is namespaced".into()))?;
    let name = agent.name_any();
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), &namespace);
    let agents: Api<PolarAgent> = Api::namespaced(ctx.client.clone(), &namespace);

    // Read from the API, not the watch cache: the single-instance rule must see a pod
    // created a moment ago.
    let listed = pods
        .list(&ListParams::default().labels(&format!("{AGENT_LABEL}={name}")))
        .await?;
    let instances: Vec<_> = listed.items.iter().map(pod::view).collect();
    let hash = pod::spec_hash(&agent.spec);
    let remembered = agent.status.clone().unwrap_or_default();
    let agent_name = agent.agent_name();
    let deployment = events::deployment_ref(&agent, &ctx.cluster_uid);
    let now_ms = chrono::Utc::now().timestamp_millis();

    let uid = agent.metadata.uid.clone().unwrap_or_default();
    if ctx.declared.record(&uid, &hash) {
        ctx.publisher.publish(&events::deployment_declared(
            &agent,
            deployment.clone(),
            &hash,
            now_ms,
        ));
        // Re-announce the live instance too, so a processor that missed its start (it
        // joined later, or the broker was away) still learns which pod the agent runs as.
        for live in listed
            .items
            .iter()
            .filter(|p| p.metadata.deletion_timestamp.is_none())
        {
            let (Some(pod_name), Some(pod_uid)) = (&live.metadata.name, &live.metadata.uid) else {
                continue;
            };
            let started_at = live
                .status
                .as_ref()
                .and_then(|s| s.start_time.as_ref())
                .map(|t| t.0.as_millisecond())
                .unwrap_or(now_ms);
            let instance = pod_name
                .rsplit_once('-')
                .and_then(|(_, n)| n.parse().ok())
                .unwrap_or(remembered.instances_started);
            let pod_hash = live
                .metadata
                .labels
                .as_ref()
                .and_then(|l| l.get(crate::SPEC_HASH_LABEL))
                .cloned()
                .unwrap_or_default();
            ctx.publisher.publish(&events::started(
                &agent,
                deployment.clone(),
                events::instance_ref(&ctx.cluster_uid, pod_uid, pod_name),
                instance,
                &pod_hash,
                started_at,
            ));
        }
    }

    let plan = decide(
        Desired {
            name: &name,
            spec_hash: &hash,
            suspend: agent.spec.suspend,
        },
        &instances,
        &remembered,
        chrono::Utc::now(),
        &ctx.policy,
    );

    if plan.status != remembered {
        if plan.status.last_ending != remembered.last_ending
            && let Some(ending) = &plan.status.last_ending
        {
            info!(
                agent = %name, pod = %ending.pod, kind = ?ending.kind, exit = ending.exit_code,
                ran_secs = ending.ran_secs, message = ending.message.as_deref().unwrap_or(""),
                "instance ended"
            );
        }
        if plan.status.phase != remembered.phase {
            info!(agent = %name, phase = ?plan.status.phase, message = plan.status.message.as_deref().unwrap_or(""), "phase");
        }
        agents
            .patch_status(
                &name,
                &PatchParams::apply(MANAGER),
                &Patch::Merge(json!({ "status": plan.status })),
            )
            .await?;
        for event in
            events::transitions(&agent_name, &deployment, &remembered, &plan.status, now_ms)
        {
            ctx.publisher.publish(&event);
        }
    }

    for action in &plan.actions {
        match action {
            LifecycleAction::Create { pod: pod_name } => {
                ensure_service_account(&ctx.client, &agent, &namespace).await?;
                let manifest = pod::build(&agent, pod_name, &hash);
                match pods.create(&PostParams::default(), &manifest).await {
                    Ok(created) => {
                        info!(agent = %name, pod = %pod_name, spec = %hash, "started instance");
                        let pod_uid = created.metadata.uid.unwrap_or_default();
                        ctx.publisher.publish(&events::started(
                            &agent,
                            deployment.clone(),
                            events::instance_ref(&ctx.cluster_uid, &pod_uid, pod_name),
                            plan.status.instances_started,
                            &hash,
                            now_ms,
                        ));
                    }
                    Err(kube::Error::Api(e)) if e.code == 409 => {
                        info!(agent = %name, pod = %pod_name, "instance already exists")
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            LifecycleAction::Delete { pod: pod_name, why } => {
                match pods.delete(pod_name, &DeleteParams::default()).await {
                    Ok(_) => info!(agent = %name, pod = %pod_name, why = %why, "stopping instance"),
                    Err(kube::Error::Api(e)) if e.code == 404 => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    Ok(match plan.requeue_after {
        Some(after) => Action::requeue(after.max(Duration::from_millis(500))),
        None => Action::requeue(RESYNC),
    })
}

/// The agent's identity. Created, owned by the agent, if it does not exist; an existing
/// one (made by someone else, with its own bindings) is used as it is.
async fn ensure_service_account(
    client: &Client,
    agent: &PolarAgent,
    namespace: &str,
) -> Result<(), Error> {
    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), namespace);
    let name = pod::service_account(agent);
    if accounts.get_opt(&name).await?.is_some() {
        return Ok(());
    }
    let account = ServiceAccount {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace.to_string()),
            owner_references: agent.controller_owner_ref(&()).map(|o| vec![o]),
            ..Default::default()
        },
        automount_service_account_token: Some(false),
        ..Default::default()
    };
    match accounts.create(&PostParams::default(), &account).await {
        Ok(_) => {
            info!(agent = %agent.name_any(), account = %name, "created identity");
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 409 => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn error_policy(agent: Arc<PolarAgent>, error: &Error, _: Arc<Context>) -> Action {
    warn!(agent = %agent.name_any(), "{error}; retrying in 5s");
    Action::requeue(Duration::from_secs(5))
}
