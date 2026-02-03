use polar::graph::{self, rel, GraphValue};
use polar::graph::{GraphControllerMsg, GraphOp, Property};
use polar::ProvenanceEvent;
use ractor::async_trait;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use tracing::{trace, warn};

use crate::ArtifactNodeKey;

pub struct ProvenanceLinker;

pub struct ProvenanceLinkerState {
    compiler: ActorRef<GraphControllerMsg<ArtifactNodeKey>>,
}
pub struct ProvenanceLinkerArgs {
    pub compiler: ActorRef<GraphControllerMsg<ArtifactNodeKey>>,
}

impl ProvenanceLinker {
    fn send_op(
        state: &mut ProvenanceLinkerState,
        op: GraphOp<ArtifactNodeKey>,
        err_ctx: &'static str,
    ) -> Result<(), ActorProcessingErr> {
        state
            .compiler
            .send_message(GraphControllerMsg::Op(op))
            .map_err(|e| ActorProcessingErr::from(format!("{err_ctx}: {:?}", e)))
    }

    fn upsert_node(
        state: &mut ProvenanceLinkerState,
        key: ArtifactNodeKey,
        props: Vec<Property>,
        err_ctx: &'static str,
    ) -> Result<(), ActorProcessingErr> {
        Self::send_op(state, GraphOp::UpsertNode { key, props }, err_ctx)
    }

    fn ensure_edge(
        state: &mut ProvenanceLinkerState,
        from: ArtifactNodeKey,
        to: ArtifactNodeKey,
        rel_type: &'static str,
    ) -> Result<(), ActorProcessingErr> {
        Self::send_op(
            state,
            GraphOp::EnsureEdge {
                from,
                to,
                rel_type: rel_type.to_string(),
                props: vec![],
            },
            "failed to ensure edge",
        )
    }
}

#[async_trait]
impl Actor for ProvenanceLinker {
    type Msg = ProvenanceEvent;
    type State = ProvenanceLinkerState;
    type Arguments = ProvenanceLinkerArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Expect the supervisor to pass the compiler ActorRef via args.graph or separate arg.
        // For this example, assume ProvenanceLinkerArgs contains compiler_ref.
        Ok(ProvenanceLinkerState {
            // store compiler ref elsewhere
            compiler: args.compiler,
        })
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ProvenanceEvent::OCIArtifactResolved {
                uri,
                digest,
                media_type,
                registry,
            } => {
                trace!("OCIArtifact resolved: {uri}");

                let artifact_key = ArtifactNodeKey::OCIArtifact {
                    digest: digest.clone(),
                };

                Self::upsert_node(
                    state,
                    artifact_key.clone(),
                    vec![
                        Property("digest".into(), GraphValue::String(digest)),
                        Property("uri".into(), GraphValue::String(uri)),
                        Property("media_type".into(), GraphValue::String(media_type)),
                    ],
                    "failed to upsert OCIArtifact",
                )?;

                let registry_key = ArtifactNodeKey::OCIRegistry {
                    hostname: registry.clone(),
                };

                Self::upsert_node(
                    state,
                    registry_key.clone(),
                    vec![Property("hostname".into(), GraphValue::String(registry))],
                    "failed to upsert OCIRegistry",
                )?;

                // ensure edges between the "Artifat" type node, the OCIartifact itself, and the registry
                Self::ensure_edge(
                    state,
                    ArtifactNodeKey::Artifact,
                    artifact_key.clone(),
                    graph::rel::IS,
                )?;
                Self::ensure_edge(
                    state,
                    registry_key.clone(),
                    ArtifactNodeKey::Artifact,
                    graph::rel::CONTAINS,
                )?;

                Self::ensure_edge(state, artifact_key, registry_key, graph::rel::HOSTED_BY)?;
            }

