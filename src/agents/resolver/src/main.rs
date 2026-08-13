use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::ClientEvent;
use oci_client::{
    Client as OciClient, Reference,
    client::{Certificate, CertificateEncoding},
    manifest::OciManifest,
    secrets::RegistryAuth,
};
use polar::{
    DiscoverySourceRef, ProvenanceEvent, Supervisor, SupervisorMessage,
    cassini::{CassiniClient, SubscribeRequest, TcpClient},
    health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage},
    topics::{KUBERNETES_RESOLUTION_EVENTS, PROVENANCE_DISCOVERY, PROVENANCE_EVENTS},
    try_get_proxy_ca_cert,
};

use ractor::{
    Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait,
    registry::where_is,
};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace, warn};

use oci_resolver::config::{ResolverConfig, qualify};

pub const BROKER_CLIENT_NAME: &str = "polar.oci.resolver.tcp";
pub const RESOLVER_SUPERVISOR_NAME: &str = "polar.oci.resolver.supervisor";
const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

use cassini_types::WireTraceCtx;

// --- Supervisor ---
pub struct ResolverSupervisor;

pub struct ResolverSupervisorState {
    tcp_client: TcpClient,
    oci_client: Option<OciClient>,
    config: Arc<ResolverConfig>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
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
        // Certificates discovered at runtime are *appended* to the configured
        // trust material rather than replacing the whole ClientConfig. The
        // previous implementation branched on `certs.is_empty()` and set
        // `protocol` on only one arm, so supplying a PROXY_CA_CERT silently
        // reverted the plaintext allowlist to TLS-everywhere.
        let mut runtime_certs = Vec::new();
        try_get_proxy_ca_cert().await.inspect(|data| {
            debug!("Configuring OCI client with PROXY_CA_CERT");
            runtime_certs.push(Certificate {
                encoding: CertificateEncoding::Pem,
                data: data.to_owned(),
            });
        });

        let client_config = state.config.client_config(runtime_certs)?;
        state.oci_client = Some(OciClient::new(client_config));
        Ok(())
    }
}

#[async_trait]
impl Actor for ResolverSupervisor {
    type Msg = SupervisorMessage;
    type State = ResolverSupervisorState;
    type Arguments = Arc<ResolverConfig>;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        config: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // Resolver never touches Neo4j -- Cassini-only.
        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: false,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(e))?;

        let tcp_client = TcpClient::spawn(BROKER_CLIENT_NAME, myself, |ev| {
            Some(SupervisorMessage::ClientEvent { event: ev })
        })
        .await?;

        Ok(ResolverSupervisorState {
            tcp_client,
            oci_client: None,
            config,
            healthcheck,
            draining: false,
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
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    if let Err(e) = Self::build_oci_client(state).await {
                        error!("Failed to build OCI client: {e}");
                        return Err(e);
                    }

                    let args = ResolverAgentState {
                        cassini_client: state.tcp_client.clone(),
                        oci_client: state.oci_client.clone().unwrap(),
                        config: state.config.clone(),
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
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                    } else {
                        Self::deserialize_and_dispatch(topic, payload)
                    }
                }
                // BUG FIX: was `myself.stop(Some(reason))` -- silent exit-0,
                // Job marks Completed, no restart, no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => (),
            },
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            SupervisorMessage::GraphSignal(_) => {}
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(name, reason) => {
                error!("Actor {name:?} failed! {reason:?}");
                // BUG FIX: was `myself.stop(Some(reason.to_string()))` --
                // silent exit-0 under Job (completions=1 marks an exit-0
                // pod Completed, never retried).
                if !state.draining {
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(
                            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
                            || SupervisorMessage::ForceExit,
                        )
                        .await;
                }
            }
            SupervisionEvent::ActorTerminated(name, _state, reason) => {
                let actor_name = name.get_name().unwrap_or_default();
                warn!("Actor {name:?} terminated! {reason:?}");
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(
                            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
                            || SupervisorMessage::ForceExit,
                        )
                        .await;
                } else if !state.draining {
                    // BUG FIX: was `myself.stop(reason)` unconditionally.
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(
                            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
                            || SupervisorMessage::ForceExit,
                        )
                        .await;
                }
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

/// Outcome of attempting to resolve a single discovered artifact.
enum Resolution {
    Resolved {
        reference: Reference,
        manifest: OciManifest,
        digest: String,
    },
    /// Unqualified ref and policy says skip. Not an error.
    SkippedUnqualified,
}

