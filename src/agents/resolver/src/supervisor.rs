//! The resolver's supervisor: the builder's infrastructure (health, the Cassini client and the
//! subscription), the resolver actor, and the order in which they come down.

use crate::resolver::{ResolverAgent, ResolverAgentState};
use crate::settings::ResolverSettings;
use crate::{BROKER_CLIENT_NAME, RESOLVER_SUPERVISOR_NAME};
use cassini_types::ClientEvent;
use oci_client::Client as OciClient;
use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use polar::ProvenanceEvent;
use polar::cassini::{CassiniClient, UnsubscribeRequest};
use polar::topics::PROVENANCE_DISCOVERY;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, InfrastructureEvent, InfrastructureSupervisorMessage,
    Subscription,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::path::PathBuf;
use tracing::{debug, error, info, warn};

/// What the resolver needs to drain: the pull in flight (up to 10 s) and a margin. If it is not
/// done by then the process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10 + 10);

/// How long an unfinished resolution is given to finish once the shutdown has begun.
const IN_FLIGHT_GRACE: Duration = Duration::from_secs(10);

pub enum ResolverSupervisorMessage {
    /// Lifecycle events from the Cassini client.
    ClientEvent { event: ClientEvent },
    /// An image reference was discovered, decoded from the topic.
    Discovered(ProvenanceEvent),
    /// The infrastructure has decided the agent should end: finish, then say so.
    PrepareShutdown,
}

pub struct ResolverSupervisor;

/// The OCI client's configuration: https for every registry except those named as insecure, which
/// are spoken to over http (a development registry), and the extra root certificates, if any.
pub fn client_config(insecure_registries: &[String], certs: Vec<Certificate>) -> ClientConfig {
    if certs.is_empty() {
        debug!("Initializing OCI client without extra certificates");
    } else {
        debug!(
            "Initializing OCI client with {} extra root certificates",
            certs.len()
        );
    }
    ClientConfig {
        protocol: ClientProtocol::HttpsExcept(insecure_registries.to_vec()),
        extra_root_certificates: certs,
        ..ClientConfig::default()
    }
}

pub struct ResolverSupervisorState {
    resolver: Option<ActorRef<ProvenanceEvent>>,
    tcp_client: polar::cassini::TcpClient,
    healthcheck: polar_agent_builder::HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    shutting_down: bool,
}

impl ResolverSupervisor {
    /// The OCI client, trusting the CA the settings name as well as the system's. A CA that is
    /// named and cannot be read stops the agent: a trust root that was asked for must not be
    /// silently left out.
    async fn oci_client(settings: &ResolverSettings) -> Result<OciClient, ActorProcessingErr> {
        let mut certs = Vec::new();
        if let Some(path) = &settings.proxy_ca_cert {
            let data = tokio::fs::read(path).await.map_err(|e| {
                ActorProcessingErr::from(format!("cannot read PROXY_CA_CERT {path}: {e}"))
            })?;
            debug!("Configuring OCI client with PROXY_CA_CERT {path}");
            certs.push(Certificate {
                encoding: CertificateEncoding::Pem,
                data,
            });
        }

        Ok(OciClient::new(client_config(
            &settings.insecure_registries,
            certs,
        )))
    }

    /// Decodes what was published on the discovery topic. Anything that does not decode is logged
    /// and dropped, never fatal.
    pub fn decode(payload: &[u8]) -> Option<ProvenanceEvent> {
        match rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                error!("Failed to deserialize provenance event! {e}");
                None
            }
        }
    }
}

#[async_trait]
impl Actor for ResolverSupervisor {
    type Msg = ResolverSupervisorMessage;
    type State = ResolverSupervisorState;
    /// The configuration `run` judged; the builder is made from it.
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        let settings = ResolverSettings::read(configured.config())?;

        // The subscription: what is published on the discovery topic reaches the supervisor,
        // which hands it to the resolver. The builder subscribes on the wire when the client
        // registers, and again after a reconnect.
        let deliver_to = myself.clone();
        let discovery = Subscription::new(PROVENANCE_DISCOVERY, move |incoming| {
            if let Some(event) = Self::decode(&incoming.payload)
                && let Err(e) =
                    deliver_to.send_message(ResolverSupervisorMessage::Discovered(event))
            {
                warn!("failed to hand a discovered image to the supervisor: {e}");
            }
        });

