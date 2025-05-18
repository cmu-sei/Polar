# Kubernetes Observer and Consumer Agents

This repository contains microservices that observe Kubernetes clusters and process cluster data into a graph representation. The system consists of:

* **Observer Agents**: Collect resource data from a Kubernetes cluster and watches for events.
* **Consumer Agents**: Transform and enrich this data, then publish it to a graph database.

## Getting Started

### Prerequisites

* Rust (latest stable, install via [`rustup`](https://rustup.rs/))
* Access to a Kubernetes cluster
* A valid `KUBECONFIG` file or in-cluster access
* mTLS certificates for secure service communication
* Running instance of the **Cassini** message broker - see [cassini's README for details](../broker/readme.md)

---

### Set Up Kubernetes Access

The observer agents use the [`kube`](https://docs.rs/kube) crate to authenticate and interact with the Kubernetes API. It will automatically detect configuration in the following order:

* The `KUBECONFIG` environment variable (if set)
* `$HOME/.kube/config`
* In-cluster configuration (if deployed as a pod)

If you're running locally, export your `KUBECONFIG`:

```bash
export KUBECONFIG=$HOME/.kube/config
```

---

### 3. Prepare mTLS Certificates

Services communicate over mutually authenticated TLS.

Each service instance (Observer, Consumer, Cassini) requires:

* A certificate (`cert.pem`)
* A private key (`key.pem`)
* A trusted CA certificate (`ca.pem`)
 
**NOTE: ALL must be base64 PEM encoded files!**

See the [workspace readme](../README.md) for details on generating your own using the nix flake!

---

### 4. Start the Cassini Broker

Cassini is a part of the Polar workspace, so from the `src/agents` directory, you should be able to run the following to start it, provided your environment is set up properly.

```bash
cargo run -b cassini-server
```

---

### 5. Run the Observer Agent

Each observer monitors a specific cluster and publishes messages to Cassini.

```bash
cargo run -b kube-observer
```

---

### 6. Run the Consumer Agent

Consumers subscribe to messages from Cassini and process them into graph nodes/edges.

```bash
cargo run -p kube-consumer
```
