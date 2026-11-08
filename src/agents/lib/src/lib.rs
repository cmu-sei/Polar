/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

pub mod cassini;
pub mod egress;
pub mod event_log;
pub mod events;
pub mod graph;
pub mod logging;
pub mod request_reply;
pub mod time;
pub mod topics;
pub use events::*;

use cassini_client::{
    OfflineBehavior, PublishRequest, TCPClientConfig, TcpClientActor, TcpClientArgs,
    TcpClientMessage,
};
use cassini_types::{ClientEvent, WireTraceCtx};
pub use polar_health;
pub use polar_health as health;
use ractor::OutputPort;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::{Certificate, Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::io::Read;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

use crate::cassini::CassiniClient;
use crate::topics::PROVENANCE_DISCOVERY;

// ── Topic constants ────────────────────────────────────────────────────────────

/// Topic the build processor subscribes to for all provenance events.
/// All agents — CI pipeline stages, k8s observer, GitLab observer, resolver —
/// publish canonical `ProvenanceEvent` instances here.
pub const PROVENANCE_LINKER_TOPIC: &str = "polar.provenance.linker";

pub const TRANSACTION_FAILED_ERROR: &str = "Expected to start a transaction with the graph";
pub const QUERY_COMMIT_FAILED: &str = "Error committing transaction to graph";
pub const QUERY_RUN_FAILED: &str = "Error running query on the graph.";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";
pub const BUILD_PROCESSOR_NAME: &str = "polar.builds.processor";

/// The canonical topic for all provenance events.
/// Replaces the old per-subject topics (polar.builds.artifact.produced, etc.).
pub const BUILD_EVENTS_TOPIC: &str = "polar.provenance.events";

// ── Framework types ────────────────────────────────────────────────────────────

/// Re-export of [`GraphControllerActor::get_neo_config`] as a free function.
///
/// Agents that need a Neo4j connection config can call `polar::get_neo_config()`
/// without importing `GraphControllerActor` directly.
pub fn get_neo_config() -> Result<neo4rs::Config, ractor::ActorProcessingErr> {
    graph::controller::GraphControllerActor::get_neo_config()
}

/// Wrapper for a ractor output port where a raw message payload and topic
/// can be piped to a necessary dispatcher.
pub type QueueOutput = Arc<OutputPort<(Vec<u8>, String)>>;

/// Canonical serialization error type for the system.
pub type RkyvError = rkyv::rancor::Error;

pub trait Supervisor {
    /// Dispatch messages off message queues to associated actors within
    /// an agent supervision tree.
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>);
}

pub enum SupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    Heartbeat,
    /// Graph controller availability/failure signal, translated from
    /// polar::graph::GraphSignal via an OutputPort subscription in
    /// supervisors that hold a graph controller.
    GraphSignal(crate::graph::controller::GraphSignal),
    /// Self-scheduled via send_after after a fatal ActorFailed/ActorTerminated
    /// event. Gives the actor's normal mailbox loop a bounded window to keep
    /// logging any in-flight messages (which can no longer be processed into
    /// the graph, since whatever failed is presumably why we're exiting)
    /// before the process actually exits.
    /// TODO: once Cassini supports a durable, registration-id-independent
    /// dead-letter queue, publish drained messages there instead of only
    /// logging them.
    ForceExit,
}

/// Spawns `actor` linked under `supervisor` as `name`, unless an actor of that name is already running: then that one is
/// kept, and returned. For the children an agent spawns when its Cassini client registers, which it does again after a
/// reconnection, and after the broker ended its session (cassini-backpressure.md, the decisions reviewed with the
/// maintainer, 14): spawning again under the same name failed, and the agent with it. Whatever the agent does on each
/// registration besides (its subscriptions) it goes on doing.
pub async fn spawn_linked_unless_running<A: Actor>(
    name: &str,
    actor: A,
    args: A::Arguments,
    supervisor: ractor::ActorCell,
) -> Result<ActorRef<A::Msg>, ActorProcessingErr> {
    if let Some(running) = ractor::registry::where_is(name.to_string()) {
        debug!(%name, "already running: kept");
        return Ok(running.into());
    }
    let (spawned, _) = Actor::spawn_linked(Some(name.to_string()), actor, args, supervisor)
        .await
        .map_err(ActorProcessingErr::from)?;
    Ok(spawned)
}

