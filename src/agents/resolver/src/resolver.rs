//! The OCI resolver itself: given a discovered image reference, pull its manifest from the
//! registry and publish what was found.

use crate::auth;
use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::WireTraceCtx;
use oci_client::{Client as OciClient, Reference, manifest::OciManifest};
use polar::{
    DiscoverySourceRef, ProvenanceEvent,
    cassini::{CassiniClient, TcpClient},
    topics::{KUBERNETES_RESOLUTION_EVENTS, PROVENANCE_EVENTS},
};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::path::PathBuf;
use std::str::FromStr;
use tracing::{debug, error, info, instrument, warn};

pub struct ResolverAgent;

impl ResolverAgent {
    pub fn registry_from_image_ref(uri: &str) -> Option<String> {
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
        let auth = auth::registry_auth(state.docker_config.as_deref(), reference.resolve_registry())?;

        match state.oci_client.pull_manifest(&reference, &auth).await {
            Ok(response) => Ok(Some(response)),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
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
            share_identity: true,
        })?)
    }
}

pub struct ResolverAgentState {
    pub cassini_client: TcpClient,
    pub oci_client: OciClient,
    /// Where registry credentials are looked up; the usual places when `None`.
    pub docker_config: Option<PathBuf>,
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
        info!("ready to resolve discovered image references");
        Ok(args)
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
                    Ok(Some((manifest, digest))) => {
                        debug!("Resolved image: \n {manifest:?}");

                        if let Some(hostname) = Self::registry_from_image_ref(&uri) {
                            let manifest_data = serde_json::to_vec(&manifest)?;

                            // construct new event, pass source_ref through
                            let event = ProvenanceEvent::OCIArtifactResolved {
                                uri: uri.clone(),
                                digest,
                                manifest_data,
                                registry: hostname,
                                source_ref: source_ref.clone(),
                            };

                            // depending on the source of the discovery, forward to appropriate parties
                            match source_ref {
                                DiscoverySourceRef::KubernetesPodContainer { .. } => {
                                    //forward to a k8s consumer

                                    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&event)?;

                                    state.cassini_client.publish(PublishRequest {
                                        topic: KUBERNETES_RESOLUTION_EVENTS.to_string(),
                                        payload: payload.into(),
                                        trace_ctx: WireTraceCtx::from_current_span(),
                                        offline_behavior: OfflineBehavior::default(),
                                        share_identity: true,
                                    })?;
                                }
                                DiscoverySourceRef::OCIRepository { .. } => (),
                            }

                            Self::forward_event(state, event)?
                        } else {
                            warn!("Could not extract registry hostname from image ref: {uri}");
                        }
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

#[cfg(test)]
mod tests {
    use super::ResolverAgent;

    #[test]
    fn an_image_with_a_registry_is_resolved_and_a_local_one_is_not() {
        for (uri, registry) in [
            ("ghcr.io/org/app:1", Some("ghcr.io")),
            ("localhost:5000/repo:1", Some("localhost:5000")),
            ("polar-nu-init:latest", None),
            ("ubuntu", None),
        ] {
            assert_eq!(ResolverAgent::registry_from_image_ref(uri).as_deref(), registry, "{uri}");
        }
    }
}
