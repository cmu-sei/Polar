use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use oci_client::{
    client::{Certificate, CertificateEncoding, ClientConfig},
    manifest::{OciImageManifest, OciManifest},
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
use std::{env, str::FromStr};
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

                // subscribe the dispatcher
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

type RegistryCredentials = (String, String);

impl ResolverAgent {
    pub fn get_registry_credentials() -> Result<RegistryCredentials, ActorProcessingErr> {
        let username = env::var("REGISTRY_USERNAME")?;
        let password = env::var("REGISTRY_PASSWORD")?;

        Ok((username, password))
    }

    pub fn deserialize_payload(payload: Vec<u8>) -> Option<ProvenanceEvent> {
        if let Ok(event) = rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(&payload) {
            Some(event)
        } else {
            warn!("Failed to deserialize message into provenance event.");
            None
        }
    }

    fn build_oci_auth(reference: &Reference) -> RegistryAuth {
        use docker_credential::CredentialRetrievalError;
        use docker_credential::DockerCredential;
        let server = reference
            .resolve_registry()
            .strip_suffix('/')
            .unwrap_or_else(|| reference.resolve_registry());

        debug!("Loading configuration for regisry: {server}");
        // Retrieve oci registry credentials from docker config, if no credentials are found, use anonymous auth.
        // This implie a config.json is set somewhere
        // TODO: what if I don't want to read that?
        // Skopeo would've done the same thing, so I suppose this is a good practice to go with conventions.
        match docker_credential::get_credential(server) {
            Err(CredentialRetrievalError::ConfigNotFound) => RegistryAuth::Anonymous,
            Err(CredentialRetrievalError::NoCredentialConfigured) => RegistryAuth::Anonymous,
            Err(e) => panic!("Error handling docker configuration file: {e}"),
            Ok(DockerCredential::UsernamePassword(username, password)) => {
                debug!(username, password, "Found oci registry credentials");
                RegistryAuth::Basic(username, password)
            }
            Ok(DockerCredential::IdentityToken(_)) => {
                warn!("Cannot use contents of docker config, identity token not supported. Using anonymous auth");
                RegistryAuth::Anonymous
            }
        }
    }

    async fn inspect_image(image_ref: &str) -> Result<OciImageManifest, ActorProcessingErr> {
        debug!("attempting to resolve image: {image_ref}");
        // Parse the image reference, e.g., "ghcr.io/myorg/myimage:latest"
        let reference = oci_client::Reference::from_str(image_ref)?;

        let auth = ResolverAgent::build_oci_auth(&reference);

        // Create ORAS client (by default supports HTTPS and OCI registries)
        // optionally, read in a proxy cert
        //
        let client = match std::env::var("PROXY_CA_CERT") {
            Ok(cert_path) => {
                let data = get_file_as_byte_vec(&cert_path)?;
                info!("Configuring OCI client with proxy cert found at {cert_path}");
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

        client
            .auth(&reference, &auth, RegistryOperation::Pull)
            .await?;

        let (manifest, _digest, _config) =
            client.pull_manifest_and_config(&reference, &auth).await?;

        Ok(manifest)
    }
}

pub struct ResolverAgentArgs {
    pub cassini_client: ActorRef<TcpClientMessage>,
}

pub struct ResolverAgentState {
    pub cassini_client: ActorRef<TcpClientMessage>,
    pub web_client: WebClient,
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
        info!("ResolverAgent started");

        let state = ResolverAgentState {
            cassini_client: args.cassini_client,
            web_client: polar::get_web_client(),
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
                debug!("Received provenance event!");
                let image_data = ResolverAgent::inspect_image(&uri).await?;

                debug!("Resolved image: \n {img}", img = image_data.to_string());

                let digest = image_data.config.digest;
                let media_type = image_data.config.media_type;

                // forward image data to the linker
                let message = ProvenanceEvent::ImageRefResolved {
                    id,
                    digest,
                    media_type,
                };

                let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&message)?;

                state
                    .cassini_client
                    .cast(TcpClientMessage::Publish {
                        topic: PROVENANCE_LINKER_TOPIC.to_string(),
                        payload: payload.into(),
                    })
                    .ok();
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
