use kube_observer::supervisor::ClusterObserverSupervisor;
use ractor::Actor;
use tracing::error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install aws-lc-rs as the default Rustls crypto provider.
    // Required when multiple providers are present in the dependency tree.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    polar::init_logging("kubernetes.cluster.observer.supervisor".to_string());

    let (_, handle) = Actor::spawn(
        Some("kubernetes.cluster.observer.supervisor".to_string()),
        ClusterObserverSupervisor,
        (),
    )
    .await
    .map_err(|e| {
        error!("{e}");
        format!("kube-observer failed to start: {e}")
    })?;

    handle
        .await
        .map_err(|e| format!("kube-observer supervisor task failed: {e}"))?;

    // The supervisor stopping is never a success: an observer that is no
    // longer observing must be restarted. Exit non-zero so the Job does.
    Err("kube-observer supervisor stopped; exiting non-zero so the Job restarts it".into())
}
