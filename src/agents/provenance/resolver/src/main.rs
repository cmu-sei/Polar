use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use oci_client::{
    client::{Certificate, CertificateEncoding, ClientConfig},
    manifest::OciManifest,
    secrets::RegistryAuth,
    Client as OciClient, Reference, RegistryOperation,
};
use polar::{
    get_file_as_byte_vec, ProvenanceEvent, Supervisor, SupervisorMessage,
    PROVENANCE_DISCOVERY_TOPIC, PROVENANCE_LINKER_TOPIC,
};
use provenance_common::RESOLVER_SUPERVISOR_NAME;
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, OutputPort,
    SupervisionEvent,
};
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
impl Supervisor for ResolverSupervisor {
    fn deserialize_and_dispatch(_topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
                //lookup and forward
                if let Some(resolver) = where_is(PROVENANCE_DISCOVERY_TOPIC.to_string()) {
                    resolver
                        .send_message(event)
                        .map_err(|e| error!("Failed to forward provenance event! {e}"))
                        .ok();
                }
            }
            Err(e) => {
                error!("Failed to deserialize provenance event! {e}");
            }
        }
    }
}

#[async_trait]
impl Actor for ResolverSupervisor {
    type Msg = SupervisorMessage;
    type State = ResolverSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let events_output = std::sync::Arc::new(OutputPort::default());

        //subscribe to registration event
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;

        let (cassini_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output,
            },
            myself.clone().into(),
        )
        .await?;

        let state = ResolverSupervisorState { cassini_client };

        Ok(state)
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let args = ResolverAgentArgs {
                        cassini_client: state.cassini_client.clone(),
                    };

                    let (_resolver, _) = Actor::spawn_linked(
                        Some(PROVENANCE_DISCOVERY_TOPIC.to_string()),
                        ResolverAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await?;
                }
                _ => {}
            },
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
            SupervisionEvent::ActorFailed(name, reason) => {
                error!("Actor {name:?} failed! {reason:?}");
                myself.stop(Some(reason.to_string()));
            }
            SupervisionEvent::ActorTerminated(name, _state, reason) => {
                warn!("Actor {name:?} terminated! {reason:?}");
                myself.stop(reason)
            }
            _ => {}
        }
        Ok(())
    }
}

// --- Resolver Agent ---
pub struct ResolverAgent;

impl ResolverAgent {
    fn resolve_registry_auth(reference: &Reference) -> Result<RegistryAuth, ActorProcessingErr> {
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
                    return Ok(RegistryAuth::Basic(u, p));
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
                    return Ok(RegistryAuth::Anonymous);
                }
                Err(CredentialRetrievalError::NoCredentialConfigured) => {
                    debug!(
                        "No credentials for key {} — using anonymous authentication",
                        candidate
                    );
                    return Ok(RegistryAuth::Anonymous);
                }
                Err(e) => {
                    error!("Error reading docker credentials for {}: {}", candidate, e);
                    return Err(ActorProcessingErr::from(e));
                }
            }
        }

        warn!(
            "No usable credentials — falling back to anonymous for {}",
            base
        );
        Ok(RegistryAuth::Anonymous)
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

        let auth = Self::resolve_registry_auth(&reference)?;

        client
            .auth(&reference, &auth, RegistryOperation::Pull)
            .await?;

        match client.pull_manifest(&reference, &auth).await {
            Ok(response) => Ok(response),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
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

                match ResolverAgent::inspect_image(&uri, &state.oci_client).await {
                    Ok((manifest, digest)) => {
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
                    Err(e) => {
                        warn!("Failed to resolve image: {uri}, {e}");
                    }
                }
            }
            _ => warn!("Received unexpected message! {msg:?}"),
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
