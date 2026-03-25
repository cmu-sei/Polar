use chrono::Utc;
use oci_client::manifest::OciManifest;
use polar::ProvenanceEvent;
use polar::graph::controller::IntoGraphKey;
use polar::graph::controller::{GraphControllerMsg, GraphOp, GraphValue, Property, rel};
use ractor::ActorRef;
use ractor::async_trait;
use ractor::{Actor, ActorProcessingErr};
use tracing::{debug, trace, warn};

use crate::ArtifactNodeKey;

pub struct ProvenanceLinker;

pub struct ProvenanceLinkerState {
    compiler: ActorRef<GraphControllerMsg>,
}
pub struct ProvenanceLinkerArgs {
    pub compiler: ActorRef<GraphControllerMsg>,
}

impl ProvenanceLinker {
    fn send_op(
        state: &mut ProvenanceLinkerState,
        op: GraphOp,
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
        Self::send_op(
            state,
            GraphOp::UpsertNode {
                key: key.into_key(),
                props,
            },
            err_ctx,
        )
    }

    fn ensure_edge(
        state: &mut ProvenanceLinkerState,
        from: ArtifactNodeKey,
        to: ArtifactNodeKey,
        rel_type: &'static str,
        props: Vec<Property>,
    ) -> Result<(), ActorProcessingErr> {
        Self::send_op(
            state,
            GraphOp::EnsureEdge {
                from: from.into_key(),
                to: to.into_key(),
                rel_type: rel_type.to_string(),
                props,
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
                manifest_data,
                registry,
            } => {
                debug!("OCIArtifact resolved: {uri}");

                match serde_json::from_slice(&manifest_data)? {
                    OciManifest::Image(manifest) => {
                        let artifact_key = ArtifactNodeKey::OCIArtifact {
                            digest: digest.clone(),
                        };

                        let media_type = manifest.media_type.unwrap_or("null".to_string());

                        Self::upsert_node(
                            state,
                            artifact_key.clone(),
                            vec![
                                Property("digest".into(), GraphValue::String(digest)),
                                Property("uri".into(), GraphValue::String(uri)),
                                Property("media_type".into(), GraphValue::String(media_type)),
                                Property(
                                    "observed_at".into(),
                                    GraphValue::String(Utc::now().to_rfc3339()),
                                ),
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
                            rel::IS,
                            vec![],
                        )?;
                        Self::ensure_edge(
                            state,
                            registry_key.clone(),
                            ArtifactNodeKey::Artifact,
                            rel::CONTAINS,
                            vec![],
                        )?;

                        Self::ensure_edge(
                            state,
                            artifact_key.clone(),
                            registry_key,
                            rel::HOSTED_BY,
                            vec![],
                        )?;

                        // handle image layers
                        for layer in manifest.layers {
                            let layer_k = ArtifactNodeKey::OCILayer {
                                digest: layer.digest.clone(),
                            };

                            let urls = match layer.urls {
                                Some(urls) => urls
                                    .iter()
                                    .map(|url| GraphValue::String(url.to_owned()))
                                    .collect::<Vec<_>>(),
                                None => vec![],
                            };

                            let layer_props = vec![
                                Property(
                                    "media_type".to_string(),
                                    GraphValue::String(layer.media_type.clone()),
                                ),
                                Property("size".to_string(), GraphValue::I64(layer.size)),
                                Property("urls".to_string(), GraphValue::List(urls)), // TODO: What to do with these? these should give us an opportunity to chase downthe rest of the supply chain
                            ];

                            Self::upsert_node(
                                state,
                                layer_k.clone(),
                                layer_props,
                                "Failed to upset layer node",
                            )?;

                            Self::ensure_edge(
                                state,
                                artifact_key.clone(),
                                layer_k,
                                "HAS_LAYER",
                                vec![],
                            )?;
                        }
                    }
                    // Index and manifest are both artifacts
                    // They differ only in media type and outgoing relationships.
                    // Child manifests are addressable content
                    // We must create a stub node even if we haven’t fetched it yet.
                    // Otherwise we cannot traverse supply chains correctly.
                    // Platform belongs on descriptor edge
                    // The same manifest digest can appear in multiple indices with different descriptor metadata.
                    // Platform is not intrinsic to the manifest — it’s part of the descriptor.
                    // This supports recursive chasing:
                    // Later we can:
                    // detect child digests missing full manifest data
                    // enqueue them
                    // hydrate layers
                    // build a full closure
                    //
                    // TODO:
                    // we may want A boolean property is_index = true for faster filtering
                    // Or simply rely on media_type prefixes
                    // application/vnd.oci.image.index.v1+json
                    // application/vnd.docker.distribution.manifest.list.v2+json
                    OciManifest::ImageIndex(index) => {
                        let artifact_key = ArtifactNodeKey::OCIArtifact {
                            digest: digest.clone(),
                        };

                        let media_type = index.media_type.unwrap_or("null".to_string());

                        Self::upsert_node(
                            state,
                            artifact_key.clone(),
                            vec![
                                Property("digest".into(), GraphValue::String(digest.clone())),
                                Property("uri".into(), GraphValue::String(uri.clone())),
                                Property("media_type".into(), GraphValue::String(media_type)),
                                Property(
                                    "schema_version".into(),
                                    GraphValue::I64(index.schema_version as i64),
                                ),
                                Property(
                                    "artifact_type".into(),
                                    GraphValue::String(
                                        index.artifact_type.unwrap_or_else(|| "null".into()),
                                    ),
                                ),
                                Property(
                                    "observed_at".into(),
                                    GraphValue::String(Utc::now().to_rfc3339()),
                                ),
                            ],
                            "failed to upsert OCI index artifact",
                        )?;

                        let registry_key = ArtifactNodeKey::OCIRegistry {
                            hostname: registry.clone(),
                        };

                        Self::upsert_node(
                            state,
                            registry_key.clone(),
                            vec![Property(
                                "hostname".into(),
                                GraphValue::String(registry.clone()),
                            )],
                            "failed to upsert OCIRegistry",
                        )?;

                        // Type linkage
                        Self::ensure_edge(
                            state,
                            ArtifactNodeKey::Artifact,
                            artifact_key.clone(),
                            rel::IS,
                            vec![],
                        )?;

                        Self::ensure_edge(
                            state,
                            registry_key.clone(),
                            ArtifactNodeKey::Artifact,
                            rel::CONTAINS,
                            vec![],
                        )?;

                        Self::ensure_edge(
                            state,
                            artifact_key.clone(),
                            registry_key.clone(),
                            rel::HOSTED_BY,
                            vec![],
                        )?;

                        // Handle child manifests
                        for entry in index.manifests {
                            let child_key = ArtifactNodeKey::OCIArtifact {
                                digest: entry.digest.clone(),
                            };

                            // Upsert stub artifact node for referenced manifest.
                            // You may not have fetched it yet — that's fine.
                            Self::upsert_node(
                                state,
                                child_key.clone(),
                                vec![
                                    Property(
                                        "digest".into(),
                                        GraphValue::String(entry.digest.clone()),
                                    ),
                                    Property(
                                        "media_type".into(),
                                        GraphValue::String(entry.media_type.clone()),
                                    ),
                                    Property("size".into(), GraphValue::I64(entry.size)),
                                ],
                                "failed to upsert child manifest stub",
                            )?;

                            // Platform properties belong on the edge.
                            let mut edge_props = vec![
                                Property(
                                    "descriptor_media_type".into(),
                                    GraphValue::String(entry.media_type.clone()),
                                ),
                                Property("descriptor_size".into(), GraphValue::I64(entry.size)),
                            ];

                            if let Some(platform) = entry.platform {
                                edge_props.push(Property(
                                    "platform_os".into(),
                                    GraphValue::String(platform.os),
                                ));
                                edge_props.push(Property(
                                    "platform_arch".into(),
                                    GraphValue::String(platform.architecture),
                                ));

                                if let Some(variant) = platform.variant {
                                    edge_props.push(Property(
                                        "platform_variant".into(),
                                        GraphValue::String(variant),
                                    ));
                                }
                            }

                            Self::ensure_edge(
                                state,
                                artifact_key.clone(),
                                child_key,
                                "HAS_MANIFEST",
                                edge_props,
                            )?;
                        }
                    }
                }
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
                    rel::IS,
                    vec![],
                )?;
                Self::ensure_edge(
                    state,
                    ref_key.clone(),
                    artifact_key.clone(),
                    rel::INSTANCE_OF,
                    vec![],
                )?;
            }
            ProvenanceEvent::OCIRegistryDiscovered { hostname } => {
                debug!("OCI registry discovered: {hostname}");

                Self::upsert_node(
                    state,
                    ArtifactNodeKey::OCIRegistry {
                        hostname: hostname.clone(),
                    },
                    vec![],
                    "failed to upsert OCIRegistry",
                )?;
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

                Self::ensure_edge(
                    state,
                    ArtifactNodeKey::Artifact,
                    sbom_k.clone(),
                    rel::IS,
                    vec![],
                )?;

                for component in &sbom.components {
                    let component_k = ArtifactNodeKey::Component {
                        claim_type: component.component_type.clone(),
                        name: name.clone(),
                        version: component.version.clone(),
                    };

                    Self::ensure_edge(
                        state,
                        sbom_k.clone(),
                        component_k.clone(),
                        rel::DESCRIBES,
                        vec![],
                    )?;
                }
            }
            _ => warn!("unexpected linker command"),
        }
        Ok(())
    }
}
