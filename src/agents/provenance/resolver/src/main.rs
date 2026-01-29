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
use tracing::{debug, error, field::debug, info, instrument, trace, warn};
pub const BROKER_CLIENT_NAME: &str = "polar.provenance.resolver.tcp";
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
    cassini_client: ActorRef<TcpClientMessage>,
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
    fn load_resolver_config() -> Result<ResolverConfig, ActorProcessingErr> {
        debug!("Attemnpting to load configuration");
        let path =
            std::env::var("POLAR_RESOLVER_CONFIG").unwrap_or_else(|_| "resolver.json".to_string());

        let bytes = get_file_as_byte_vec(&path)?;

        match serde_json::from_slice::<ResolverConfig>(&bytes) {
            Ok(config) => {
                debug!(
                    "Loaded configuration from {path}: {:?} {}",
                    config,
                    serde_json::to_string_pretty(&config).unwrap_or_default()
                );
                Ok(config)
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
                    let config = Self::load_resolver_config()?;
                    let args = ResolverAgentArgs {
                        cassini_client: state.cassini_client.clone(),
                        config,
                    };

                    Actor::spawn_linked(
                        Some(PROVENANCE_DISCOVERY_TOPIC.to_string()),
                        ResolverAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await?;
                }
                ClientEvent::MessagePublished { topic, payload } => {
                    Self::deserialize_and_dispatch(topic, payload)
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error: {reason}");
                    // TODO: Handle transport errors appropriately
                    // Ideally we
                    myself.stop(Some(reason))
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
            _ => {}
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

    #[instrument(name = "resolver.scrape_registry", level = "trace")]
    async fn scrape_registry(
        myself: ActorRef<ProvenanceEvent>,
        tcp_client: ActorRef<TcpClientMessage>,
        web_client: &WebClient,
        registry: &RegistryConfig,
    ) -> Result<(), ActorProcessingErr> {
        let base = Self::normalize_registry_host(&registry.url);

        // Emit registry discovery backed by successful network reachability
        let event = ProvenanceEvent::OCIRegistryDiscovered {
            hostname: base.clone(),
        };

        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&event)?;
        tcp_client.cast(TcpClientMessage::Publish {
            topic: PROVENANCE_LINKER_TOPIC.to_string(),
            payload: payload.into(),
        })?;

        let catalog_url = format!("https://{}/v2/_catalog", base);
        debug!("Scraping registry catalog: {}", catalog_url);

        let resp = web_client.get(&catalog_url).send().await?;
        if !resp.status().is_success() {
            warn!(
                status = %resp.status(),
                registry = %registry.name,
                "Registry does not support catalog listing"
            );
            return Ok(());
        }

        #[derive(Deserialize)]
        struct Catalog {
            repositories: Vec<String>,
        }

        let catalog: Catalog = resp.json().await?;

        for repo in &catalog.repositories {
            let tags_url = format!("https://{}/v2/{}/tags/list", base, repo);
            let resp = web_client.get(&tags_url).send().await?;
            if !resp.status().is_success() {
                debug!(
                    status = %resp.status(),
                    repo,
                    "Skipping repository without tag listing"
                );
                continue;
            }

            #[derive(Deserialize)]
            struct Tags {
                tags: Option<Vec<String>>,
            }

            let tags: Tags = resp.json().await?;
            let uris = Self::discover_images_from_tags(&base, repo, tags.tags);

            for uri in uris {
                myself.cast(ProvenanceEvent::ImageRefDiscovered { uri })?;
            }
        }

        Ok(())
    }

    #[instrument(skip_all, name = "ArtifactResolver.startup_scrape", level = "trace")]
    async fn startup_scrape(
        myself: ActorRef<ProvenanceEvent>,
        state: &mut ResolverAgentState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Starting resolver startup scrape");

        for registry in &state.config.registries {
            if let Err(e) = Self::scrape_registry(
                myself.clone(),
                state.cassini_client.clone(),
                &state.web_client,
                registry,
            )
            .await
            {
                warn!(
                    registry = %registry.name,
                    error = %e,
                    "Registry scrape failed"
                );
            }
        }

        Ok(())
    }
    /// Check if the registry is allowed based on the configuration.
    fn registry_allowed(config: &ResolverConfig, reference: &Reference) -> bool {
        let registry = Self::normalize_registry_host(reference.resolve_registry());

        config
            .registries
            .iter()
            .any(|r| ResolverAgent::normalize_registry_host(&r.url) == registry)
    }

    fn build_oci_client(config: &ResolverConfig) -> Result<OciClient, ActorProcessingErr> {
        let mut certs = Vec::new();
        debug!("Attempting to load certificates");
        for registry in &config.registries {
            if let Some(cert_path) = &registry.client_cert_path {
                let data = get_file_as_byte_vec(cert_path)?;
                trace!("found certificate for {} at {cert_path}", registry.url);
                certs.push(Certificate {
                    encoding: CertificateEncoding::Pem,
                    data,
                });
            }
        }

        if certs.is_empty() {
            debug!("Initializing OCI client without certificates");
            Ok(OciClient::default())
        } else {
            debug!(
                "Initializing OCI client with {} extra root certificates",
                certs.len()
            );
            Ok(OciClient::new(ClientConfig {
                extra_root_certificates: certs,
                ..ClientConfig::default()
            }))
        }
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
        state
            .oci_client
            .auth(&reference, &auth, RegistryOperation::Pull)
            .await?;

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
        })?)
    }

    fn emit_oci_resolved_event(
        state: &mut ResolverAgentState,
        uri: String,
        manifest: OciManifest,
        digest: String,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(hostname) = Self::registry_from_image_ref(&uri) {
            let event = ProvenanceEvent::OCIRegistryDiscovered {
                hostname: hostname.clone(),
            };
            Self::forward_event(state, event)?;

            // emit OCIArtifactResolved event
            let event = ProvenanceEvent::OCIArtifactResolved {
                uri: uri.clone(),
                digest: digest.clone(),
                media_type: manifest.content_type().to_owned(),
                registry: hostname.clone(),
            };
            Ok(Self::forward_event(state, event)?)
        } else {
            // tODO: I'm very interested to see how we'd fail here, considering that by the time we call this fn, we've already resolved it over the web.
            warn!("Could not extract registry hostname from image ref: {uri}");
            Ok(())
        }
    }
}

