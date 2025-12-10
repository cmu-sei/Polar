use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use oci_client::{
    client::{Certificate, CertificateEncoding, ClientConfig},
    manifest::OciManifest,
    secrets::RegistryAuth,
    Client as OciClient, Reference, RegistryOperation,
};
use polar::{
    get_file_as_byte_vec, DispatcherMessage, ProvenanceEvent, QueueOutput, DISPATCH_ACTOR,
    PROVENANCE_DISCOVERY_TOPIC, PROVENANCE_LINKER_TOPIC,
};
use provenance_common::{MessageDispatcher, RESOLVER_CLIENT_NAME, RESOLVER_SUPERVISOR_NAME};
use ractor::{
    async_trait, factory::queues::Queue, Actor, ActorProcessingErr, ActorRef, OutputPort,
    SupervisionEvent,
};
use reqwest::Client as WebClient;
use rkyv::Resolver;
use std::{env, str::FromStr, sync::OnceLock};
use tracing::{debug, error, info, trace, warn};

pub const BROKER_CLIENT_NAME: &str = "polar.provenance.resolver.tcp";

// --- Supervisor ---
pub struct ResolverSupervisor;

#[derive(Clone)]
pub struct ResolverSupervisorState {
    cassini_client: ActorRef<TcpClientMessage>,
    queue_output: QueueOutput,
}

#[derive(Debug, Clone)]
pub enum ResolverSupervisorMsg {
    Initialize,
}

#[async_trait]
impl Actor for ResolverSupervisor {
    type Msg = ResolverSupervisorMsg;
    type State = ResolverSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let events_output = std::sync::Arc::new(OutputPort::default());
        let queue_output = std::sync::Arc::new(OutputPort::default());

        //subscribe to registration event
        events_output.subscribe(myself.clone(), |_registration_id| {
            //TODO: feel free to store the registration id in state in case the connection dies
            // so we can reconnect and resume session
            Some(ResolverSupervisorMsg::Initialize)
        });

        let config = TCPClientConfig::new()?;

        let (cassini_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output,
                queue_output: queue_output.clone(),
            },
            myself.clone().into(),
        )
        .await?;

        let state = ResolverSupervisorState {
            cassini_client,
            queue_output,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Perform any post-start initialization tasks here

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("Handling message: {:?}", msg);

        match msg {
            ResolverSupervisorMsg::Initialize => {
                let args = ResolverAgentArgs {
                    cassini_client: state.cassini_client.clone(),
                };

                let (resolver, _) = Actor::spawn_linked(
                    Some(PROVENANCE_DISCOVERY_TOPIC.to_string()),
                    ResolverAgent,
                    args,
                    myself.clone().into(),
                )
                .await?;

                state.queue_output.subscribe(resolver, |(message, _topic)| {
                    // when we get messages off the queue, we need to deserialize it,
                    // then forward directly to the resolver.
                    // if it's NOT a provenance event, it probably isn't for us
                    // In the future, it might be some new configuration, we'll have to handle this alter.
                    // For now, though pass in an empty topic, as the dispatcher will discard it.
                    ResolverAgent::deserialize_payload(message)
                });
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(name, _reason) => {
                error!("Actor {name:?} failed! {_reason:?}");
                myself.stop(None);
            }
            SupervisionEvent::ActorTerminated(name, state, reason) => {
                warn!("Actor {name:?} terminated! {reason:?}");
            }
            _ => {}
        }
        Ok(())
    }
}

// --- Resolver Agent ---
pub struct ResolverAgent;

impl ResolverAgent {
    fn deserialize_payload(payload: Vec<u8>) -> Option<ProvenanceEvent> {
        if let Ok(event) = rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(&payload) {
            Some(event)
        } else {
            warn!("Failed to deserialize message into provenance event.");
            None
        }
    }