/// Spawn a TCP client to connect to the Cassini message broker.
///
/// CAUTION: There is a known bug in the ractor framework that makes output
/// ports drop messages and freeze under high load.
/// See: https://github.com/slawlor/ractor/issues/225
#[deprecated = "There is a known bug in the ractor framework that makes output ports drop messages and freeze under high load. Functionality moved to TcpClient::spawn()"]
pub async fn spawn_tcp_client<M, F>(
    service_name: &str,
    supervisor: ActorRef<M>,
    map_event: F,
) -> Result<ActorRef<TcpClientMessage>, ActorProcessingErr>
where
    M: Send + 'static,
    F: Fn(ClientEvent) -> Option<M> + Send + Sync + 'static,
{
    let events_output = std::sync::Arc::new(OutputPort::default());

    events_output.subscribe(supervisor.clone(), map_event);

    let config = TCPClientConfig::new()?;

    let (tcp_client, _) = Actor::spawn_linked(
        Some(format!("{service_name}.tcp")),
        TcpClientActor,
        TcpClientArgs {
            config,
            registration_id: None,
            delivery: cassini_client::DeliveryMode::Broadcast(events_output),
            answers: None,
        },
        supervisor.into(),
    )
    .await?;

    Ok(tcp_client)
}

// ── Provenance emission helper ─────────────────────────────────────────────────

/// Serialize and publish a [`ProvenanceEvent`] to the discovery topic via rkyv.
///
/// Used by Rust agents that emit events directly — k8s observer, GitLab observer,
/// resolver. Nushell pipeline stages use `emit-build-event` in `core.nu` instead,
/// which emits JSON to the same topic.
pub fn emit_provenance_event(
    ev: ProvenanceEvent,
    client: &dyn CassiniClient,
) -> Result<(), ActorProcessingErr> {
    let payload = rkyv::to_bytes::<RkyvError>(&ev)?.to_vec();
    let trace_ctx = WireTraceCtx::from_current_span();
    tracing::trace!("Emitting event {ev:?}");

    client.publish(PublishRequest {
        topic: PROVENANCE_DISCOVERY.to_string(),
        payload,
        trace_ctx,
        offline_behavior: OfflineBehavior::Drop,
        share_identity: true,
    })?;

    Ok(())
}

// ── File utilities ─────────────────────────────────────────────────────────────

/// Read a file at a given path and return its raw bytes.
#[deprecated = "Use async_get_file_as_byte_vec instead to avoid blocking."]
pub fn get_file_as_byte_vec(filename: &String) -> Result<Vec<u8>, std::io::Error> {
    let mut f = std::fs::File::open(filename)?;
    let metadata = std::fs::metadata(filename)?;
    let mut buffer = vec![0; metadata.len() as usize];
    f.read_exact(&mut buffer)?;
    Ok(buffer)
}

pub async fn async_get_file_as_byte_vec(path: &str) -> Result<Vec<u8>, std::io::Error> {
    let mut file = fs::File::open(path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;
    Ok(buffer)
}

/// Generate a content-addressed UID for an artifact based on its raw bytes.
/// This is the *only* valid way artifacts should acquire identity.
pub fn artifact_uid_from_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex::encode(digest)
}

// ── Web client ─────────────────────────────────────────────────────────────────
// TODO: Generalize to try and read a file from an env var
pub async fn try_get_proxy_ca_cert() -> Option<Vec<u8>> {
    match std::env::var("PROXY_CA_CERT") {
        Ok(path) => {
            info!("Attempting to load a ca certificate at {path}");
            async_get_file_as_byte_vec(&path).await.ok()
        }
        Err(_e) => None,
    }
}

