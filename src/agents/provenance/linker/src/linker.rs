use chrono::Utc;
use oci_client::manifest::OciManifest;
use polar::graph::controller::IntoGraphKey;
use polar::graph::controller::{GraphControllerMsg, GraphOp, GraphValue, Property, rel};
use polar::{ArtifactProducedPayload, BinaryLinkedPayload, ProvenanceEvent, SbomGraphFragment};
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

    pub(crate) fn handle_sbom_analyzed(
        state: &mut ProvenanceLinkerState,
        fragment: SbomGraphFragment,
    ) -> Result<(), ActorProcessingErr> {
        trace!(
            "Processing SbomAnalyzed: {} with {} components, {} edges",
            fragment.filename,
            fragment.components.len(),
            fragment.edges.len()
        );

        // 1. Upsert the SBOM document node itself.
        let sbom_k = ArtifactNodeKey::Sbom {
            artifact_content_hash: fragment.artifact_content_hash.clone(),
        };
        Self::upsert_node(
            state,
            sbom_k.clone(),
            vec![Property(
                "filename".into(),
                GraphValue::String(fragment.filename.clone()),
            )],
            "Failed to upsert SBOM node",
        )?;

        // 2. (Artifact)-[:IS]->(Sbom) — type hierarchy edge.
        Self::ensure_edge(
            state,
            ArtifactNodeKey::Artifact,
            sbom_k.clone(),
            rel::IS,
            vec![],
        )?;

        // 3. Upsert the root package and link the SBOM to it.
        //    (Sbom)-[:DESCRIBES]->(Package {purl: root})
        //    The SBOM describes ONE package. Dependencies are linked
        //    via DEPENDS_ON between Package nodes, not from the SBOM.
        if let Some(ref root) = fragment.root {
            let root_k = ArtifactNodeKey::Package {
                purl: root.purl.clone(),
            };
            Self::upsert_node(
                state,
                root_k.clone(),
                vec![
                    Property("name".into(), GraphValue::String(root.name.clone())),
                    Property("version".into(), GraphValue::String(root.version.clone())),
                    Property(
                        "component_type".into(),
                        GraphValue::String(root.component_type.clone()),
                    ),
                ],
                "Failed to upsert root package node",
            )?;
            Self::ensure_edge(state, sbom_k.clone(), root_k, rel::DESCRIBES, vec![])?;
        }

        // 4. Upsert all dependency package nodes.
        //    We do this before edges so every node exists before we
        //    try to link them. This is idempotent — MERGE won't
        //    duplicate if the purl already exists from a prior build.
        for comp in &fragment.components {
            let comp_k = ArtifactNodeKey::Package {
                purl: comp.purl.clone(),
            };

            Self::upsert_node(
                state,
                comp_k,
                vec![
                    Property("name".into(), GraphValue::String(comp.name.clone())),
                    Property("version".into(), GraphValue::String(comp.version.clone())),
                    Property(
                        "component_type".into(),
                        GraphValue::String(comp.component_type.clone()),
                    ),
                ],
                "Failed to upsert component package node",
            )?;
        }

        // 4.5. Link the root package to its direct dependencies.
        //
        //      The `edges` list encodes the full tree, but we need to
        //      guarantee the root is connected to its direct deps even
        //      if the CycloneDX generator didn't include the root as a
        //      `ref` in the `dependencies` array. Some generators
        //      (including cargo-cyclonedx in certain versions) omit it.
        //
        //      Strategy: look for an edge where from_ref == root.purl.
        //      If found, those are the authoritative direct deps.
        //      If not found, every component is treated as a direct dep
        //      of the root — a flat fallback that's better than a gap.
        if let Some(ref root) = fragment.root {
            let root_k = ArtifactNodeKey::Package {
                purl: root.purl.clone(),
            };

            let root_edge = fragment.edges.iter().find(|e| e.from_ref == root.purl);

            match root_edge {
                Some(edge) => {
                    // Authoritative: the SBOM's dependency array explicitly
                    // lists what the root depends on.
                    for dep_purl in &edge.to_refs {
                        let dep_k = ArtifactNodeKey::Package {
                            purl: dep_purl.clone(),
                        };
                        Self::ensure_edge(state, root_k.clone(), dep_k, rel::DEPENDS_ON, vec![])?;
                    }
                }
                None => {
                    // Fallback: no explicit root entry in `dependencies`.
                    // Treat all components as direct deps of root. This
                    // loses the transitive/direct distinction but prevents
                    // an orphaned root node with no outgoing edges.
                    warn!(
                        "SBOM {} has no dependency entry for root purl {}, \
                                 falling back to flat linkage",
                        fragment.filename, root.purl
                    );
                    for comp in &fragment.components {
                        let comp_k = ArtifactNodeKey::Package {
                            purl: comp.purl.clone(),
                        };
                        Self::ensure_edge(state, root_k.clone(), comp_k, rel::DEPENDS_ON, vec![])?;
                    }
                }
            }
        }

        // 5. Write the dependency tree edges.
        //    These come from CycloneDX's `dependencies` array, which
        //    encodes the actual graph structure. Each edge says
        //    "from_ref depends on each of to_refs."
        //
        //    This is where the real value is — the old handler couldn't
        //    do this because it only had the flat component list. Now
        //    we get the full tree: direct deps, transitive deps, the
        //    whole thing.
        for edge in &fragment.edges {
            let from_k = ArtifactNodeKey::Package {
                purl: edge.from_ref.clone(),
            };
            for to_ref in &edge.to_refs {
                let to_k = ArtifactNodeKey::Package {
                    purl: to_ref.clone(),
                };
                Self::ensure_edge(state, from_k.clone(), to_k, rel::DEPENDS_ON, vec![])?;
            }
        }

        debug!(
            "SbomAnalyzed: wrote {} package nodes + {} dependency edges for {}",
            fragment.components.len() + fragment.root.iter().count(),
            fragment
                .edges
                .iter()
                .map(|e| e.to_refs.len())
                .sum::<usize>(),
            fragment.filename,
        );

        Ok(())
    }

    pub(crate) fn handle_artifact_produced(
        state: &mut ProvenanceLinkerState,
        payload: ArtifactProducedPayload,
    ) -> Result<(), ActorProcessingErr> {
        trace!(
            "Processing ArtifactProduced: {} ({})",
            payload.name, payload.artifact_type
        );

        // Upsert the build artifact node.
        let artifact_k = ArtifactNodeKey::BuildArtifact {
            content_hash: payload.artifact_content_hash.clone(),
        };
        let mut props: Vec<Property> = vec![Property(
            "artifact_type".into(),
            GraphValue::String(payload.artifact_type.clone()),
        )];
        if !payload.name.is_empty() {
            props.push(Property(
                "name".into(),
                GraphValue::String(payload.name.clone()),
            ));
        }
        if !payload.content_type.is_empty() {
            props.push(Property(
                "content_type".into(),
                GraphValue::String(payload.content_type.clone()),
            ));
        }
        Self::upsert_node(
            state,
            artifact_k.clone(),
            props,
            "Failed to upsert build artifact node",
        )?;

        // (Artifact)-[:IS]->(BuildArtifact) — type hierarchy.
        Self::ensure_edge(
            state,
            ArtifactNodeKey::Artifact,
            artifact_k.clone(),
            rel::IS,
            vec![],
        )?;

        // If this artifact is an SBOM, link it to the Sbom node
        // that will be (or was already) created by handle_sbom_analyzed.
        // The join key is the content hash — both events carry it.
        //
        //   (BuildArtifact {hash})-[:ANALYZED_AS]->(Sbom {hash})
        //
        // This edge is what connects provenance ("pipeline stage X
        // produced this file at time T") to the dependency graph
        // ("this file describes package Y which depends on Z").
        if payload.artifact_type == "sbom" {
            let sbom_k = ArtifactNodeKey::Sbom {
                artifact_content_hash: payload.artifact_content_hash.clone(),
            };
            Self::ensure_edge(state, artifact_k, sbom_k, rel::ANALYZED_AS, vec![])?;
        } else if payload.artifact_type == "elf-binary" {
            // Binary artifacts get a Binary node, not a generic
            // BuildArtifact. This is what handle_binary_linked
            // attaches BUILT_FROM edges to.
            let binary_k = ArtifactNodeKey::Binary {
                content_hash: payload.artifact_content_hash.clone(),
            };
            let mut props: Vec<Property> = vec![Property(
                "artifact_type".into(),
                GraphValue::String(payload.artifact_type.clone()),
            )];
            if !payload.name.is_empty() {
                props.push(Property(
                    "name".into(),
                    GraphValue::String(payload.name.clone()),
                ));
            }
            Self::upsert_node(
                state,
                binary_k.clone(),
                props,
                "Failed to upsert Binary node",
            )?;
            Self::ensure_edge(state, ArtifactNodeKey::Artifact, binary_k, rel::IS, vec![])?;
        }
        Ok(())
    }
    pub(crate) fn handle_binary_linked(
        state: &mut ProvenanceLinkerState,
        payload: BinaryLinkedPayload,
    ) -> Result<(), ActorProcessingErr> {
        trace!(
            "Processing BinaryLinked: {} -> {}",
            payload.binary_name, payload.root_purl
        );

        // 1. Upsert the Binary node.
        let binary_k = ArtifactNodeKey::Binary {
            content_hash: payload.binary_content_hash.clone(),
        };

        let mut props: Vec<Property> = vec![Property(
            "name".into(),
            GraphValue::String(payload.binary_name.clone()),
        )];
        if !payload.binding_digest.is_empty() {
            props.push(Property(
                "binding_digest".into(),
                GraphValue::String(payload.binding_digest.clone()),
            ))
        }

        Self::upsert_node(
            state,
            binary_k.clone(),
            props,
            "Failed to upsert Binary node",
        )?;

        // 2. Type hierarchy: (Artifact)-[:IS]->(Binary)
        Self::ensure_edge(
            state,
            ArtifactNodeKey::Artifact,
            binary_k.clone(),
            rel::IS,
            vec![],
        )?;

        // 3. The money edge: (Binary)-[:BUILT_FROM]->(Package)
        //    The Package node already exists — handle_sbom_analyzed
        //    created it when it processed the SBOM for this package.
        //    If events arrive out of order (binary.linked before
        //    sbom.analyzed), MERGE creates the Package node with just
        //    the purl, and handle_sbom_analyzed will SET the remaining
        //    properties when it runs. Idempotent and order-independent.
        let package_k = ArtifactNodeKey::Package {
            purl: payload.root_purl.clone(),
        };
        Self::upsert_node(
            state,
            package_k.clone(),
            vec![],
            "Failed to upsert Package node for binary linkage",
        )?;
        Self::ensure_edge(state, binary_k.clone(), package_k, rel::BUILT_FROM, vec![])?;

        // 4. Link the Binary to the SBOM that describes its deps.
        //    (SBOM)-[:ATTESTS]->(Binary)
        //    This is a direct edge for convenience — you can already
        //    traverse Binary → Package ← Sbom, but the direct edge
        //    makes queries simpler and encodes the fact that this
        //    specific binary's deps are described by this specific SBOM.
        if !payload.sbom_content_hash.is_empty() {
            let sbom_k = ArtifactNodeKey::Sbom {
                artifact_content_hash: payload.sbom_content_hash.clone(),
            };
            Self::ensure_edge(state, sbom_k, binary_k, rel::ATTESTS, vec![])?;
        }

        debug!(
            "BinaryLinked: {} ({}) -> {}",
            payload.binary_name, payload.binary_content_hash, payload.root_purl
        );

        Ok(())
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
            ProvenanceEvent::SbomAnalyzed(e) => {
                debug!("Sbom analyzed \n{e:?}");
                Self::handle_sbom_analyzed(state, e)?;
            }
            ProvenanceEvent::ArtifactProduced(e) => {
                debug!("Artifact produced! {e:?}");
                Self::handle_artifact_produced(state, e)?;
            }
            ProvenanceEvent::ArtifactDiscovered { name, url } => {
                debug!("Artifact discovered {name} , {url}")
            }
            ProvenanceEvent::BinaryLinked(e) => {
                debug!("Binary linked {e:?}");
                Self::handle_binary_linked(state, e)?;
            }

            _ => warn!("unexpected linker command {message:?}"),
        }
        Ok(())
    }
}
