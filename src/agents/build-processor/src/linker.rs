use chrono::Utc;
use neo4rs::BoltType;
use oci_client::manifest::OciManifest;
use polar::graph::controller::IntoGraphKey;
use polar::graph::controller::{GraphControllerMsg, GraphOp, GraphValue, Property, rel};
use polar::graph::nodes::builds::ArtifactNodeKey;
use polar::graph::nodes::builds::BuildNodeKey;
use polar::{
    ArtifactProducedPayload, BinaryLinkedPayload, ContainerImageCreatedPayload, PackageRef,
    PackageStatusFoundPayload, ProvenanceEvent, SbomGraphFragment, SecurityAdvisoryFoundPayload,
};
use ractor::ActorRef;
use ractor::async_trait;
use ractor::{Actor, ActorProcessingErr};
use tracing::{debug, trace, warn};

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

    /// Atomicity note: each RawQuery is one transaction, so each GROUP is now
    /// atomic (all component nodes, or none). The groups are still separate
    /// transactions relative to each other — this is group-atomic, not
    /// SBOM-atomic. Full-SBOM atomicity would require a single multi-statement
    /// transaction, which is the argument for a typed batch variant later; it is
    /// deliberately not done here to keep each query eyeball-verifiable.
    ///
    /// Label/rel assumptions carried over from the existing code — verify against
    /// your ArtifactNodeKey::Package cypher_match output:
    ///   - Package node label is `Package`, MERGE key is `purl`.
    ///   - Dependency edge rel type is rel::DEPENDS_ON ("DEPENDS_ON").
    /// If cypher_match renders Package with a different label, the string literals
    /// below must match it (RawQuery bypasses cypher_match entirely).
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

        // ── 1. Sbom node + type/describe edges (singletons, per-op path) ─────────
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

        Self::ensure_edge(
            state,
            ArtifactNodeKey::Artifact,
            sbom_k.clone(),
            rel::IS,
            vec![],
        )?;

        if let Some(ref root) = fragment.root {
            let root_k = ArtifactNodeKey::Package {
                purl: root.purl.clone(),
            };
            // Root node is upserted as part of the batched component set below
            // (it may or may not also appear in `components`; MERGE dedupes). We
            // still write the DESCRIBES edge here as a singleton.
            Self::ensure_edge(state, sbom_k.clone(), root_k, rel::DESCRIBES, vec![])?;
        }

        // ── 2. Batched Package node upserts (root + all components) ──────────────
        // One UNWIND over a rows list. Root is folded in so it can't be created as
        // a bare purl-only stub by the DESCRIBES edge above (MERGE dedupes on purl).
        //
        //   UNWIND $rows AS row
        //   MERGE (p:Package { purl: row.purl })
        //   SET p.name = row.name,
        //       p.version = row.version,
        //       p.component_type = row.component_type,
        //       p.license = row.license,
        //       p.source = row.source
        //
        // license/source are intrinsic to a specific (name, version) — unlike
        // `scope` (which is relative to a specific SBOM and would need to live
        // on an edge, not here) — so they're correctly Package node properties.
        // Both are Option<String> on PackageRef; absent values become
        // GraphValue::Null via the same From<GraphValue> for BoltType path the
        // rest of this map already goes through, not a separate construction.
        {
            let mut rows: Vec<GraphValue> = Vec::with_capacity(fragment.components.len() + 1);

            let opt_str = |v: &Option<String>| -> GraphValue {
                match v {
                    Some(s) => GraphValue::String(s.clone()),
                    None => GraphValue::Null,
                }
            };

            let push_pkg = |p: &PackageRef, rows: &mut Vec<GraphValue>| {
                rows.push(GraphValue::Map(vec![
                    ("purl".into(), GraphValue::String(p.purl.clone())),
                    ("name".into(), GraphValue::String(p.name.clone())),
                    ("version".into(), GraphValue::String(p.version.clone())),
                    (
                        "component_type".into(),
                        GraphValue::String(p.component_type.clone()),
                    ),
                    ("license".into(), opt_str(&p.license)),
                    ("source".into(), opt_str(&p.source)),
                ]));
            };
            if let Some(ref root) = fragment.root {
                push_pkg(root, &mut rows);
            }
            for comp in &fragment.components {
                push_pkg(comp, &mut rows);
            }

            if !rows.is_empty() {
                let cypher = "\
                    UNWIND $rows AS row \
                    MERGE (p:Package { purl: row.purl }) \
                    SET p.name = row.name, \
                        p.version = row.version, \
                        p.component_type = row.component_type, \
                        p.license = row.license, \
                        p.source = row.source"
                    .to_string();

                let params = vec![("rows".to_string(), BoltType::from(GraphValue::List(rows)))];
                Self::send_op(
                    state,
                    GraphOp::RawQuery { cypher, params },
                    "Failed to batch-upsert package nodes",
                )?;
            }
        }

        // ── 3. Root → direct-dependency edges (batched) ─────────────────────────
        // Same fallback logic as before: prefer the authoritative root entry in
        // `edges`; if absent, treat every component as a direct dep. Difference is
        // only in expression — the chosen dep purls become one UNWIND row set.
        if let Some(ref root) = fragment.root {
            let dep_purls: Vec<String> =
                match fragment.edges.iter().find(|e| e.from_ref == root.purl) {
                    Some(edge) => edge.to_refs.clone(),
                    None => {
                        warn!(
                            "SBOM {} has no dependency entry for root purl {}, \
                         falling back to flat linkage",
                            fragment.filename, root.purl
                        );
                        fragment.components.iter().map(|c| c.purl.clone()).collect()
                    }
                };

            if !dep_purls.is_empty() {
                // UNWIND $deps AS dep
                // MERGE (r:Package { purl: $root })
                // MERGE (d:Package { purl: dep })
                // MERGE (r)-[:DEPENDS_ON]->(d)
                let cypher = format!(
                    "UNWIND $deps AS dep \
                     MERGE (r:Package {{ purl: $root }}) \
                     MERGE (d:Package {{ purl: dep }}) \
                     MERGE (r)-[:{rel}]->(d)",
                    rel = rel::DEPENDS_ON,
                );
                let params = vec![
                    ("root".to_string(), BoltType::from(root.purl.clone())),
                    (
                        "deps".to_string(),
                        BoltType::from(
                            dep_purls
                                .into_iter()
                                .map(BoltType::from)
                                .collect::<Vec<_>>(),
                        ),
                    ),
                ];
                Self::send_op(
                    state,
                    GraphOp::RawQuery { cypher, params },
                    "Failed to batch root dependency edges",
                )?;
            }
        }

        // ── 4. Full dependency tree edges (batched) ─────────────────────────────
        // Flatten every (from_ref -> to_ref) pair into one row set. Preserves the
        // exact edges the per-op loop wrote; MERGE on both endpoints keeps it safe
        // even if a purl wasn't in `components` (same as EnsureEdge did).
        {
            let mut pairs: Vec<GraphValue> = Vec::new();
            for edge in &fragment.edges {
                for to_ref in &edge.to_refs {
                    pairs.push(GraphValue::Map(vec![
                        ("from".into(), GraphValue::String(edge.from_ref.clone())),
                        ("to".into(), GraphValue::String(to_ref.clone())),
                    ]));
                }
            }

            if !pairs.is_empty() {
                // UNWIND $pairs AS pair
                // MERGE (a:Package { purl: pair.from })
                // MERGE (b:Package { purl: pair.to })
                // MERGE (a)-[:DEPENDS_ON]->(b)
                let cypher = format!(
                    "UNWIND $pairs AS pair \
                     MERGE (a:Package {{ purl: pair.from }}) \
                     MERGE (b:Package {{ purl: pair.to }}) \
                     MERGE (a)-[:{rel}]->(b)",
                    rel = rel::DEPENDS_ON,
                );
                let params = vec![("pairs".to_string(), BoltType::from(GraphValue::List(pairs)))];
                Self::send_op(
                    state,
                    GraphOp::RawQuery { cypher, params },
                    "Failed to batch dependency tree edges",
                )?;
            }
        }

        debug!(
            "SbomAnalyzed: batched {} package nodes + {} dependency edges for {}",
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
            Self::ensure_edge(state, artifact_k.clone(), sbom_k, rel::ANALYZED_AS, vec![])?;
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
        } else if payload.artifact_type == "oci-image" {
            Self::send_op(
                state,
                GraphOp::EnsureEdge {
                    from: ArtifactNodeKey::BuildArtifact {
                        content_hash: payload.artifact_content_hash.clone(),
                    }
                    .into_key(),
                    rel_type: rel::INSTANCE_OF.to_string(),
                    to: ArtifactNodeKey::OCIArtifact {
                        digest: payload.artifact_content_hash.clone(),
                    }
                    .into_key(),
                    props: vec![],
                },
                "failed to write INSTANCE_OF edge to OCIArtifact",
            )?;
        }

        // ── Edge: BuildJob -[:PRODUCED]-> artifact ─────────────────────────────
        // Only written when the event carries build_id — pipeline emissions
        // always have one; observer agent emissions don't.
        if let Some(ref build_id) = payload.build_id {
            Self::send_op(
                state,
                GraphOp::EnsureEdge {
                    from: BuildNodeKey::BuildJob {
                        build_id: build_id.clone(),
                    }
                    .into_key(),
                    rel_type: rel::PRODUCED.to_string(),
                    to: artifact_k.into_key(),
                    props: vec![],
                },
                "failed to write PRODUCED edge",
            )?;
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

        let mut props: Vec<Property> = vec![
            Property(
                "name".into(),
                GraphValue::String(payload.binary_name.clone()),
            ),
            Property(
                "observed_at".into(),
                GraphValue::String(Utc::now().to_rfc3339()),
            ),
        ];
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

    /// Graph topology produced:
    ///
    ///   (Artifact)-[:IS]->(ContainerImage {config_digest})
    ///   (ContainerImage {config_digest})
    ///       -[:HAS_LAYER {order: 0}]->(OCILayer {digest: diff_id_0})
    ///       -[:HAS_LAYER {order: 1}]->(OCILayer {digest: diff_id_1})
    ///       -[:HAS_LAYER {order: 2}]->(OCILayer {digest: diff_id_2})
    ///
    /// This happens at build time, before any registry push. The
    /// ContainerImage node exists from the moment nix-build-image
    /// completes. Later, when the image is uploaded and the resolver
    /// (or image.linked event) provides the manifest digest, we add:
    ///
    ///   (OCIArtifact {digest: manifest_digest})-[:REFERS_TO]->(ContainerImage {config_digest})
    ///
    /// The ContainerImage is the stable join point. The OCIArtifact is
    /// the registry-specific handle. This separation matters because:
    ///   - Same content pushed to GitLab and ACR = two OCIArtifacts,
    ///     one ContainerImage.
    ///   - Retagging an image = new OCIArtifact (new manifest digest
    ///     due to new tag), same ContainerImage.
    ///   - Rebuilding with same inputs on Nix = same ContainerImage
    ///     (deterministic config digest), new OCIArtifacts.
    pub(crate) fn handle_container_image_created(
        state: &mut ProvenanceLinkerState,
        payload: ContainerImageCreatedPayload,
    ) -> Result<(), ActorProcessingErr> {
        trace!(
            "Processing ContainerImageCreated: {} ({} layers)",
            payload.image_name,
            payload.layers.len()
        );

        // 1. Upsert an OCIArtifact node keyed on config digest.
        let image_k = ArtifactNodeKey::OCIArtifact {
            digest: payload.digest.clone(),
        };

        let mut props: Vec<Property> = vec![
            Property(
                "name".into(),
                GraphValue::String(payload.image_name.clone()),
            ),
            Property(
                "tarball_hash".into(),
                GraphValue::String(payload.tarball_hash.clone()),
            ),
            Property("uri".into(), GraphValue::String(payload.uri.clone())),
        ];
        if !payload.os.is_empty() {
            props.push(Property(
                "os".into(),
                GraphValue::String(payload.os.clone()),
            ));
        }
        if !payload.arch.is_empty() {
            props.push(Property(
                "arch".into(),
                GraphValue::String(payload.arch.clone()),
            ));
        }
        if !payload.created.is_empty() {
            props.push(Property(
                "created".into(),
                GraphValue::String(payload.created.clone()),
            ));
        }
        if !payload.entrypoint.is_empty() {
            props.push(Property(
                "entrypoint".into(),
                GraphValue::String(payload.entrypoint.clone()),
            ));
        }
        if !payload.cmd.is_empty() {
            props.push(Property(
                "cmd".into(),
                GraphValue::String(payload.cmd.clone()),
            ));
        }

        if !payload.repo_tags.is_empty() {
            // Store tags as a comma-separated string. Neo4j doesn't
            // have great support for array properties in MERGE, and
            // tags are informational, not identity.
            let tags_str = payload.repo_tags.join(",");
            props.push(Property(
                "repo_tags".into(),
                GraphValue::String(tags_str.into()),
            ));
        }

        Self::upsert_node(
            state,
            image_k.clone(),
            props,
            "Failed to upsert ContainerImage node",
        )?;

        // 2. Type hierarchy: (Artifact)-[:IS]->(ContainerImage)
        Self::ensure_edge(
            state,
            ArtifactNodeKey::Artifact,
            image_k.clone(),
            rel::IS,
            vec![],
        )?;

        // 3. Create OCILayer nodes and HAS_LAYER edges with ordering.
        //    These use the uncompressed diff ID as identity. If the
        //    resolver later creates OCILayer nodes keyed on compressed
        //    digest, the linker will need to reconcile them via the
        //    config's rootfs.diff_ids mapping. For now, diff ID is
        //    the canonical layer identity for locally-built images.
        for layer in &payload.layers {
            let layer_k = ArtifactNodeKey::OCILayer {
                digest: layer.diff_id.clone(),
            };

            Self::upsert_node(
                state,
                layer_k.clone(),
                vec![],
                "Failed to upsert OCILayer node",
            )?;

            Self::ensure_edge(
                state,
                image_k.clone(),
                layer_k,
                rel::HAS_LAYER,
                vec![Property(
                    "order".into(),
                    GraphValue::I64(layer.order as i64),
                )],
            )?;
        }

        debug!(
            "ContainerImageCreated: {} with {} layers (config: {})",
            payload.image_name,
            payload.layers.len(),
            payload.config_digest,
        );

        Ok(())
    }

    /// Project a SecurityAdvisoryFound event as one atomic transaction:
    ///   MERGE advisory node + SET props
    ///   MERGE (:Finding) + (advisory)-[:IS]->(:Finding)
    ///   MERGE (:BuildJob) + (build)-[:REPORTED]->(advisory)
    ///   [only if affected_package present]
    ///     MERGE (:Package) + (advisory)-[:AFFECTS]->(package)
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_security_advisory_found(
        state: &mut ProvenanceLinkerState,
        p: SecurityAdvisoryFoundPayload,
    ) -> Result<(), ActorProcessingErr> {
        trace!(
            "Processing SecurityAdvisoryFound: {identifier} ({severity})",
            identifier = p.identifier,
            severity = p.severity,
        );

        // Base query: advisory node, its Finding type edge, and the BuildJob
        // that reported it. `SET a += $props` overwrites only the keys we
        // supply — identifier is also the MERGE key so it's stable.
        let mut cypher = String::from(
            "MERGE (a:SecurityAdvisory { identifier: $identifier }) \
                SET a += $props \
                MERGE (f:Finding { type: \"Finding\" }) \
                MERGE (a)-[:IS]->(f) \
                MERGE (j:BuildJob { build_id: $build_id }) \
                MERGE (j)-[:REPORTED { at: $observed_at }]->(a)",
        );

        // Property map SET onto the advisory node. Only non-null fields are
        // included, so absent optionals don't write literal nulls. `severity`
        // and `identifier` always present.
        let mut props: Vec<(String, BoltType)> = vec![
            ("identifier".into(), p.identifier.clone().into()),
            ("severity".into(), p.severity.into()),
        ];
        // NB: fix_version carries a semver CONSTRAINT (">=x"), not a concrete
        // version — stored under this name to match the wire field.
        for (k, v) in [
            ("kind", p.kind),
            ("scanner", p.scanner),
            ("cve_id", p.cve_id),
            ("ghsa_id", p.ghsa_id),
            ("fix_version", p.fix_version),
            ("unaffected_constraint", p.unaffected_constraint),
            ("advisory_url", p.advisory_url),
        ] {
            if let Some(val) = v {
                props.push((k.into(), val.into()));
            }
        }

        let mut params: Vec<(String, BoltType)> = vec![
            ("identifier".into(), p.identifier.into()),
            ("build_id".into(), p.build_id.into()),
            ("observed_at".into(), Utc::now().to_rfc3339().into()),
            (
                "props".into(),
                BoltType::Map(props.into_iter().map(|(k, v)| (k.into(), v)).collect()),
            ),
        ];

        // Conditionally append the AFFECTS edge. Built as a string fragment
        // rather than written with a null purl — MERGE (:Package { purl: null })
        // would create or match a garbage node. Only fires when the scanner's
        // (name, version) resolved to a real SBOM purl in scanning.nu.
        if let Some(purl) = p.affected_package {
            cypher.push_str(
                " MERGE (p:Package { purl: $affected_purl }) \
                     MERGE (a)-[:AFFECTS { at: $observed_at }]->(p)",
            );
            params.push(("affected_purl".into(), purl.into()));
        }

        Self::send_op(
            state,
            GraphOp::RawQuery { cypher, params },
            "Failed to project SecurityAdvisoryFound",
        )
    }

    /// Project a PackageStatusFound event as one atomic transaction. Same shape
    /// as the advisory, but the package edge is CONCERNS (not AFFECTS) — a
    /// maintenance-status fact is not an active exploitable flaw. No severity.
    pub(crate) fn handle_package_status_found(
        state: &mut ProvenanceLinkerState,
        p: PackageStatusFoundPayload,
    ) -> Result<(), ActorProcessingErr> {
        trace!(
            "Processing PackageStatusFound: {identifier} ({kind})",
            identifier = p.identifier,
            kind = p.kind
        );

        let mut cypher = String::from(
            "MERGE (s:PackageStatus { identifier: $identifier }) \
                SET s += $props \
                MERGE (f:Finding { type: \"Finding\" }) \
                MERGE (s)-[:IS]->(f) \
                MERGE (j:BuildJob { build_id: $build_id }) \
                MERGE (j)-[:REPORTED { at: $observed_at }]->(s)",
        );

        let mut props: Vec<(String, BoltType)> = vec![
            ("identifier".into(), p.identifier.clone().into()),
            ("kind".into(), p.kind.into()),
            ("package_name".into(), p.package_name.into()),
            ("package_version".into(), p.package_version.into()),
        ];
        for (k, v) in [("scanner", p.scanner), ("advisory_url", p.advisory_url)] {
            if let Some(val) = v {
                props.push((k.into(), val.into()));
            }
        }

        let mut params: Vec<(String, BoltType)> = vec![
            ("identifier".into(), p.identifier.into()),
            ("build_id".into(), p.build_id.into()),
            ("observed_at".into(), Utc::now().to_rfc3339().into()),
            (
                "props".into(),
                BoltType::Map(props.into_iter().map(|(k, v)| (k.into(), v)).collect()),
            ),
        ];

        if let Some(purl) = p.affected_package {
            cypher.push_str(
                " MERGE (p:Package { purl: $affected_purl }) \
                     MERGE (s)-[:CONCERNS { at: $observed_at }]->(p)",
            );
            params.push(("affected_purl".into(), purl.into()));
        }

        Self::send_op(
            state,
            GraphOp::RawQuery { cypher, params },
            "Failed to project PackageStatusFound",
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
                .. // disregard source ref here
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
            ProvenanceEvent::ContainerImageCreated(e) => {
                debug!("Container Image created");
                Self::handle_container_image_created(state, e)?;
            }
            ProvenanceEvent::SecurityAdvisoryFound(e) => {
                Self::handle_security_advisory_found(state, e)?;
            }
            ProvenanceEvent::PackageStatusFound(e) => {
                Self::handle_package_status_found(state, e)?;
            }
            _ => warn!("unexpected linker command {message:?}"),
        }
        Ok(())
    }
}
