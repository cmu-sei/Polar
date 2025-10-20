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

## Getting Started

### Prerequisites
- **Rust**: Install Rust using [rustup](https://rustup.rs/).
- **Certificates**: Generate a root CA, server, and client certificates for mTLS using tools like OpenSSL or [tls-gen](https://github.com/rabbitmq/tls-gen).
- **Dependencies**: Install the required Rust crates (`ractor`, `tokio`, `tokio-rustls`, `serde`, etc.).

---
Ensure the following environment variables are set before trying to run cassini
```bash
# The address the broker server will bind and listen for connections to
# for example  127.0.0.1:8080 to listen on your host system's local port 8080
export CASSINI_BIND_ADDR=""

# The absolute file path to the ca_certificates.pem file created by TLS_GEN.
# Used by the Rust binaries to recognize eachother through TLS
export TLS_CA_CERT=""

# The absolute file path to the certificate chain containing the server certificate,followed by the root ca certificate used to sign it
# optionally, followed by the server key, if one was set - MUST BE IN PEM FORMAT
export TLS_SERVER_CERT_CHAIN=""
# The server key file
export TLS_SERVER_KEY=""

### These must also be set anywhere the TcpClient actor is in use, for example, the cassini integration tests the TLS_CA_CERT must also be provided.
#The absolute file path to the client certificate - MUST BE IN PEM FORMAT
#export TLS_CLIENT_CERT=""
# The absolute file path to the client key - MUST BE IN PEM FORMAT
#export TLS_CLIENT_KEY=""
```


## Example Usage

1. **Publish a Message**:
   The client sends a `PublishRequest` to the broker with a topic and payload. The broker routes the message to all subscribed clients.

2. **Subscribe to a Topic**:
   The client sends a `SubscribeRequest` to the broker. Once subscribed, the client receives messages published to the specified topic.

3. **Disconnect Gracefully**:
   The client sends a `DisconnectRequest`, and the broker cleans up the associated session actor and any subscriptions associated with it.

## Testing
[Check out the README for our test harness](..test/README.md)
