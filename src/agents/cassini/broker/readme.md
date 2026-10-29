<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

# Cassini

Polar's resilient message broker

## Overview

Cassini is a message broker built using the [Ractor](https://crates.io/crates/ractor) framework, designed to handle service to service messaging in Polar. The message broker acts as a central hub for passing messages between various actors within the system. Additionally, the project includes a client that communicates with the broker over TCP, with connections secured using mutual TLS (mTLS).

The goal of the project is to provide a reliable, scalable, and secure messaging infrastructure.

---

## Features

### Core Features
- **Actor-Based Architecture**: Leverages Ractor to implement both the broker and client as actors, promoting modularity and scalability.
- **Message Routing**: The broker can route messages to multiple subscribers or forward specific messages to target actors.
- **Session Management**: Clients maintain session state, such as a unique session ID, for handling subscriptions and reconnections.

### Security
- **mTLS Communication**:
  - Server and client authenticate each other using certificates signed by a trusted root Certificate Authority (CA).
  - Ensures encrypted communication and prevents unauthorized connections.

### Extensibility
- **Actor Integration**: The broker is designed to work seamlessly with other actors in a Rust project, supporting custom message types and patterns.


---

## Architecture

### Message Broker
- **Supervisor** The central supervisor process that primarily manages the lifecycle of its more productive managers.
- **Listener Manager**: The process that listens for incoming connections on a configured address and port. In cooperation with the session manager, it manages the lifecycle events of connected clients by spinning up additional *listeners*.
    - *Listeners* are actors that maintain the actual TLS secured client connections, they forward and respond to incoming messages, and live and die by the TCP connection they maintain.
- **Session Manager**: This supervisor manages connected client *sessions* and is responsible for "registering" authenticated clients and cleaning up after them when they disconnect, intentionally or otherwise
    - *Sessions* are actors primarily responsible for communicating with all other actors in the architecture and storing additional metadata about the client connection. When a client is registered, all messages go through these actors.
- **Subscriber Manager**: As its name suggests, this supervisor is responsible for managing subscription actors that represent all connected client subscriptions to a particular *topic*
    - *Subscribers* are actors that represent a client's subscriptions. They are responsible for actually forwarding new messages published to the session they're resposible for.
- **Topic Manager**: This supervisor manages the actual topics the clients wish to publish messages to and read from.
    - *Topic* actors are responsible for managing the actual message queues for individual topics.


### Client
- **Asynchronous Communication**: The client connects to the broker over a secure TCP connection, sends requests, and receives responses.
- **Session Persistence**: Each client maintains session state, including a unique session ID, allowing for reconnects in case of network interruptions.

---

## Running it on its own

The broker needs nothing else of Polar to start, accept mTLS clients and route messages: no cert-issuer, credentials
agent, `polar-init`, custody daemon or Kubernetes. It needs three PEM files from a CA of your choosing and a shutdown
token. Checked 2026-10-28 by the steps below.

### What it reads

| Variable | | What it is |
|---|---|---|
| `TLS_SERVER_CERT_CHAIN` | required | the broker's certificate, then the CA that signed it, in PEM |
| `TLS_SERVER_KEY` | one of these two | the broker's private key, in PEM |
| `POLAR_CUSTODY_SOCKET` | | the custody daemon's socket instead, which holds the key (`docs/architecture/key-custody.md`); leave it unset here |
| `TLS_CA_CERT` | required | the CA every client's certificate must chain to (its first certificate is used) |
| `CASSINI_SHUTDOWN_TOKEN` | required, secret | the token an orderly shutdown asks with |
| `CASSINI_BIND_ADDR` | default `0.0.0.0:8080` | where it listens |
| `CASSINI_DECISION_CACHE_SECS` | default `5` | how long a connection keeps a publish or subscribe decision; `0` decides every one |
| `RUST_LOG` | default `info` | what is written to the console |
| `JAEGER_OTLP_ENDPOINT` | optional | where traces are exported over OTLP HTTP; none are exported when it is unset |

`POLAR_START_MARKER` is set only in a cluster pod (by `polar-init`); leave it unset, or the broker exits 1 at once for
want of the marker. `cassini-server --help` lists the broker's own variables with what each is for; `--scaffold` gives
the whole configuration document (the builder's and the health actor's variables too), every value empty, as JSON.

### Who may do what, with no credentials agent

The broker decides every publish and subscribe: the policy first, then the certificate's floor
(`docs/architecture/cassini-broker.md`, "Policy in the broker"). The policy reaches it only as the credentials agent's
signed authority, over Cassini itself; with no credentials agent there is none, and nothing turns enforcement off or
loads a policy from a file. So on its own the broker gives each client what its certificate admits and refuses the rest:

- **Topics named in the certificate**, as URI SANs: `p:pub:<topic>` to publish, `p:sub:<topic>` to subscribe. This is the
  simple way, and the one below.
- **Topics a workload implies**: an identity (the first DNS SAN) of the form `<workload>.<namespace>.serviceaccount.<domain>`,
  with `g:<group>` URI SANs, is given the topics its workload and groups imply (`cert-issuer/common/src/topics.rs`).
- **A policy, by playing the credentials agent**: a client whose first DNS SAN is `POLAR_AUTHORITY_PUBLISHER`
  (`credentials-agent.polar.serviceaccount.cluster.local` by default) and whose certificate admits publishing
  `polar-authority-snapshot-replies` can publish a signed `AuthoritySnapshot`, signed by a document-signing key whose
  certificate chains to `TLS_CA_CERT`. `tests/enforcement.rs` does this; there is no tool for it.

A client whose certificate names no topics, and implies none, may use none.

### Step by step

From a scratch directory, with `openssl`, and the workspace built (`cargo build --bin cassini-server --bin
cassini-client` in `src/agents`; the binaries are in `src/agents/target/debug`). P-256 keys, as cert-issuer issues.

1. A CA, the broker's certificate (named `localhost`, which the client checks), and a client's certificate carrying the
   topic it may use:

   ```bash
   openssl req -x509 -new -nodes -newkey ec -pkeyopt ec_paramgen_curve:P-256 -keyout ca.key -out ca.pem -days 30 \
     -subj "/CN=cassini-dev-ca" -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign"

   openssl req -new -nodes -newkey ec -pkeyopt ec_paramgen_curve:P-256 -keyout server.key -out server.csr -subj "/CN=localhost"
   openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out server.pem -days 30 \
     -extfile <(printf "subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature")
   cat server.pem ca.pem > server-chain.pem

   openssl req -new -nodes -newkey ec -pkeyopt ec_paramgen_curve:P-256 -keyout client.key -out client.csr -subj "/CN=demo-client"
   openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out client.pem -days 30 \
     -extfile <(printf "subjectAltName=DNS:demo-client,URI:p:pub:demo.hello,URI:p:sub:demo.hello\nextendedKeyUsage=clientAuth\nkeyUsage=critical,digitalSignature")
   ```

   The client's identity is its first DNS SAN (`demo-client`); the URI SANs are what it may use.

2. The broker, in one terminal. The `env -u` clears what a Polar dev shell may already export:

   ```bash
   env -u POLAR_START_MARKER -u POLAR_CUSTODY_SOCKET -u JAEGER_OTLP_ENDPOINT \
     CASSINI_BIND_ADDR=127.0.0.1:8080 \
     TLS_CA_CERT=$PWD/ca.pem TLS_SERVER_CERT_CHAIN=$PWD/server-chain.pem TLS_SERVER_KEY=$PWD/server.key \
     CASSINI_SHUTDOWN_TOKEN="$(head -c 24 /dev/urandom | base64)" \
     cassini-server
   ```

   It logs `Broker startup complete` and `Server running on 127.0.0.1:8080`. It also asks, once, for the credentials
   agent's authority (`polar-authority-snapshot-requests`): nobody answers, and it goes on without.

3. A client, in another. The command-line client reads its own variables, and writes its results to its log, which
   `CASSINI_LOG` (not `RUST_LOG`) filters; `RUST_LOG=off` keeps it from exporting traces to `localhost:4318`:

   ```bash
   export BROKER_ADDR=127.0.0.1:8080 CASSINI_SERVER_NAME=localhost \
     TLS_CA_CERT=$PWD/ca.pem TLS_CLIENT_CERT=$PWD/client.pem TLS_CLIENT_KEY=$PWD/client.key \
     CASSINI_LOG=info RUST_LOG=off
   unset POLAR_CUSTODY_SOCKET CASSINI_DAEMON_SOCK

   cassini-client publish demo.hello "hello"     # Published to topic=demo.hello
   cassini-client list-topics                    # demo.hello, polar-authority-snapshot-requests
   cassini-client publish demo.other "x"         # refused: not among its declared or implied topics
   ```

   The refusal is logged at once ("The broker refused a publish on demo.other ... no policy permits it"); the command
   then waits out its publish timeout (10 s, `--publish-timeout`) and fails.

4. Stop the broker with Ctrl-C (SIGTERM). It shuts down in order and exits 1: a broker that ends has failed, by design,
   so that its pod restarts.

The command-line client publishes and asks the broker about itself; it does not subscribe. To watch messages arrive,
the broker's tests (`tests/`, with a throwaway CA in `tests/support/mod.rs`) and the in-cluster load harness
(`../test/README.md`, `infra/load-harness.nu`) are where subscribers are.

### Traces

To see spans in Jaeger, run one and point the broker at it:

```bash
podman run --rm --name jaeger -p 16686:16686 -p 4318:4318 -e COLLECTOR_OTLP_ENABLED=true cr.jaegertracing.io/jaegertracing/jaeger:2.11.0
JAEGER_OTLP_ENDPOINT=http://localhost:4318/v1/traces   # in the broker's environment
```

## Testing

The broker's own tests run it in process, with a throwaway CA: `cargo test -p cassini-broker` in `src/agents`. The load
harness runs in the cluster: [its README](../test/README.md).
