use kube_observer::supervisor::ClusterObserverSupervisor;
use ractor::Actor;
use tracing::error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging("kubernetes.cluster.observer.supervisor".to_string());

    // Start kubernetes supervisor
    // TODO: REMOVE THIS WHENEVER WE WANT TO OBSERVE MORE CLUSTERS AT ONCE
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
