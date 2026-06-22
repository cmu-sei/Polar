# Kubernetes Agent Design Document

## Overview
The Kubernetes Agent is designed to observe and report on the state of Kubernetes cluster resources for integration into a larger distributed data processing and analysis system. The agent is responsible for initial observation and continuous monitoring (via watchers) of Kubernetes resources, such as ConfigMaps, Pods, and Nodes, with the goal of capturing relevant metadata and relationships for downstream processing (e.g., graph database ingestion, lineage tracking, and security assurance).

## Key Design Decision So Far

### 1. **Hybrid Observation Model**
- **Decision**: Use a hybrid approach of initial listing (observation) followed by continuous watching for events, changes etc.
- **Rationale**: Some Kubernetes resources may already exist at agent startup, and only relying on a watcher would miss them. Listing all resources ensures full visibility from the start, while watchers capture ongoing changes.


### 2. **Concurrent Namespace-Specific Observers**
- **Decision**: The Cluster Observer Supervisor will spawn a separate set of observers per namespace for each observed resource.
- **Rationale**: This supports horizontal scalability and makes it easier to isolate logic or failures to specific namespaces.

### 3. **Eventual Consistency Between Resource Observers**
- **Decision**: Allow each resource observer (e.g., for Pods, ConfigMaps, Secrets) to operate independently, with the expectation that relationships (e.g., Pod references to Secrets) will be reconciled asynchronously.
- **Rationale**: Decouples observers and supports modular design. This approach also aligns with actor-based architectures (e.g., Ractor), where individual actors handle their own domains.

### 4. **Data Enrichment for Relationship Mapping**
- **Decision**: Enrich observed data with relevant contextual metadata (e.g., labels, annotations, image references). Many clusters in regulated environments mandate this through kyverno policies already.
- **Rationale**: Enables downstream systems to map resource relationships, such as linking Pods to container registries or ConfigMaps to services.

### 5. **Modular Expansion Strategy**
- **Decision**: Start with foundational resources (ConfigMaps, Pods, Nodes), and iteratively add observers for other relevant Kubernetes primitives (e.g., Secrets, Services, Volumes, Deployments).
- **Rationale**: Allows focused development and testing of each observer. It also aligns with evolving use cases without overwhelming the system early.

## Topology

TODO: Visual architecture diagram outlining cluster supervisor, and actors for various resources.

## Roles

**Cluster(Observer)Supervisor**
 - Spawns and monitors child actors.
 - Holds shared config and cluster context.
 - Can restart children or orchestrate data flow changes.

Configuration data will be provided by a **Configuration Agent** in the future, but for now, values are hard-coded.

Its supervisor could provide structured actor metadata via dhall configurations.
```dhall
let Actor =
      { cluster : Text
      , namespace : Text
      , role : Text
      , resource : Text
      }

in  [ { cluster = "prod-cluster", namespace = "default", role = "observer", resource = "pod" }
    , ...
    ]
```

The ClusterSupervisor watches all child actors and manages their lifecycles.

Should its children fail for some reason, it will likely restart them, depending on the cause.

In deployment, the container for the kube-observer will need to be able to authenticate with the kubernetes API server, to enable this, we have to specify
a ServiceAccount, a ClusterRole for that account that speicifies desired permissions, and a ClusterRoleBinding for that account. In the future, operators may desire more or less permissions than our standard, so we will aim to make this more configurable.

**Observers** (Kube.rs watchers)
 - Wraps a kube_runtime::watcher for a specific resource type.
 - Sends structured events (created, updated, deleted) to corresponding consumer agents.

**ConsumerSupervisor**
 - Similar to the observer cluster supervisor, the consumer has the simple task of lifecycle management for its child actors.

**Consumers**
 - Receive k8s data from observer actors corresponding to their resource.
 - Transforms data structures into queries to represent resources in the graph database.

## Current Resource Implementations

### Namespaces

Upon initialization, the cluster supervisor will try to discover all the namespaces it can so it can observe them. Once it has this list,
it will spin up additional PodObserver actors to read currently deployed pods and set up a "watcher" to listen for additional pods to come online.

### Nodes
TODO: My initial thoughts are that since nodes aren't namespaced, the supervisor or perhaps another actor shopuld watch them for real time changes. It'll be more valuable whenever we can observe cloud/on-prem machines to see how they look in the cluster.


### Pods

The PodObserver Lists all Pods in the specified namespaces. The datatype exposed by the k8s_openapi crate also contains attributes detailing resources used by the Pod, including volumes, configMaps, secrets, etc.
From there, consumers can extract more information about the relationship to other resources.
Once it reads all data, the observer watches for changes and prints update events.

It is planned to link images to container registry metadata.


