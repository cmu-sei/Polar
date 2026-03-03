use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use classifier::*;
use oci_client::{
    Client as OciClient, Reference,
    client::{Certificate, CertificateEncoding, ClientConfig},
    manifest::OciManifest,
    secrets::RegistryAuth,
};
use polar::{
    PROVENANCE_DISCOVERY_TOPIC, PROVENANCE_LINKER_TOPIC, ProvenanceEvent, Supervisor,
    SupervisorMessage, async_get_file_as_byte_vec, get_web_client, spawn_tcp_client,
    try_get_proxy_ca_cert,
};
use provenance_common::RESOLVER_SUPERVISOR_NAME;
use provenance_resolver::classifier;
use ractor::{
    Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait, registry::where_is,
};
use reqwest::Client as WebClient;
use std::str::FromStr;
use tracing::{debug, error, info, instrument, trace, warn};
pub const BROKER_CLIENT_NAME: &str = "polar.provenance.resolver.tcp";
use cassini_types::WireTraceCtx;
use serde::Deserialize;

#[derive(Debug, serde::Serialize, Deserialize, Clone)]
pub struct ResolverConfig {
    pub registries: Vec<RegistryConfig>,
}

#[derive(Debug, serde::Serialize, Deserialize, Clone)]
pub struct RegistryConfig {
    pub name: String,
    pub url: String,
    pub client_cert_path: Option<String>,
}

// --- Supervisor ---
pub struct ResolverSupervisor;

#[derive(Clone)]
pub struct ResolverSupervisorState {
    tcp_client: ActorRef<TcpClientMessage>,
    config: Option<ResolverConfig>,
    oci_client: Option<OciClient>,
}