pub struct ResolverAgentArgs {
    pub cassini_client: ActorRef<TcpClientMessage>,
    pub config: ResolverConfig,
}

pub struct ResolverAgentState {
    pub cassini_client: ActorRef<TcpClientMessage>,
    pub web_client: WebClient,
    pub oci_client: OciClient,
    pub config: ResolverConfig,
}

#[async_trait]
impl Actor for ResolverAgent {
    type Msg = ProvenanceEvent;
    type State = ResolverAgentState;
    type Arguments = ResolverAgentArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        let oci_client = Self::build_oci_client(&args.config)?;
        let state = ResolverAgentState {
            cassini_client: args.cassini_client,
            web_client: polar::get_web_client()?,
            oci_client,
            config: args.config,
        };
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("Subscribing to topic {PROVENANCE_DISCOVERY_TOPIC}");
        state.cassini_client.cast(TcpClientMessage::Subscribe(
            PROVENANCE_DISCOVERY_TOPIC.to_string(),
        ))?;

        // fire-and-forget startup scrape
        if let Err(e) = Self::startup_scrape(myself.clone(), state).await {
            warn!("Startup scrape failed: {}", e);
        }

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
    fn registry_allowed_matches_configured_registries() {
        let config = ResolverConfig {
            registries: vec![RegistryConfig {
                name: "test".into(),
                url: "https://registry.example.com".into(),
                client_cert_path: None,
            }],
        };

        let allowed = Reference::from_str("registry.example.com/repo:tag").unwrap();
        let denied = Reference::from_str("evil.com/repo:tag").unwrap();

        assert!(ResolverAgent::registry_allowed(&config, &allowed));
        assert!(!ResolverAgent::registry_allowed(&config, &denied));
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