        // The resolver never touches the graph: Cassini only.
        let resources = AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME)
            .with_subscription(discovery)
            .with_lifecycle_handler(|event| Some(ResolverSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(ResolverSupervisorMessage::PrepareShutdown)
                }
            });

        let oci_client = Self::oci_client(&settings).await?;
        let (resolver, _) = Actor::spawn_linked(
            Some(format!("{RESOLVER_SUPERVISOR_NAME}.resolver")),
            ResolverAgent,
            ResolverAgentState {
                cassini_client: resources.tcp_client.clone(),
                oci_client,
                docker_config: settings.docker_config.map(PathBuf::from),
            },
            myself.get_cell(),
        )
        .await
        .map_err(|e| {
            error!("Failed to spawn resolver agent: {e:?}");
            ActorProcessingErr::from(e)
        })?;

        Ok(ResolverSupervisorState {
            resolver: Some(resolver),
            tcp_client: resources.tcp_client,
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            shutting_down: false,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ResolverSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            ResolverSupervisorMessage::Discovered(event) => {
                if state.shutting_down {
                    warn!("shutting down -- dropping a discovered image: {event:?}");
                } else if let Some(resolver) = &state.resolver {
                    resolver.send_message(event)?;
                }
            }
            ResolverSupervisorMessage::PrepareShutdown => {
                info!(
                    "RESOLVER_SUPERVISOR: PrepareShutdown received -- unsubscribing and letting the resolution in flight finish"
                );
                state.shutting_down = true;
                if let Err(e) = state.tcp_client.unsubscribe(UnsubscribeRequest {
                    topic: PROVENANCE_DISCOVERY.to_string(),
                    trace_ctx: None,
                }) {
                    warn!("unsubscribe from '{PROVENANCE_DISCOVERY}' failed (continuing): {e}");
                }
                if let Some(resolver) = state.resolver.take()
                    && let Err(e) = resolver
                        .stop_and_wait(
                            Some("supervisor shutting down".into()),
                            Some(IN_FLIGHT_GRACE),
                        )
                        .await
                {
                    error!(
                        "the resolver did not stop within {}s, continuing: {e:?}",
                        IN_FLIGHT_GRACE.as_secs()
                    );
                }
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorStarted(actor) => debug!("{actor:?} started!"),
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                info!("RESOLVER_SUPERVISOR: {actor:?} terminated. {reason:?}");
                // The infrastructure is this supervisor's linked child from build(); its ending
                // means the shutdown handshake is done, or that it failed. Either way exit
                // non-zero so the Job creates a new pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("RESOLVER_SUPERVISOR: infrastructure is down, exiting");
                    std::process::exit(1);
                }
                if !state.shutting_down {
                    error!("RESOLVER_SUPERVISOR: the resolver ended unexpectedly; exiting");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, reason) => {
                error!("RESOLVER_SUPERVISOR: {actor:?} failed! {reason:?}");
                if !state.shutting_down {
                    std::process::exit(1);
                }
            }
            _ => (),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registries_named_as_insecure_are_spoken_to_over_http_and_no_other() {
        let config = client_config(
            &["10.0.2.2:5000".to_string(), "reg.local:5000".to_string()],
            Vec::new(),
        );
        match config.protocol {
            ClientProtocol::HttpsExcept(except) => {
                assert_eq!(
                    except,
                    vec!["10.0.2.2:5000".to_string(), "reg.local:5000".to_string()]
                )
            }
            other => panic!("expected https except the named ones, got {other:?}"),
        }
        // and none named means https for every registry
        assert!(matches!(
            client_config(&[], Vec::new()).protocol,
            ClientProtocol::HttpsExcept(ref e) if e.is_empty()
        ));
    }

    #[test]
    fn extra_certificates_do_not_change_which_registries_are_insecure() {
        let cert = Certificate {
            encoding: CertificateEncoding::Pem,
            data: b"x".to_vec(),
        };
        let config = client_config(&["10.0.2.2:5000".to_string()], vec![cert]);
        assert_eq!(config.extra_root_certificates.len(), 1);
        assert!(matches!(config.protocol, ClientProtocol::HttpsExcept(ref e) if e.len() == 1));
    }

    #[test]
    fn a_payload_that_is_not_an_event_is_dropped_not_fatal() {
        assert!(ResolverSupervisor::decode(b"not an archived event").is_none());
        assert!(ResolverSupervisor::decode(&[]).is_none());
    }

    #[test]
    fn an_event_that_was_archived_decodes_to_the_same_event() {
        let event = ProvenanceEvent::OCIArtifactDiscovered {
            uri: "ghcr.io/org/app:1".to_string(),
            source_ref: polar::DiscoverySourceRef::OCIRepository {
                repo_uri: "ghcr.io/org/app".to_string(),
            },
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
        let decoded = ResolverSupervisor::decode(&bytes).expect("it decodes");
        match decoded {
            ProvenanceEvent::OCIArtifactDiscovered { uri, .. } => {
                assert_eq!(uri, "ghcr.io/org/app:1")
            }
            other => panic!("expected the discovery, got {other:?}"),
        }
    }
}
