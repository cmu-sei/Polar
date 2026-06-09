use cassini_client::{OfflineBehavior, PublishRequest, TcpClientMessage};
use cassini_types::ClientEvent;
use oci_client::{
    Client as OciClient, Reference,
    client::{Certificate, CertificateEncoding, ClientConfig},
    manifest::OciManifest,
    secrets::RegistryAuth,
};
use polar::{
    PROVENANCE_LINKER_TOPIC, ProvenanceEvent, Supervisor, SupervisorMessage,
    cassini::{CassiniClient, SubscribeRequest, TcpClient},
    topics::PROVENANCE_DISCOVERY,
    try_get_proxy_ca_cert,
};

use ractor::{
    Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait, registry::where_is,
};
use std::str::FromStr;
use tracing::{debug, error, info, instrument, trace, warn};

pub const BROKER_CLIENT_NAME: &str = "polar.oci.resolver.tcp";
pub const RESOLVER_SUPERVISOR_NAME: &str = "polar.oci.resolver.supervisor";

use cassini_types::WireTraceCtx;

// --- Supervisor ---
pub struct ResolverSupervisor;

#[derive(Clone)]
pub struct ResolverSupervisorState {
    tcp_client: TcpClient,
    oci_client: Option<OciClient>,
}

impl Supervisor for ResolverSupervisor {
    #[instrument(level = "trace", skip(payload))]
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
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

        try_get_proxy_ca_cert().await.inspect(|data| {
            debug!("Configuring OCI client with PROXY_CA_CERT");
            certs.push(Certificate {
                encoding: CertificateEncoding::Pem,
                data: data.to_owned(),
            });
        });

        state.oci_client = if certs.is_empty() {
            debug!("Initializing OCI client without extra certificates");
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
        .into();

        Ok(())
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

        let tcp_client = TcpClient::spawn(BROKER_CLIENT_NAME, myself, |ev| {
            Some(SupervisorMessage::ClientEvent { event: ev })
        })
        .await?;

        Ok(ResolverSupervisorState {
            tcp_client,
            oci_client: None,
        })
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
                    if let Err(e) = Self::build_oci_client(state).await {
                        error!("Failed to build OCI client: {e}");
                        return Err(e);
                    }

                    let args = ResolverAgentState {
                        cassini_client: state.tcp_client.clone(),
                        oci_client: state.oci_client.clone().unwrap(),
                    };

                    Actor::spawn_linked(
                        Some(PROVENANCE_DISCOVERY.to_string()),
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
                    myself.stop(Some(reason))
                }
                _ => (),
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
        // A hostname must either contain a dot (e.g. "ghcr.io") or a colon
        // with a port AND a path component (e.g. "localhost:5000/repo").
        // A bare "name:tag" with no slash in the URI is a Docker Hub short
        // ref, not a registry hostname.
        let has_dot = first.contains('.');
        let has_port = first.contains(':') && uri.contains('/');
        if has_dot || has_port {
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
                    // IdentityToken is not usable for manifest pulls; skip to next candidate.
                    warn!(
                        "IdentityToken returned for {} — not supported. Skipping.",
                        candidate
                    );
                    continue;
                }
                Err(CredentialRetrievalError::ConfigNotFound) => {
                    debug!("No docker config found — falling back to anonymous authentication");
                    return Ok(RegistryAuth::Anonymous);
                }
                Err(CredentialRetrievalError::NoCredentialConfigured) => {
                    debug!(
                        "No credentials configured for {} — falling back to anonymous authentication",
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
            "No usable credentials found — falling back to anonymous for {}",
            base
        );
        Ok(RegistryAuth::Anonymous)
    }

    fn build_registry_candidates(base: &str) -> Vec<String> {
        // Only HTTPS variants are generated. Plain HTTP is not supported;
        // see issue #<TBD> for rationale.
        vec![
            base.to_string(),
            format!("https://{}", base),
            format!("https://{}/", base),
            format!("{}/v1/", base),
            format!("https://{}/v1/", base),
        ]
    }

    #[instrument(level = "debug", name = "resolver.inspect_image", skip(state))]
    async fn inspect_image(
        state: &mut ResolverAgentState,
        image_ref: &str,
    ) -> Result<Option<(OciManifest, String)>, ActorProcessingErr> {
        // gaurd against unnecessary network calls by looking to see if a registry prefix is present.
        // images tagged like "polar-nu-init:latest" for instance, which are fine when using Orbstack for example, are valid
        // and can be used in its local cluster. So when the kubernetes processsor sees this and emits an event, we get exactly that.
        // To save ourselves the network call, this is an easy add.
        if Self::registry_from_image_ref(image_ref).is_none() {
            debug!(
                "Skipping image ref \"{image_ref}\": no registry component — likely a locally loaded image"
            );
            return Ok(None);
        }

        debug!("Attempting to resolve image: {image_ref}");
        let reference = Reference::from_str(image_ref)?;
        let auth = Self::resolve_registry_auth(&reference)?;

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

        Ok(state.cassini_client.publish(PublishRequest {
            topic: PROVENANCE_LINKER_TOPIC.to_string(),
            payload: payload.into(),
            trace_ctx: WireTraceCtx::from_current_span(),
            offline_behavior: OfflineBehavior::default(),
        })?)
    }

    fn emit_oci_resolved_event(
        state: &mut ResolverAgentState,
        uri: String,
        manifest: OciManifest,
        digest: String,
    ) -> Result<(), ActorProcessingErr> {
        // TODO: Represent image layers as nodes connected to the artifact for supply chain
        // analysis. Requires resolving the OciManifest serialization — serialize to bytes
        // with serde, rehydrate on the consumer side to crawl layers for additional data.

        if let Some(hostname) = Self::registry_from_image_ref(&uri) {
            let manifest_data = serde_json::to_vec(&manifest)?;

            let event = ProvenanceEvent::OCIArtifactResolved {
                uri: uri.clone(),
                digest,
                manifest_data,
                registry: hostname,
            };

            Ok(Self::forward_event(state, event)?)
        } else {
            warn!("Could not extract registry hostname from image ref: {uri}");
            Ok(())
        }
    }
}

pub struct ResolverAgentState {
    pub cassini_client: TcpClient,
    pub oci_client: OciClient,
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
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("Subscribing to topic {PROVENANCE_DISCOVERY}");
        state.cassini_client.subscribe(SubscribeRequest {
            topic: PROVENANCE_DISCOVERY.to_string(),
            trace_ctx: WireTraceCtx::from_current_span(),
        })?;

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
                trace!("Received ImageRefDiscovered event");
                match Self::inspect_image(state, &uri).await {
                    Ok(Some((manifest, digest))) => {
                        debug!("Resolved image: \n {manifest:?}");

                        Self::emit_oci_resolved_event(
                            state,
                            uri.clone(),
                            manifest.clone(),
                            digest.clone(),
                        )?;

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

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let (_supervisor, handle) = Actor::spawn(
        Some(RESOLVER_SUPERVISOR_NAME.to_string()),
        ResolverSupervisor,
        (),
    )
    .await
    .expect("Expected to start supervisor");

    tokio::select! {
        _ = handle => {
            debug!("Supervisor exited");
        }
        _ = tokio::signal::ctrl_c() => {
            debug!("Received shutdown signal");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Reference, ResolverAgent};
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
        assert!(!candidates.iter().any(|c| c.starts_with("http://")));
    }
}
