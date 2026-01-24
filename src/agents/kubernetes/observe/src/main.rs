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

// /// Helper function to continuously observe nodes and their events
// /// TODO: Refactor to use tracing to output, and send events to consumers
// async fn _observe_nodes(client: Client) -> Result<(), watcher::Error> {
//     let api: Api<Node> = Api::all(client.clone());
//     let node_list = api
//         .list(&ListParams::default())
//         .await
//         .expect("Failed to list nodes");

//     println!("--- Current Nodes ---");
//     for node in node_list.items {
//         print_node_info(&node);
//     }

//     let mut watcher = watcher(api, watcher::Config::default()).boxed();

//     while let Some(event) = watcher.try_next().await? {
//         match event {
//             Event::Apply(node) => {
//                 println!("Node updated/applied:");
//                 print_node_info(&node);
//             }
//             Event::Delete(node) => {
//                 println!("Node deleted: {}", node.name_any());
//             }
//             _ => (),
//         }
//     }

//     Ok(())
// }