            ProvenanceEvent::ImageRefResolved {
                uri,
                digest,
                media_type,
            } => {
                trace!("ImageRef resolved: {uri}");

                let ref_key = ArtifactNodeKey::ContainerImageRef {
                    normalized: uri.clone(),
                };

                Self::upsert_node(
                    state,
                    ref_key.clone(),
                    vec![Property("normalized".into(), GraphValue::String(uri))],
                    "failed to upsert ContainerImageReference",
                )?;

                let artifact_key = ArtifactNodeKey::OCIArtifact {
                    digest: digest.clone(),
                };

                Self::upsert_node(
                    state,
                    artifact_key.clone(),
                    vec![
                        Property("digest".into(), GraphValue::String(digest)),
                        Property("media_type".into(), GraphValue::String(media_type)),
                    ],
                    "failed to upsert OCIArtifact from ImageRefResolved",
                )?;

                // ensure edges between the "Artifat" type node, the OCIartifact itself, and the registry
                Self::ensure_edge(
                    state,
                    ArtifactNodeKey::Artifact,
                    artifact_key.clone(),
                    graph::rel::IS,
                )?;
                Self::ensure_edge(
                    state,
                    ref_key.clone(),
                    artifact_key.clone(),
                    graph::rel::INSTANCE_OF,
                )?;
            }
            ProvenanceEvent::OCIRegistryDiscovered { hostname } => {
                trace!("OCI registry discovered: {hostname}");

                Self::upsert_node(
                    state,
                    ArtifactNodeKey::OCIRegistry {
                        hostname: hostname.clone(),
                    },
                    vec![Property("hostname".into(), GraphValue::String(hostname))],
                    "failed to upsert OCIRegistry",
                )?;
            }
            ProvenanceEvent::PodContainerUsesImage {
                pod_uid,
                container_name,
                image_ref,
            } => {
                trace!(
                    "PodContainer {} / {} observed image ref {}",
                    pod_uid,
                    container_name,
                    image_ref
                );
                // 1. Upsert PodContainer (idempotent, no inference)
                let pod_container_key = ArtifactNodeKey::PodContainer {
                    pod_uid: pod_uid.clone(),
                    container_name: container_name.clone(),
                };
                let props = vec![
                    Property("pod_uid".into(), polar::graph::GraphValue::String(pod_uid)),
                    Property(
                        "name".into(),
                        polar::graph::GraphValue::String(container_name),
                    ),
                ];

                Self::upsert_node(
                    state,
                    pod_container_key.clone(),
                    props,
                    "Failed to upsert PodContainer",
                )?;

                // 2. Upsert ContainerImageReference (string-only claim)
                let image_ref_key = ArtifactNodeKey::ContainerImageRef {
                    normalized: image_ref.clone(),
                };

                let props = vec![Property(
                    "normalized".into(),
                    polar::graph::GraphValue::String(image_ref),
                )];
                Self::upsert_node(
                    state,
                    image_ref_key.clone(),
                    props,
                    "Failed to upsert ContainerImageReference",
                )?;

                // 3. Create PodContainer -> ImageRef edge
                let edge_op = GraphOp::EnsureEdge {
                    from: pod_container_key,
                    to: image_ref_key.clone(),
                    rel_type: "USES_IMAGE".to_string(),
                    props: vec![],
                };

                state
                    .compiler
                    .send_message(GraphControllerMsg::Op(edge_op))
                    .map_err(|e| {
                        ActorProcessingErr::from(format!(
                            "failed to create USES_IMAGE edge: {:?}",
                            e
                        ))
                    })?;
            }
            ProvenanceEvent::SbomResolved { uid, sbom, name } => {
                trace!("Received SBOMResolved directive");
                let sbom_k = ArtifactNodeKey::Sbom {
                    uid: uid.clone(),
                    sbom: sbom.clone(),
                };

                Self::upsert_node(
                    state,
                    sbom_k.clone(),
                    Vec::default(),
                    "Failed to upsert sbom_key in graph",
                )?;

                Self::ensure_edge(state, ArtifactNodeKey::Artifact, sbom_k.clone(), rel::IS)?;

                for component in &sbom.components {
                    let component_k = ArtifactNodeKey::Component {
                        claim_type: component.component_type.clone(),
                        name: name.clone(),
                        version: component.version.clone(),
                    };

                    Self::ensure_edge(state, sbom_k.clone(), component_k.clone(), rel::DESCRIBES)?;
                }
            }
            _ => warn!("unexpected linker command"),
        }
        Ok(())
    }
}