impl Supervisor for ResolverSupervisor {
    #[instrument(level = "trace", skip(payload))]
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
                //lookup and forward
                trace!("Looking up actor {topic} and forwarding payload");
                if let Some(resolver) = where_is(topic.to_string()) {
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

impl ResolverSupervisor {
    async fn build_oci_client(
        state: &mut ResolverSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        let mut certs = Vec::new();

        // load proxy ca certificate
        try_get_proxy_ca_cert().await.inspect(|data| {
            debug!("Configuring OCI Client with PROXY_CA_CERT");
            certs.push(Certificate {
                encoding: CertificateEncoding::Pem,
                data: data.to_owned(),
            });
        });

        if let Some(config) = &state.config {
            for registry in &config.registries {
                if let Some(cert_path) = &registry.client_cert_path {
                    let data = async_get_file_as_byte_vec(cert_path).await?;
                    debug!("found certificate for {} at {cert_path}", registry.url);
                    certs.push(Certificate {
                        encoding: CertificateEncoding::Pem,
                        data,
                    });
                }
            }
        }

        state.oci_client = {
            if certs.is_empty() {
                debug!("Initializing OCI client without certificates");
                OciClient::default()
            } else {
                debug!(
                    "Initializing OCI client with {} extra root certificates",
                    certs.len()
                );
                OciClient::new(ClientConfig {
                    extra_root_certificates: certs,
                    ..ClientConfig::default()
                })
            }
        }
        .into();

        Ok(())
    }

    async fn load_resolver_config(
        state: &mut ResolverSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        let path =
            std::env::var("POLAR_RESOLVER_CONFIG").unwrap_or_else(|_| "resolver.json".to_string());

        debug!("Loading configuration from {path}");
        let bytes = async_get_file_as_byte_vec(&path).await?;

        match serde_json::from_slice::<ResolverConfig>(&bytes) {
            Ok(config) => {
                debug!(
                    "Loaded configuration from {path}: {:?} {}",
                    config,
                    serde_json::to_string_pretty(&config).unwrap_or_default()
                );
                state.config = config.into();
                Ok(())
            }
            Err(e) => Err(ActorProcessingErr::from(format!(
                "Failed to load resolver configuration! {e}"
            ))),
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

        let tcp_client = spawn_tcp_client(BROKER_CLIENT_NAME, myself, |ev| {
            Some(SupervisorMessage::ClientEvent { event: ev })
        })
        .await?;

        let state = ResolverSupervisorState {
            tcp_client,
            config: None,
            oci_client: None,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started.");
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
                    let web_client = get_web_client()?;

                    let (classifier, _) = Actor::spawn_linked(
                        Some("polar.provenance.classifier".to_string()),
                        ArtifactClassifier,
                        ArtifactClassifierState {
                            web_client: web_client.clone(),
                            tcp_client: state.tcp_client.clone(),
                        },
                        myself.clone().into(),
                    )
                    .await
                    .inspect_err(|e| error!("Failed to start classifier {e}"))?;

                    if let Err(e) = Self::load_resolver_config(state).await {
                        error!("Failed to load resolver configuration. {e}");
                        return Err(e);
                    }

                    if let Err(e) = Self::build_oci_client(state).await {
                        error!("Failed to build oci client {e}");
                        return Err(e);
                    }

                    let args = ResolverAgentState {
                        classifier: Some(classifier),
                        cassini_client: state.tcp_client.clone(),
                        config: state.config.clone().unwrap(),
                        web_client,
                        oci_client: state.oci_client.clone().unwrap(),
                    };

                    Actor::spawn_linked(
                        Some(PROVENANCE_DISCOVERY_TOPIC.to_string()),
                        ResolverAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await
                    .inspect_err(|e| error!("Failed to spawn resolver agent: {e:?}"))?;
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    Self::deserialize_and_dispatch(topic, payload)
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error: {reason}");
                    // TODO: Handle transport errors appropriately
                    // Ideally we
                    myself.stop(Some(reason))
                }
                ClientEvent::ControlResponse { .. } => {
                    // ignore
                }
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
            SupervisionEvent::ActorStarted(actor) => {
                debug!("{actor:?} started!");
            }
            _ => (),
        }
        Ok(())
    }
}

// --- Resolver Agent ---
pub struct ResolverAgent;

impl ResolverAgent {
    fn registry_from_image_ref(uri: &str) -> Option<String> {
        let first = uri.split('/').next()?;
        if first.contains('.') || first.contains(':') {
            Some(first.to_string())
        } else {
            None
        }
    }

    fn normalize_registry_host(s: &str) -> String {
        s.trim()
            .trim_end_matches('/')
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string()
    }

    /// Pure function: converts a repo name and optional tags into full image URIs.
    fn discover_images_from_tags(
        registry_host: &str,
        repo: &str,
        tags: Option<Vec<String>>,
    ) -> Vec<String> {
        tags.unwrap_or_default()
            .into_iter()
            .map(|tag| format!("{}/{}:{}", registry_host, repo, tag))
            .collect()
    }

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
                    debug!(
                        "docker config not found — skipping remaining candidates and using anonymous authentication"
                    );
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
                    error!("Error reading credentials for {}: {}", candidate, e);
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

    #[instrument(level = "debug", name = "resolver.inspect_image", skip(state))]
    async fn inspect_image(
        state: &mut ResolverAgentState,
        image_ref: &str,
    ) -> Result<Option<(OciManifest, String)>, ActorProcessingErr> {
        debug!("attempting to resolve image: {image_ref}");
        // Parse the image reference, e.g., "ghcr.io/myorg/myimage:latest"
        let reference = Reference::from_str(&image_ref)?;
        // TODO: Consider adding a "strict" mode to allow reading from unconfigured registriesq?
        // if !Self::registry_allowed(&state.config, &reference) {
        //     debug!("Skipping image from unconfigured registry: {}", image_ref);
        //     return Ok(None);
        // }

        let auth = Self::resolve_registry_auth(&reference)?;

        // I'm not really sure this is necessary,
        // not all registreis use Oauth2 and if there are some, we're going to have to figure out a way to tell the agent that it is
        // so this doesn't get in the way when HTTP basic will work fine
        // state
        //     .oci_client
        //     .auth(&reference, &auth, RegistryOperation::Pull)
        //     .await?;

        match state.oci_client.pull_manifest(&reference, &auth).await {
            Ok(response) => Ok(Some(response)),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }

    fn forward_event(
        state: &mut ResolverAgentState,
        event: ProvenanceEvent,
    ) -> Result<(), ActorProcessingErr> {
        trace!("Forwarding event to {PROVENANCE_LINKER_TOPIC}: {:?}", event);
        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&event)?;

        Ok(state.cassini_client.cast(TcpClientMessage::Publish {
            topic: PROVENANCE_LINKER_TOPIC.to_string(),
            payload: payload.into(),
            trace_ctx: WireTraceCtx::from_current_span(),
        })?)
    }

    fn emit_oci_resolved_event(
        state: &mut ResolverAgentState,
        uri: String,
        manifest: OciManifest,
        digest: String,
    ) -> Result<(), ActorProcessingErr> {
        // TODO: WE have an issue open to also represent image layers as nodoes connected to the artifact
        //  Which would enable extremely powerfyl analysis of the supply chain, but first we have to figure out this serialization problem.
        // Instead of stripping fields here, we should just reserailize the OCiManifest structure to bytes using serde, then rehydrate it on the other side to enable crawling
        // of the vector of layers and get additonal data

        if let Some(hostname) = Self::registry_from_image_ref(&uri) {
            // let event = ProvenanceEvent::OCIRegistryDiscovered {
            //     hostname: hostname.clone(),
            // };
            // Self::forward_event(state, event)?;

            //serialize manifest
            //
            let manifest_data = serde_json::to_vec(&manifest)?;

            // emit OCIArtifactResolved event
            let event = ProvenanceEvent::OCIArtifactResolved {
                uri: uri.clone(),
                digest: digest,
                manifest_data,
                registry: hostname,
            };

            Ok(Self::forward_event(state, event)?)
        } else {
            // tODO: I'm very interested to see how we'd fail here, considering that by the time we call this fn, we've already resolved it over the web.
            warn!("Could not extract registry hostname from image ref: {uri}");
            Ok(())
        }
    }
}

pub struct ResolverAgentState {
    pub cassini_client: ActorRef<TcpClientMessage>,
    pub classifier: Option<ActorRef<ArtifactClassifierMsg>>,
    pub web_client: WebClient,
    pub oci_client: OciClient,
    pub config: ResolverConfig,
}

#[async_trait]
impl Actor for ResolverAgent {
    type Msg = ProvenanceEvent;
    type State = ResolverAgentState;
    type Arguments = ResolverAgentState;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        Ok(args)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("Subscribing to topic {PROVENANCE_DISCOVERY_TOPIC}");
        state.cassini_client.cast(TcpClientMessage::Subscribe {
            topic: PROVENANCE_DISCOVERY_TOPIC.to_string(),
            trace_ctx: WireTraceCtx::from_current_span(),
        })?;

        // // fire-and-forget startup scrape
        // if let Err(e) = Self::startup_scrape(myself.clone(), state).await {
        //     warn!("Startup scrape failed: {}", e);
        // }

        Ok(())
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ProvenanceEvent::OCIArtifactDiscovered { uri } => {
                match Self::inspect_image(state, &uri).await {
                    Ok(Some((manifest, digest))) => {
                        debug!("Resolved image: \n {manifest:?}");

                        Self::emit_oci_resolved_event(state, uri, manifest, digest)?
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!("Failed to resolve image: {uri}, {e}");
                    }
                }
            }
            ProvenanceEvent::ImageRefDiscovered { uri } => {
                trace!("Received image ref discovered event");
                match Self::inspect_image(state, &uri).await {
                    Ok(Some((manifest, digest))) => {
                        debug!("Resolved image: \n {manifest:?}");

                        Self::emit_oci_resolved_event(
                            state,
                            uri.clone(),
                            manifest.clone(),
                            digest.clone(),
                        )?;

                        // Emit ImageRefResolved event (unchanged semantics)
                        let media_type = manifest.content_type().to_owned();
                        let event = ProvenanceEvent::ImageRefResolved {
                            uri,
                            digest,
                            media_type,
                        };
                        Self::forward_event(state, event)?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!("Failed to resolve image: {uri}, {e}");
                    }
                }
            }
            ProvenanceEvent::ArtifactDiscovered { name, url } => {
                trace!("Received ArtifactDiscovered directive");
                // forward for classification
                //
                state.classifier.as_ref().map(|c| {
                    c.cast(ArtifactClassifierMsg::Classify {
                        download_url: url,
                        filename: name,
                    })
                    .map_err(|e| error!("Failed to contact classifier: {c:?}. {e}"))
                });
            }
            _ => warn!("Received unexpected message! {msg:?}"),
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging(RESOLVER_SUPERVISOR_NAME.to_string());

    let (_supervisor, handle) = Actor::spawn(
        Some(RESOLVER_SUPERVISOR_NAME.to_string()),
        ResolverSupervisor,
        (),
    )
    .await
    .expect("Expected to start supervisor");

    handle.await.expect("Expected to finish supervisor");
    tokio::signal::ctrl_c().await.unwrap();
}

#[cfg(test)]
mod tests {
    use crate::{Reference, RegistryConfig, ResolverAgent, ResolverConfig};
    use std::str::FromStr;

    #[test]
    fn normalize_registry_host_variants() {
        let cases = [
            ("https://example.com/", "example.com"),
            ("http://example.com", "example.com"),
            ("example.com/", "example.com"),
            (" example.com ", "example.com"),
        ];

        for (input, expected) in cases {
            assert_eq!(ResolverAgent::normalize_registry_host(input), expected);
        }
    }

    #[test]
    fn build_registry_candidates_contains_expected_variants() {
        let candidates = ResolverAgent::build_registry_candidates("example.com");

        assert!(candidates.contains(&"example.com".to_string()));
        assert!(candidates.contains(&"https://example.com".to_string()));
        assert!(candidates.contains(&"example.com/v1/".to_string()));
    }

    #[test]
    fn test_discover_images_from_tags() {
        let uris = ResolverAgent::discover_images_from_tags(
            "registry.example.com",
            "foo/bar",
            Some(vec!["a".into(), "b".into()]),
        );
        assert_eq!(
            uris,
            vec![
                "registry.example.com/foo/bar:a",
                "registry.example.com/foo/bar:b"
            ]
        );

        // Handles None tags
        let uris =
            ResolverAgent::discover_images_from_tags("registry.example.com", "foo/bar", None);
        assert!(uris.is_empty());
    }
}