/// Build an HTTP client with optional proxy CA certificate.
///
/// Reads `PROXY_CA_CERT` from the environment. If present, configures
/// the client to trust that certificate. Falls back to default trust roots.
pub fn get_web_client() -> Result<Client, ActorProcessingErr> {
    debug!("Attempting to find PROXY_CA_CERT");
    match std::env::var("PROXY_CA_CERT") {
        Ok(path) => {
            let cert_data = std::fs::read(&path)?;
            let root_cert = Certificate::from_pem(&cert_data)?;
            debug!(
                "Found PROXY_CA_CERT at: {}. Configuring web client...",
                path
            );
            Ok(ClientBuilder::new()
                .add_root_certificate(root_cert)
                .use_rustls_tls()
                .build()?)
        }
        Err(_) => {
            debug!(
                "PROXY_CA_CERT not found. Configuring web client without proxy CA certificate..."
            );
            Ok(ClientBuilder::new().build()?)
        }
    }
}

// ── Domain types ───────────────────────────────────────────────────────────────

/// A canonical reference to a container image.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContainerImageReference {
    pub registry: String,
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

impl ContainerImageReference {
    pub fn canonical(&self) -> String {
        match (&self.tag, &self.digest) {
            (Some(tag), _) => format!("{}/{}:{}", self.registry, self.repository, tag),
            (None, Some(digest)) => format!("{}/{}@{}", self.registry, self.repository, digest),
            _ => format!("{}/{}", self.registry, self.repository),
        }
    }
}

#[derive(Debug)]
pub enum DispatcherMessage {
    Dispatch { message: Vec<u8>, topic: String },
}

// ── SBOM types ─────────────────────────────────────────────────────────────────

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Hash,
    rkyv::Serialize,
    rkyv::Deserialize,
    rkyv::Archive,
)]
pub enum SbomFormat {
    CycloneDx,
    Spdx,
}

impl SbomFormat {
    pub fn as_str(&self) -> &str {
        match self {
            SbomFormat::CycloneDx => "CycloneDx",
            SbomFormat::Spdx => "spdx",
        }
    }
}

#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Serialize,
    rkyv::Deserialize,
    rkyv::Archive,
)]
pub struct NormalizedComponent {
    pub name: String,
    pub version: String,
    pub purl: String,
    pub component_type: String,
}

#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Serialize,
    rkyv::Deserialize,
    rkyv::Archive,
)]
pub struct NormalizedSbom {
    pub format: SbomFormat,
    pub spec_version: String,
    pub components: Vec<NormalizedComponent>,
}

// ── Logging ────────────────────────────────────────────────────────────────────

pub fn init_logging(service_name: String) {
    logging::init_with(service_name, &logging::LoggingSettings::from_process_env());
}

#[cfg(test)]
mod spawn_tests {
    use super::*;

    struct Child;

    #[ractor::async_trait]
    impl Actor for Child {
        type Msg = ();
        type State = ();
        type Arguments = ();

        async fn pre_start(&self, _myself: ActorRef<()>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
    }

    struct Parent;

    #[ractor::async_trait]
    impl Actor for Parent {
        type Msg = ();
        type State = ();
        type Arguments = ();

        async fn pre_start(&self, _myself: ActorRef<()>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
    }

    /// An agent's child spawned on each registration, registered twice (as after a session the broker ended): the
    /// second keeps the first, and nothing fails; once it has ended, the next registration spawns it again.
    #[tokio::test]
    async fn a_child_spawned_on_each_registration_survives_a_second_registration() {
        let (parent, _) = Actor::spawn(None, Parent, ()).await.unwrap();
        let name = "polar.test.spawned-on-registration";
        let first = spawn_linked_unless_running(name, Child, (), parent.get_cell())
            .await
            .expect("the first registration spawns it");
        let second = spawn_linked_unless_running(name, Child, (), parent.get_cell())
            .await
            .expect("a second registration does not fail");
        assert_eq!(first.get_id(), second.get_id(), "the running one is kept");
        first.stop_and_wait(None, None).await.unwrap();
        let third = spawn_linked_unless_running(name, Child, (), parent.get_cell())
            .await
            .expect("once ended, spawned again");
        assert_ne!(third.get_id(), first.get_id());
        third.stop_and_wait(None, None).await.unwrap();
        parent.stop(None);
    }
}