### Node Types:

    Pod { name, namespace, serviceAccountName, ... }

    Volume { name } (some volumes will also have backing types)

    ConfigMap { name }

    Secret { name }

    PVC { name }

    ContainerImage { image } (optional, could just be a property)

Edge Types:

    (Pod)-[:USES_VOLUME]->(Volume)

    (Volume)-[:BACKED_BY]->(ConfigMap | Secret | PVC)

    (Pod)-[:USES_CONFIGMAP]->(ConfigMap) (for env vars)

    (Pod)-[:USES_SECRET]->(Secret) (for env vars)

    (Pod)-[:RUNS_IMAGE]->(ContainerImage)

Optional:

    (Pod)-[:RUNS_AS]->(ServiceAccount) (or just store as a property of the relationship)

## Messaging strategy

K8s observers and data processors share a single distinct message type that more or less mirrors kubernetes' events.
The design of our messaging system is driven by several key principles:

### 1. **Unified Abstraction**

By defining a central message type—a `KubeMessage` that wraps an enum of possible Kubernetes resources (e.g., Pod, Node, Deployment)—we create a single, unified abstraction that all parts of the system can rely on. This allows all watchers (the actors interfacing with the Kubernetes API) to output their events in a consistent format, which simplifies downstream routing and processing. In this way, each watcher doesn’t need to know the specifics of every processor; they all just send out an `KubeMessage`.

### 2. **Leverage Serde Serialization**

Since Kubernetes resource types provided by the `k8s-openapi` and `kube` crates are fully `serde`-serializable, we can directly embed these types in our messages. This not only simplifies our implementation by removing the need for manual data transformation but also allows flexibility:

* **Direct Use:** Processors can work with the raw, rich resource data.
* **Data Reduction:** Alternatively, we could extract only the relevant portions (e.g., metadata and status) if we need to optimize message size or processing speed.

### 3. **Decoupling and Loose Coupling**

Using a central routing mechanism (`DataRouter`) that receives all `KubeMessage` messages means the producers (watchers) and consumers (processors) are decoupled. This architecture supports:

* **Flexibility:** New processors can be added without changing the watchers.
* **Resilience:** Failures in one processor won’t necessarily impact the others.
* **Dynamic Routing:** The router can filter, duplicate, or transform messages as needed before sending them to the appropriate actor.

### 4. **Type Safety and Clear Intent**

By having a dedicated enum (`ObservedResource`) for the different resource types, we maintain type safety. Each variant clearly indicates which Kubernetes resource it represents. Additionally, including a `WatchAction` (Applied, Deleted, Restarted) alongside context (such as the cluster name, namespace, and a timestamp) gives every event a well-defined meaning, making the data more actionable for downstream processors.

### 5. **Extensibility**

This design lays a strong foundation for future growth:

* **Adding New Resources:** As you need to observe more resource types, you can simply add a new variant to `ObservedResource` and create the corresponding watcher.
* **Modular Processors:** Each processor (e.g., for metrics, logging, graph updates) can be implemented as an independent actor that subscribes only to the events it cares about.
* **Dynamic Scaling:** The supervision layer (like the `ClusterSupervisor`) can manage these actors independently, allowing you to scale or restart parts of the system without a complete overhaul.

### 6. **Actor Model Fit**

With the actor-based approach using the ractor framework:

* **Message Passing:** Our design naturally fits the message-passing paradigm of actors. Each event is self-contained, and the routing between actors happens via these messages.
* **Fault Isolation:** If a particular processor fails or experiences a surge of events, it doesn’t block or crash the entire system. The supervisor can intervene, and messages can be retried or rerouted as needed.
* **Concurrency:** Actors can process events concurrently, improving the responsiveness and throughput of your service in a dynamic cluster environment.

In summary, the reasoning behind this design is to create a robust, flexible, and scalable architecture that leverages Rust’s type safety and the power of actor-based concurrency. The use of serialization for Kubernetes types simplifies our workflow and ensures that we can easily transform and route the rich data provided by the cluster into actionable insights for visualization and metrics collection.

Does this cover your questions, or would you like to dive deeper into any particular aspect of the messaging system?

## Planned Enhancements
- **Inter-observer Communication**: Message passing between observers to verify resource references (e.g., Pod-to-Secret linkage).
- **Graph Building**: Emit messages representing resource relationships to a central processor or graph DB writer.
- **Security Auditing**: Track usage of sensitive resources like Secrets and ServiceAccounts.
- **Actor Model Integration**: Full integration with Ractor-based message-passing system.

## Conclusion
This design allows the Kubernetes Agent to flexibly and reliably observe resource states while providing a strong foundation for relationship mapping and security verification. It balances completeness, modularity, and extensibility to support evolving requirements across regulated and secure environments.
