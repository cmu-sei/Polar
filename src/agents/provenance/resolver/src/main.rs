use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use oci_client::{manifest::OciManifest, secrets::RegistryAuth, Client as OciClient, Reference};
use polar::{DispatcherMessage, ProvenanceEvent, DISPATCH_ACTOR, PROVENANCE_LINKER_TOPIC};
use provenance_common::{MessageDispatcher, RESOLVER_CLIENT_NAME, RESOLVER_SUPERVISOR_NAME};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort};
use reqwest::Client as WebClient;
use std::str::FromStr;
use tracing::{debug, error, info, trace, warn};

pub const BROKER_CLIENT_NAME: &str = "polar.provenance.resolver.tcp";

// --- Supervisor ---
pub struct ResolverSupervisor;

#[derive(Clone)]
pub struct ResolverSupervisorState {
    cassini_client: ActorRef<TcpClientMessage>,
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

        let (dispatcher, _) = Actor::spawn_linked(
            Some(DISPATCH_ACTOR.to_string()),
            MessageDispatcher,
            (),
            myself.clone().into(),
        )
        .await
        .map_err(|err| {
            error!("Failed to spawn dispatcher: {}", err);
            return ActorProcessingErr::from(err);
        })?;

        let registration_output = std::sync::Arc::new(OutputPort::default());

        //subscribe to registration event
        registration_output.subscribe(myself.clone(), |_registration_id| {
            //TODO: feel free to store the registration id in state in case the connection dies
            // so we can reconnect and resume session
            Some(ResolverSupervisorMsg::Initialize)
        });

        let queue_output = std::sync::Arc::new(OutputPort::default());

        // subscribe the dispatcher
        queue_output.subscribe(dispatcher, |payload| {
            // when we get messages off the queue, we need to deserialize it,
            // then forward directly to the resolver.
            // if it's NOT a provenance event, it probably isn't for us
            // In the future, it might be some new configuration, we'll have to handle this alter.
            // For now, though pass in an empty topic, as the dispatcher will discard it.
            Some(DispatcherMessage::Dispatch {
                message: payload,
                topic: RESOLVER_CLIENT_NAME.to_string(),
            })
        });

        let config = TCPClientConfig::new();

        return match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                output_port: registration_output,
                queue_output: queue_output,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok((client, _handle)) => {
                let state = ResolverSupervisorState {
                    cassini_client: client,
                };
                Ok(state)
            }
            Err(e) => Err(ActorProcessingErr::from(e)),
        };
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
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

                let (_resolver, _) = Actor::spawn_linked(
                    Some(RESOLVER_CLIENT_NAME.to_string()),
                    ResolverAgent,
                    args,
                    myself.clone().into(),
                )
                .await
                .expect("Failed to spawn resolver agent");
            }
            _ => {}
        }
        Ok(())
    }
}

// --- Resolver Agent ---
pub struct ResolverAgent;

impl ResolverAgent {
    fn build_oci_auth(reference: &Reference) -> RegistryAuth {
        use docker_credential::CredentialRetrievalError;
        use docker_credential::DockerCredential;
        let server = reference
            .resolve_registry()
            .strip_suffix('/')
            .unwrap_or_else(|| reference.resolve_registry());

        // Retrieve oci registry credentials from docker config, if no credentials are found, use anonymous auth.
        // This implie a config.json is set somewhere
        // TODO: what if I don't want to read that?
        // Skopeo would've done the same thing, so I suppose this is a good practice to go with conventions.
        match docker_credential::get_credential(server) {
            Err(CredentialRetrievalError::ConfigNotFound) => RegistryAuth::Anonymous,
            Err(CredentialRetrievalError::NoCredentialConfigured) => RegistryAuth::Anonymous,
            Err(e) => panic!("Error handling docker configuration file: {e}"),
            Ok(DockerCredential::UsernamePassword(username, password)) => {
                debug!(username, "Found oci registry credentials");
                RegistryAuth::Basic(username, password)
            }
            Ok(DockerCredential::IdentityToken(_)) => {
                warn!("Cannot use contents of docker config, identity token not supported. Using anonymous auth");
                RegistryAuth::Anonymous
            }
        }
    }

    async fn inspect_image(image_ref: &str) -> Result<OciManifest, ActorProcessingErr> {
        // Parse the image reference, e.g., "ghcr.io/myorg/myimage:latest"
        match oci_client::Reference::from_str(image_ref) {
            Ok(reference) => {
                let auth = ResolverAgent::build_oci_auth(&reference);

                // Create ORAS client (by default supports HTTPS and OCI registries)
                // TODO: what if we want HTTP, or some other configurations
                // Shouldn't we instantiate beforehand and keep it in state?
                let client = OciClient::default();

                // // Retrieve the manifest descriptor for this reference
                let (manifest, hash) = client
                    .pull_manifest(&reference, &auth)
                    .await
                    .expect("Cannot pull manifest");

                debug!("Resolved manifest for {image_ref} with hash {hash}");

                Ok(manifest)
            }
            Err(e) => return Err(ActorProcessingErr::from(e)),
        }
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
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Perform any post-start initialization tasks here
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
                let image_data = ResolverAgent::inspect_image(&uri).await?;

                // forward image data to the linker
                let message = ProvenanceEvent::ImageRefResolved {
                    id,
                    digest: image_data.to_string(),
                    media_type: image_data.content_type().to_owned(),
                };

                let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&message)
                    .expect("Expected to serialize message");

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
