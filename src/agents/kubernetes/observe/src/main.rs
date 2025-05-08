use futures::{StreamExt, TryStreamExt};
use kube::{api::{Api, ListParams, ResourceExt}, runtime::watcher::Event, Client, Config};
use k8s_openapi::{api::core::v1::{Node, Pod}, apimachinery::pkg::api::resource::Quantity};
use kube::runtime::watcher;
use k8s_openapi::api::core::v1::ConfigMap;
use kube_observer::{supervisor::{ClusterObserverSupervisor, ClusterObserverSupervisorArgs}, KUBERNETES_OBSERVER};
use ractor::Actor;
use tracing::error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging();

    // Infer the runtime environment and try to create a Kubernetes Client
    let cassini_client_config = cassini::TCPClientConfig::new();


    let args = ClusterObserverSupervisorArgs {
        cassini_client_config,
    };

    //TODO - make name a constant
    // Start kubernetes supervisor
    match Actor::spawn(Some("kubernetes.minikube.observer.supervisor".to_string()), ClusterObserverSupervisor, args).await {
        Ok( (_, handle) ) => handle.await.expect("Something went wrong"),
        Err(e) => error!("{e}")
    }
    Ok(())
    
}


async fn observe_configmaps(client: Client, namespace: String) -> Result<(), watcher::Error> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace);
    let cm_list = api.list(&ListParams::default()).await.expect("Expected to get a list of configmaps");

    println!("Namespace: {}", namespace);
    for cm in cm_list.items {
        let name = cm.metadata.name.unwrap_or_default();
        println!("  - ConfigMap: {}", name);
        
    }

    let mut watcher = watcher(api, watcher::Config::default()).boxed();

    while let Some(event) = watcher.try_next().await? {
        match event {
            Event::Apply(configmap) => {
                println!("ConfigMap updated: {}", configmap.metadata.name.unwrap());
            }
            Event::Delete(configmap) => {
                println!("ConfigMap deleted: {}", configmap.metadata.name.unwrap());
            }
            _ => {}
        }
    }
    Ok(())
}

async fn observe_pods(client: Client, namespace: String) -> Result<(), watcher::Error> {
    let api: Api<Pod> = Api::namespaced(client.clone(), &namespace);
    let pod_list = api.list(&ListParams::default()).await.expect("Expected to get a list of Pods");

    println!("Namespace: {} - Pods:", namespace);
    for pod in pod_list.items {
        println!("  - Pod: {}", pod.name_any());
        log_pod_info(&pod);
    }

    let mut watcher = watcher(api, watcher::Config::default()).boxed();
    while let Some(event) = watcher.try_next().await? {
        match event {
            Event::Apply(pod) => {
                println!("Pod updated: {}", pod.name_any());
                log_pod_info(&pod);
            }
            Event::Delete(pod) => {
                println!("Pod deleted: {}", pod.name_any());
            },
            _ => {}
        }
    }
    Ok(())
}

fn log_pod_info(pod: &Pod) {
    let name = pod.name_any();
    println!("  - Pod: {}", name);

    if let Some(spec) = &pod.spec {
        // ServiceAccount
        if let Some(sa) = &spec.service_account_name {
            println!("    -> Uses ServiceAccount: {} ", sa);
        }

        // Volumes
        if let Some(volumes) = &spec.volumes {
            
            for volume in volumes {
                println!("    -> Uses Volume: {} ", volume.name);

                volume
                .config_map
                .as_ref()
                .map(|cm| println!("    -> Mounts ConfigMap: {} ", cm.name.clone()));

                volume
                .persistent_volume_claim
                .as_ref()
                .map(|pvc| println!("    -> Uses PVC: {} ", pvc.claim_name));
        
                volume
                .secret
                .as_ref()
                .map(|secret| println!("    -> Mounts Secret: {} ", secret.clone().secret_name.unwrap_or_default() ));
            
            }
        }

        // Containers and InitContainers
        let containers = spec.containers.iter().chain(spec.init_containers.iter().flatten());
        for container in containers {
            println!("    -> Container Image: {} ", container.image.clone().unwrap_or_default());

            // Environment variable references
            for env in container.env.iter().flatten() {
                if let Some(value_from) = &env.value_from {
                    if let Some(cm_ref) = &value_from.config_map_key_ref {
                        println!("      -> Env from ConfigMap: {} ", cm_ref.name.clone());
                    }
                    if let Some(secret_ref) = &value_from.secret_key_ref {
                        println!("      -> Env from Secret: {} ", secret_ref.name.clone());
                    }
                }
            }
        }
    }
}

async fn observe_nodes(client: Client) -> Result<(), watcher::Error> {
    let api: Api<Node> = Api::all(client.clone());
    let node_list = api.list(&ListParams::default()).await.expect("Failed to list nodes");

    println!("--- Current Nodes ---");
    for node in node_list.items {
        print_node_info(&node);
    }

    let mut watcher = watcher(api, watcher::Config::default()).boxed();

    while let Some(event) = watcher.try_next().await? {
        match event {
            Event::Apply(node) => {
                println!("Node updated/applied:");
                print_node_info(&node);
            }
            Event::Delete(node) => {
                println!("Node deleted: {}", node.name_any());
            }
            _ => ()
        }
    }

    Ok(())
}

fn print_node_info(node: &Node) {
    let name = node.metadata.name.clone().unwrap_or_default();

    let capacity = node.status.as_ref().and_then(|s| s.capacity.clone());
    let allocatable = node.status.as_ref().and_then(|s| s.allocatable.clone());

    let cpu = capacity
        .as_ref()
        .and_then(|c| c.get("cpu"))
        .map(|c| c.clone())
        .unwrap_or(Quantity::default());

    let memory = capacity
        .as_ref()
        .and_then(|c| c.get("memory"))
        .map(|m| m.clone())
        .unwrap_or(Quantity::default());

    let status = node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|conditions| {
            conditions.iter().find(|c| c.type_ == "Ready").map(|c| c.status.clone())
        })
        .unwrap_or(String::default());

    println!("Node: {}", name);
    println!("  Ready: {}", status);
    println!("  CPU: {} | Memory: {}", cpu.0, memory.0);

    if let Some(labels) = &node.metadata.labels {
        println!("  Labels:");
        for (k, v) in labels {
            println!("    {}: {}", k, v);
        }
    }
}