    fn resolve_registry_auth(reference: &Reference) -> RegistryAuth {
        use docker_credential::{self, CredentialRetrievalError, DockerCredential};
        let registry = reference.resolve_registry();
        debug!("Resolving OCI credentials for registry: {}", registry);

        let base = Self::normalize_registry_host(registry);
        let candidates = Self::build_registry_candidates(&base);

        for candidate in candidates {
            debug!("Attempting docker-credential lookup for key: {}", candidate);

            match docker_credential::get_credential(&candidate) {
                Ok(DockerCredential::UsernamePassword(u, p)) => {
                    debug!(
                        username = u,
                        password = p,
                        "Resolved credentials from docker config for key {}",
                        candidate
                    );
                    return RegistryAuth::Basic(u, p);
                }
                Ok(DockerCredential::IdentityToken(_)) => {
                    // TODO: really? Should we error out here too?
                    warn!(
                        "IdentityToken returned for {} — unusable for ORAS. Skipping.",
                        candidate
                    );
                    continue;
                }
                Err(CredentialRetrievalError::ConfigNotFound) => {
                    debug!("docker config not found — skipping remaining candidates and using anonymous authentication");
                    return RegistryAuth::Anonymous;
                }
                Err(CredentialRetrievalError::NoCredentialConfigured) => {
                    debug!(
                        "No credentials for key {} — using anonymous authentication",
                        candidate
                    );
                    return RegistryAuth::Anonymous;
                }
                Err(e) => {
                    error!("Error reading docker credentials for {}: {}", candidate, e);
                    todo!("file was malformed or something, unrecoverable and should be handled.")
                }
            }
        }

        warn!(
            "No usable credentials — falling back to anonymous for {}",
            base
        );
        RegistryAuth::Anonymous
    }

    fn normalize_registry_host(s: &str) -> String {
        s.trim()
            .trim_end_matches('/')
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string()
    }

    fn build_registry_candidates(base: &str) -> Vec<String> {
        vec![
            base.to_string(),
            format!("https://{}", base),
            format!("http://{}", base),
            format!("{}/", base),
            format!("https://{}/", base),
            format!("http://{}/", base),
            format!("{}/v1/", base),
            format!("https://{}/v1/", base),
            format!("http://{}/v1/", base),
        ]
    }

    async fn inspect_image(
        image_ref: &str,
        client: &OciClient,
    ) -> Result<(OciManifest, String), ActorProcessingErr> {
        debug!("attempting to resolve image: {image_ref}");
        // Parse the image reference, e.g., "ghcr.io/myorg/myimage:latest"
        let reference = oci_client::Reference::from_str(image_ref)?;

        let auth = Self::resolve_registry_auth(&reference);

        client
            .auth(&reference, &auth, RegistryOperation::Pull)
            .await?;

        Ok(client.pull_manifest(&reference, &auth).await?)
    }
}

pub struct ResolverAgentArgs {
    pub cassini_client: ActorRef<TcpClientMessage>,
}

pub struct ResolverAgentState {
    pub cassini_client: ActorRef<TcpClientMessage>,
    pub web_client: WebClient,
    pub oci_client: OciClient,
}

#[async_trait]
impl Actor for ResolverAgent {
    type Msg = ProvenanceEvent;
    type State = ResolverAgentState;
    type Arguments = ResolverAgentArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let oci_client = match std::env::var("PROXY_CA_CERT") {
            Ok(cert_path) => {
                let data = get_file_as_byte_vec(&cert_path)?;
                debug!("Configuring OCI client with proxy cert found at {cert_path}");
                let cert = Certificate {
                    encoding: CertificateEncoding::Pem,
                    data,
                };
                let client_config = ClientConfig {
                    extra_root_certificates: vec![cert],
                    ..ClientConfig::default()
                };

                OciClient::new(client_config)
            }
            Err(_) => OciClient::default(),
        };

        let state = ResolverAgentState {
            cassini_client: args.cassini_client,
            web_client: polar::get_web_client(),
            oci_client,
        };
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        //subscribe to topic on the broker
        //
        state.cassini_client.cast(TcpClientMessage::Subscribe(
            myself
                .get_name()
                .expect("Expected actor to have been named."),
        ))?;
        Ok(())
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ProvenanceEvent::ImageRefDiscovered { id, uri } => {
                trace!("Received image ref discovered event!");
                let (manifest, digest) =
                    ResolverAgent::inspect_image(&uri, &state.oci_client).await?;

                debug!("Resolved image: \n {manifest:?}");

                let media_type = manifest.content_type().to_owned();

                // forward image data to the linker
                let message = ProvenanceEvent::ImageRefResolved {
                    id,
                    uri,
                    digest,
                    media_type,
                };

                let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&message)?;

                state.cassini_client.cast(TcpClientMessage::Publish {
                    topic: PROVENANCE_LINKER_TOPIC.to_string(),
                    payload: payload.into(),
                })?;
            }
            _ => todo!(),
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging();

    let (_supervisor, handle) = Actor::spawn(
        Some(RESOLVER_SUPERVISOR_NAME.to_string()),
        ResolverSupervisor,
        (),
    )
    .await
    .expect("Expected to start supervisor");

    handle.await.expect("Expected to finish supervisor");
}
