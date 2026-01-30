pub mod pods;
pub mod supervisor;

pub const TCP_CLIENT_NAME: &str = "kubernetes.cluster_name.supervisor_name.client";

pub struct KubernetesObserver;

pub struct KubernetesObserverState {
    pub namespace: String,
    pub client: kube::Client,
}

/// Standard messages exchanged by the observer actors internal to the agent
pub enum KubernetesObserverMessage {
    Pods,
    Deployments,
    ConfigMaps,
    Secrets,
}

pub struct KubernetesObserverArgs {
    pub namespace: String,
}
