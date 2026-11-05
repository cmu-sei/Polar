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

impl ResolverAgent {
    pub fn registry_from_image_ref(uri: &str) -> Option<String> {
        // Docker's rule: the first component is a registry only when a path follows it (a `/`) and it contains a dot
        // ("ghcr.io") or a colon ("localhost:5000"), or is "localhost". With no `/`, the whole reference is a name with its tag
        // or digest: the dots of a tag ("neo4j:5.26.2") or the colon of a digest ("app@sha256:...") are not a host, and taking
        // them for one sent "neo4j:5.26.2" to a registry as if it were qualified while "postgres:17.5" was skipped.
        let (first, rest) = uri.split_once('/')?;
        let is_host = first.contains('.') || first.contains(':') || first == "localhost";
        (is_host && !rest.is_empty()).then(|| first.to_string())
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
        let auth =
            auth::registry_auth(state.docker_config.as_deref(), reference.resolve_registry())?;

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
            ("localhost/repo:1", Some("localhost")),
            ("docker.io/library/postgres:17.5", Some("docker.io")),
            ("quay.io/keycloak/keycloak:26.2.4", Some("quay.io")),
            // A tag's dots and a digest's colon are not a host (seen on a cluster: "neo4j:5.26.2" was sent to a registry).
            ("neo4j:5.26.2", None),
            ("postgres:17.5", None),
            (
                "ws-image@sha256:74be7d88879d59479314d30d331733f73cd3ec640fe575a310c9aaa149a4ae6c",
                None,
            ),
            ("library/postgres:17.5", None),
        ] {
            assert_eq!(
                ResolverAgent::registry_from_image_ref(uri).as_deref(),
                registry,
                "{uri}"
            );
        }
    }
}