impl ResolverAgent {
    fn resolve_registry_auth(
        config: &ResolverConfig,
        reference: &Reference,
    ) -> Result<RegistryAuth, ActorProcessingErr> {
        use docker_credential::{self, CredentialRetrievalError, DockerCredential};

        let registry = reference.resolve_registry();
        debug!("Resolving OCI credentials for registry: {registry}");

        for candidate in config.credential_keys(registry) {
            trace!("Attempting docker-credential lookup for key: {candidate}");

            match docker_credential::get_credential(&candidate) {
                Ok(DockerCredential::UsernamePassword(u, p)) => {
                    debug!("Resolved credentials from docker config for key {candidate}");
                    return Ok(RegistryAuth::Basic(u, p));
                }
                Ok(DockerCredential::IdentityToken(_)) => {
                    // A Docker identity token is a refresh token for the
                    // registry's token endpoint, not a bearer token for the
                    // distribution API, so RegistryAuth::Bearer would fail. The
                    // proper fix is an oauth2 refresh exchange; until then, try
                    // the next key.
                    warn!("IdentityToken returned for {candidate} — unsupported, trying next key");
                    continue;
                }
                // No docker config at all: no candidate can succeed, stop.
                Err(CredentialRetrievalError::ConfigNotFound) => {
                    debug!("No docker config found for {registry}");
                    break;
                }
                // Per-key miss. The previous implementation returned Anonymous
                // here, which made the candidate loop dead code after its first
                // iteration and meant the Docker Hub key (which is the *last*
                // candidate, `https://index.docker.io/v1/`) was never tried.
                Err(CredentialRetrievalError::NoCredentialConfigured) => {
                    trace!("No credential configured for key {candidate}");
                    continue;
                }
                Err(e) => {
                    warn!("Error reading credentials for {candidate}: {e}");
                    continue;
                }
            }
        }

        if config.anonymous_fallback {
            debug!("No usable credentials for {registry} — falling back to anonymous");
            Ok(RegistryAuth::Anonymous)
        } else {
            Err(ActorProcessingErr::from(format!(
                "no credentials found for registry {registry} and auth.anonymousFallback is false"
            )))
        }
    }

    #[instrument(level = "debug", name = "resolver.inspect_image", skip(state))]
    async fn inspect_image(
        state: &mut ResolverAgentState,
        image_ref: &str,
    ) -> Result<Resolution, ActorProcessingErr> {
        // Unqualified refs (`polar-nu-init:latest`, `rancher/klipper-helm:v1`)
        // are Docker Hub shorthand as far as every other OCI client is
        // concerned. Default is to qualify them against the configured registry
        // and attempt resolution; `Skip` restores the previous short-circuit
        // for air-gapped deployments where the egress attempt is itself the
        // finding (cmu-sei/Polar#219).
        let Some(mut reference) = qualify(image_ref, &state.config.unqualified_refs)? else {
            debug!(
                "Skipping image ref {image_ref:?}: no registry component and \
                 registry.unqualifiedRefs is \"skip\""
            );
            return Ok(Resolution::SkippedUnqualified);
        };

        state.config.apply_mirror(&mut reference);

        debug!(
            "Attempting to resolve {image_ref:?} as {} (registry={}, repository={})",
            reference.whole(),
            reference.registry(),
            reference.repository()
        );

        let auth = Self::resolve_registry_auth(&state.config, &reference)?;

        let (manifest, digest) = state.oci_client.pull_manifest(&reference, &auth).await?;
        Ok(Resolution::Resolved {
            reference,
            manifest,
            digest,
        })
    }

    fn forward_event(
        state: &mut ResolverAgentState,
        event: ProvenanceEvent,
    ) -> Result<(), ActorProcessingErr> {
        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&event)?;

        Ok(state.cassini_client.publish(PublishRequest {
            topic: PROVENANCE_EVENTS.to_string(),
            payload: payload.into(),
            trace_ctx: WireTraceCtx::from_current_span(),
            offline_behavior: OfflineBehavior::default(),
        })?)
    }
}

pub struct ResolverAgentState {
    pub cassini_client: TcpClient,
    pub oci_client: OciClient,
    pub config: Arc<ResolverConfig>,
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
            ProvenanceEvent::OCIArtifactDiscovered { uri, source_ref } => {
                match Self::inspect_image(state, &uri).await {
                    Ok(Resolution::Resolved {
                        reference,
                        manifest,
                        digest,
                    }) => {
                        debug!("Resolved image: \n {manifest:?}");

                        // The registry comes from the same Reference used for
                        // the pull. Re-deriving it from the raw string, as the
                        // previous implementation did, both re-ran a heuristic
                        // that could disagree with the parser (dropping the
                        // event after a successful network round-trip) and
                        // emitted the un-normalised spelling — so
                        // `index.docker.io/library/redis` and
                        // `docker.io/library/redis` produced two distinct
                        // registry nodes in the graph for one registry.
                        let manifest_data = serde_json::to_vec(&manifest)?;

                        let event = ProvenanceEvent::OCIArtifactResolved {
                            uri: uri.clone(),
                            digest,
                            manifest_data,
                            registry: reference.registry().to_string(),
                            source_ref: source_ref.clone(),
                        };

                        match source_ref {
                            DiscoverySourceRef::KubernetesPodContainer { .. } => {
                                let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&event)?;

                                state.cassini_client.publish(PublishRequest {
                                    topic: KUBERNETES_RESOLUTION_EVENTS.to_string(),
                                    payload: payload.into(),
                                    trace_ctx: WireTraceCtx::from_current_span(),
                                    offline_behavior: OfflineBehavior::default(),
                                })?;
                            }
                            DiscoverySourceRef::OCIRepository { .. } => (),
                        }

                        Self::forward_event(state, event)?
                    }
                    Ok(Resolution::SkippedUnqualified) => {}
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

    // Fail fast and loudly. A resolver running with a different transport
    // policy than the operator believes is worse than one that refuses to boot.
    let config = Arc::new(
        ResolverConfig::load().unwrap_or_else(|e| panic!("Invalid resolver configuration: {e}")),
    );

    let (_supervisor, handle) = Actor::spawn(
        Some(RESOLVER_SUPERVISOR_NAME.to_string()),
        ResolverSupervisor,
        config,
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
