/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

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

/// Where an image reference says its image lives ([`ResolverAgent::image_source`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// A registry, by its name as Docker normalizes it (`docker.io` for a hostless name).
    Registry(String),
    /// The node's own image store (registry `localhost`): never fetched, never verified.
    NodeLocal,
}

impl ResolverAgent {
    /// Where an image reference says its image lives, by Docker's rule (`oci_client::Reference` is a port of Docker's
    /// normalize.go): a name with no registry host is Docker Hub's ("postgres:17.5" is docker.io/library/postgres), and one
    /// whose registry is `localhost` is in the node's own image store, as podman names what it builds and as Polar and Blitz
    /// name the images they build and load by hand. `localhost:5000` is a registry like any other. `None`: not a reference.
    pub fn image_source(uri: &str) -> Option<ImageSource> {
        let reference = Reference::from_str(uri).ok()?;
        Some(if reference.registry() == "localhost" {
            ImageSource::NodeLocal
        } else {
            ImageSource::Registry(reference.registry().to_string())
        })
    }

    #[instrument(level = "debug", name = "resolver.inspect_image", skip(state))]
    async fn inspect_image(
        state: &mut ResolverAgentState,
        image_ref: &str,
    ) -> Result<(OciManifest, String), ActorProcessingErr> {
        debug!("Attempting to resolve image: {image_ref}");
        let reference = Reference::from_str(image_ref)?;
        let auth =
            auth::registry_auth(state.docker_config.as_deref(), reference.resolve_registry())?;

        Ok(state.oci_client.pull_manifest(&reference, &auth).await?)
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
                let registry = match Self::image_source(&uri) {
                    Some(ImageSource::Registry(registry)) => registry,
                    // A node-local image fails closed. This resolver cannot see a node's image store (it may run on another
                    // node), so it never fetches one and never vouches for one: no event, and so no USES_IMAGE edge. A claim
                    // that an image is local can only take a verification away, never give one. The gap is in the graph as
                    // a PodContainer whose image names the localhost registry and has no USES_IMAGE edge (README, "Images
                    // in a node's own store").
                    Some(ImageSource::NodeLocal) => {
                        info!(
                            "not verified: {uri} is a node-local image (registry localhost), which no registry can vouch for"
                        );
                        return Ok(());
                    }
                    None => {
                        warn!("not verified: {uri} is not an image reference");
                        return Ok(());
                    }
                };
                match Self::inspect_image(state, &uri).await {
                    Ok((manifest, digest)) => {
                        debug!("Resolved image: \n {manifest:?}");
                        let manifest_data = serde_json::to_vec(&manifest)?;

                        // construct new event, pass source_ref through
                        let event = ProvenanceEvent::OCIArtifactResolved {
                            uri: uri.clone(),
                            digest,
                            manifest_data,
                            registry,
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
                    }
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
    fn a_reference_says_where_its_image_lives_by_dockers_rule() {
        use super::ImageSource::{NodeLocal, Registry};
        let registry = |r: &str| Some(Registry(r.to_string()));
        for (uri, source) in [
            // Hostless names are Docker Hub's, tags and digests included (a tag's dots are not a host).
            ("postgres:17.5", registry("docker.io")),
            ("neo4j:5.26.2", registry("docker.io")),
            ("ubuntu", registry("docker.io")),
            ("library/postgres:17.5", registry("docker.io")),
            ("ferronserver/ferron:3", registry("docker.io")),
            (
                "ws-image@sha256:74be7d88879d59479314d30d331733f73cd3ec640fe575a310c9aaa149a4ae6c",
                registry("docker.io"),
            ),
            // A registry host is the first component when a path follows it.
            ("ghcr.io/org/app:1", registry("ghcr.io")),
            ("quay.io/keycloak/keycloak:26.2.4", registry("quay.io")),
            ("docker.io/library/postgres:17.5", registry("docker.io")),
            ("localhost:5000/repo:1", registry("localhost:5000")),
            // `localhost` alone is the node's own image store.
            ("localhost/blitz-portal:latest", Some(NodeLocal)),
            ("localhost/polar-init:latest", Some(NodeLocal)),
            // Not a reference at all.
            ("UPPER/Case:bad tag", None),
        ] {
            assert_eq!(ResolverAgent::image_source(uri), source, "{uri}");
        }
    }
}
