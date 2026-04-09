use kube_observer::supervisor::ClusterObserverSupervisor;
use ractor::Actor;
use tracing::error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install aws-lc-rs as the default Rustls crypto provider.
    // Required when multiple providers are present in the dependency tree.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    polar::init_logging("kubernetes.cluster.observer.supervisor".to_string());

    match Actor::spawn(
        Some("kubernetes.cluster.observer.supervisor".to_string()),
        ClusterObserverSupervisor,
        (),
    )
    .await
    {
        Ok((_, handle)) => handle.await.expect("Something went wrong"),
        Err(e) => error!("{e}"),
    }
    Ok(())
}
