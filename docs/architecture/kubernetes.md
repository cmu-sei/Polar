# Kubernetes Agent Design Document

## Overview
The Kubernetes Agent is designed to observe and report on the state of Kubernetes cluster resources for integration into a larger distributed data processing and analysis system. The agent is responsible for initial observation and continuous monitoring (via watchers) of Kubernetes resources, such as ConfigMaps, Pods, and Nodes, with the goal of capturing relevant metadata and relationships for downstream processing (e.g., graph database ingestion, lineage tracking, and security assurance).

## Key Design Decisions

### 1. **Hybrid Observation Model**
- **Decision**: Use a hybrid approach of initial listing (observation) followed by continuous watching for events, changes etc.
- **Rationale**: Some Kubernetes resources may already exist at agent startup, and only relying on a watcher would miss them. Listing all resources ensures full visibility from the start, while watchers capture ongoing changes.


### 2. **Concurrent Namespace-Specific Observers**
- **Decision**: Spawn a separate asynchronous task per namespace for each observed resource.
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

The ClusterSupervisor watches all child actors.

upon their failure, it will restart:
    Individual watcher/processor (gracefully or forcefully).
    All watchers in a namespace.

Dead-letter channel for errors or dropped messages from policy agents.

Optional: metrics on mailbox sizes, message rates, errors?

**Observers** (Kube.rs watchers)
 - Wraps a kube_runtime::watcher for a specific resource type.
 - Sends structured events (created, updated, deleted) to corresponding consumer agents.

**ConsumerSupervisor**
 - Similar to the observer cluster supervisor, the consumer has the simple task of lifecycle management for its child actors.

**Consumers** 
 - Receive k8s data from observer actors corresponding to their resource.
 - Transforms data structures into queries to represent resources in the graph database.

## Current Resource Implementations

### Nodes
TODO: My initial thoughts are that since nodes aren't namespaced, the supervisor or perhaps another actor shopuld watch them for real time changes.
### ConfigMaps
- Lists all ConfigMaps in the specified namespaces.
- Watches for changes (Add, Update, Delete).
- Prints metadata and names.

### Pods
- Lists all Pods in the specified namespaces.
- Extracts container image information.
- Watches for changes and prints update events.
- Planned: Link images to container registry metadata.

### Nodes
- Lists all cluster nodes.
- Extracts readiness status, CPU/memory capacity, labels.
- Watches for status changes and node events.

## Planned Enhancements
- **Inter-observer Communication**: Message passing between observers to verify resource references (e.g., Pod-to-Secret linkage).
- **Graph Building**: Emit messages representing resource relationships to a central processor or graph DB writer.
- **Security Auditing**: Track usage of sensitive resources like Secrets and ServiceAccounts.
- **Actor Model Integration**: Full integration with Ractor-based message-passing system.

## Conclusion
This design allows the Kubernetes Agent to flexibly and reliably observe resource states while providing a strong foundation for relationship mapping and security verification. It balances completeness, modularity, and extensibility to support evolving requirements across regulated and secure environments.

