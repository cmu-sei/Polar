//! Build outputs: what a build produced — files, SBOMs and the dependency
//! graph they describe, binaries, container images.
//!
//! Reported by the build that produced them (instrumented pipeline stages,
//! `scripts/pipeline/core`).
//!
//! ```text
//! (Artifact)-[:IS]->(BuildArtifact | Sbom | Binary | OCIArtifact)
//! (BuildJob)-[:PRODUCED]->(BuildArtifact)
//! (BuildArtifact)-[:ANALYZED_AS]->(Sbom)                     sbom files
//! (BuildArtifact)-[:INSTANCE_OF]->(OCIArtifact)              pushed images
//! (Sbom)-[:DESCRIBES]->(Package)-[:DEPENDS_ON]->(Package)
//! (Binary)-[:BUILT_FROM]->(Package),  (Sbom)-[:ATTESTS]->(Binary)
//! (OCIArtifact)-[:HAS_DIFF_ID {order}]->(OCILayerDiffId)
//! ```
//!
//! Events about the same nodes may arrive in any order (e.g. BinaryLinked
//! before SbomAnalyzed). Every write MERGEs on identity, so whichever arrives
//! first creates the node and the other completes it.

use super::{edge, upsert};
use neo4rs::BoltType;
use polar::graph::controller::{GraphOp, GraphValue, Property, rel};
use polar::graph::nodes::builds::{ArtifactNodeKey, BuildNodeKey};
use polar::{
    ArtifactProducedPayload, BinaryLinkedPayload, ContainerImageCreatedPayload, PackageRef,
    SbomGraphFragment,
};
use tracing::{debug, trace, warn};

fn string(s: impl Into<String>) -> GraphValue {
    GraphValue::String(s.into())
}

fn type_edge(node: ArtifactNodeKey) -> GraphOp {
    edge(ArtifactNodeKey::Artifact, rel::IS, node, vec![])
}

/// A file the pipeline produced, keyed by content hash. Some artifact types
/// also get a node in their own domain:
/// - `sbom`: linked to the `Sbom` node that SbomAnalyzed writes for the same hash.
/// - `elf-binary`: a `Binary` node, which BinaryLinked attaches BUILT_FROM to.
/// - `oci-image`: the hash is the pushed manifest digest, so it is the
///   `OCIArtifact` the resolver records when the image is pulled.
pub fn artifact_produced(p: ArtifactProducedPayload) -> Vec<GraphOp> {
    trace!("ArtifactProduced: {} ({})", p.name, p.artifact_type);

    let artifact = ArtifactNodeKey::BuildArtifact {
        content_hash: p.artifact_content_hash.clone(),
    };
    let mut props = vec![Property("artifact_type".into(), string(&p.artifact_type))];
    if !p.name.is_empty() {
        props.push(Property("name".into(), string(&p.name)));
    }
    if !p.content_type.is_empty() {
        props.push(Property("content_type".into(), string(&p.content_type)));
    }

    let mut ops = vec![upsert(artifact.clone(), props), type_edge(artifact.clone())];

    match p.artifact_type.as_str() {
        // Connects "this pipeline produced this file" to "this file describes
        // package Y, which depends on Z".
        "sbom" => ops.push(edge(
            artifact.clone(),
            rel::ANALYZED_AS,
            ArtifactNodeKey::Sbom {
                artifact_content_hash: p.artifact_content_hash.clone(),
            },
            vec![],
        )),
        "elf-binary" => {
            let binary = ArtifactNodeKey::Binary {
                content_hash: p.artifact_content_hash.clone(),
            };
            let mut props = vec![Property("artifact_type".into(), string(&p.artifact_type))];
            if !p.name.is_empty() {
                props.push(Property("name".into(), string(&p.name)));
            }
            ops.push(upsert(binary.clone(), props));
            ops.push(type_edge(binary));
        }
        "oci-image" => ops.push(edge(
            artifact.clone(),
            rel::INSTANCE_OF,
            ArtifactNodeKey::OCIArtifact {
                digest: p.artifact_content_hash.clone(),
            },
            vec![],
        )),
        _ => {}
    }

    // Pipeline emissions always carry build_id; observer emissions don't.
    if let Some(build_id) = p.build_id {
        ops.push(edge(
            BuildNodeKey::BuildJob { build_id },
            rel::PRODUCED,
            artifact,
            vec![],
        ));
    }
    ops
}

/// An SBOM and the dependency graph it describes.
///
/// The package rows and dependency edges are batched into one UNWIND query
/// each, so one SBOM is a handful of transactions rather than one per
/// package. Each query is atomic on its own; the SBOM as a whole is not.
/// Full-SBOM atomicity would need a multi-statement transaction, which is
/// deliberately not done here to keep each query verifiable by eye.
///
/// The raw Cypher must agree with `ArtifactNodeKey::Package`'s label and key
/// (`Package {purl}`), since RawQuery bypasses the key's own Cypher.
pub fn sbom_analyzed(fragment: SbomGraphFragment) -> Vec<GraphOp> {
    trace!(
        "SbomAnalyzed: {} with {} components, {} edges",
        fragment.filename,
        fragment.components.len(),
        fragment.edges.len()
    );

    let sbom = ArtifactNodeKey::Sbom {
        artifact_content_hash: fragment.artifact_content_hash.clone(),
    };
    let mut ops = vec![
        upsert(
            sbom.clone(),
            vec![Property("filename".into(), string(&fragment.filename))],
        ),
        type_edge(sbom.clone()),
    ];

    // The root is upserted with the other packages below (MERGE dedupes on
    // purl); only the DESCRIBES edge is written here.
    if let Some(ref root) = fragment.root {
        ops.push(edge(
            sbom,
            rel::DESCRIBES,
            ArtifactNodeKey::Package {
                purl: root.purl.clone(),
            },
            vec![],
        ));
    }

    // ── Package nodes, root included ─────────────────────────────────────────
    // license and source are intrinsic to a (name, version), so they belong on
    // the Package node (unlike scope, which is relative to one SBOM and would
    // belong on an edge). archive_sha256 is first-writer-wins: a published
    // archive is immutable, so a later SBOM claiming a different digest for
    // the same purl must not replace the one analyses were pinned to.
    let opt_str = |v: &Option<String>| match v {
        Some(s) => GraphValue::String(s.clone()),
        None => GraphValue::Null,
    };
    let package_row = |p: &PackageRef| {
        GraphValue::Map(vec![
            ("purl".into(), string(&p.purl)),
            ("name".into(), string(&p.name)),
            ("version".into(), string(&p.version)),
            ("component_type".into(), string(&p.component_type)),
            ("license".into(), opt_str(&p.license)),
            ("source".into(), opt_str(&p.source)),
            ("archive_sha256".into(), opt_str(&p.archive_sha256)),
        ])
    };
    let rows: Vec<GraphValue> = fragment
        .root
        .iter()
        .chain(&fragment.components)
        .map(package_row)
        .collect();
    if !rows.is_empty() {
        ops.push(GraphOp::RawQuery {
            cypher: "UNWIND $rows AS row \
                 MERGE (p:Package { purl: row.purl }) \
                 SET p.name = row.name, \
                     p.version = row.version, \
                     p.component_type = row.component_type, \
                     p.license = row.license, \
                     p.source = row.source, \
                     p.archive_sha256 = coalesce(p.archive_sha256, row.archive_sha256)"
                .to_string(),
            params: vec![("rows".to_string(), BoltType::from(GraphValue::List(rows)))],
        });
    }

    // ── Root → direct dependencies ───────────────────────────────────────────
    // Prefer the root's own entry in `edges`; if the generator omitted it,
    // treat every component as a direct dependency.
    if let Some(ref root) = fragment.root {
        let deps: Vec<String> = match fragment.edges.iter().find(|e| e.from_ref == root.purl) {
            Some(e) => e.to_refs.clone(),
            None => {
                warn!(
                    "SBOM {} has no dependency entry for root purl {}, falling back to flat linkage",
                    fragment.filename, root.purl
                );
                fragment.components.iter().map(|c| c.purl.clone()).collect()
            }
        };
        if !deps.is_empty() {
            ops.push(GraphOp::RawQuery {
                cypher: format!(
                    "UNWIND $deps AS dep \
                     MERGE (r:Package {{ purl: $root }}) \
                     MERGE (d:Package {{ purl: dep }}) \
                     MERGE (r)-[:{rel}]->(d)",
                    rel = rel::DEPENDS_ON,
                ),
                params: vec![
                    ("root".to_string(), BoltType::from(root.purl.clone())),
                    (
                        "deps".to_string(),
                        BoltType::from(deps.into_iter().map(BoltType::from).collect::<Vec<_>>()),
                    ),
                ],
            });
        }
    }

    // ── The full dependency tree ─────────────────────────────────────────────
    // Every (from → to) pair. MERGE on both ends keeps it safe for purls that
    // aren't in `components`.
    let pairs: Vec<GraphValue> = fragment
        .edges
        .iter()
        .flat_map(|e| {
            e.to_refs.iter().map(|to| {
                GraphValue::Map(vec![
                    ("from".into(), string(&e.from_ref)),
                    ("to".into(), string(to)),
                ])
            })
        })
        .collect();
    let edge_count = pairs.len();
    if !pairs.is_empty() {
        ops.push(GraphOp::RawQuery {
            cypher: format!(
                "UNWIND $pairs AS pair \
                 MERGE (a:Package {{ purl: pair.from }}) \
                 MERGE (b:Package {{ purl: pair.to }}) \
                 MERGE (a)-[:{rel}]->(b)",
                rel = rel::DEPENDS_ON,
            ),
            params: vec![("pairs".to_string(), BoltType::from(GraphValue::List(pairs)))],
        });
    }

    debug!(
        "SbomAnalyzed: {} package nodes + {} dependency edges for {}",
        fragment.components.len() + fragment.root.iter().count(),
        edge_count,
        fragment.filename,
    );
    ops
}

/// A compiled binary, linked to the package it was built from and the SBOM
/// describing that package's dependencies. If this arrives before the SBOM,
/// MERGE creates a purl-only Package that SbomAnalyzed fills in later.
pub fn binary_linked(p: BinaryLinkedPayload, observed_at: &str) -> Vec<GraphOp> {
    trace!("BinaryLinked: {} -> {}", p.binary_name, p.root_purl);

    let binary = ArtifactNodeKey::Binary {
        content_hash: p.binary_content_hash.clone(),
    };
    let package = ArtifactNodeKey::Package {
        purl: p.root_purl.clone(),
    };

    let mut props = vec![
        Property("name".into(), string(&p.binary_name)),
        Property("observed_at".into(), string(observed_at)),
    ];
    if !p.binding_digest.is_empty() {
        props.push(Property("binding_digest".into(), string(&p.binding_digest)));
    }

    let mut ops = vec![
        upsert(binary.clone(), props),
        type_edge(binary.clone()),
        upsert(package.clone(), vec![]),
        edge(binary.clone(), rel::BUILT_FROM, package, vec![]),
    ];
    // A direct edge for convenience: Binary → Package ← Sbom is already
    // traversable, but this records that *this* SBOM describes *this* binary.
    if !p.sbom_content_hash.is_empty() {
        ops.push(edge(
            ArtifactNodeKey::Sbom {
                artifact_content_hash: p.sbom_content_hash.clone(),
            },
            rel::ATTESTS,
            binary,
            vec![],
        ));
    }
    debug!(
        "BinaryLinked: {} ({}) -> {}",
        p.binary_name, p.binary_content_hash, p.root_purl
    );
    ops
}

/// A container image the pipeline built and pushed, keyed by its pushed
/// manifest `digest`: the same `OCIArtifact` the resolver records when the
/// image is pulled, so build and registry facts land on one node.
///
/// Layers are recorded as the image config identifies them, by
/// *uncompressed* diff ID, under `HAS_DIFF_ID {order}`. The resolver records
/// the same layers as registry blobs, by *compressed* digest, under
/// `HAS_LAYER {order}` (see `registry`). Matching `order` on one artifact
/// pairs the two identities of a layer.
///
/// Without a digest there is no registry identity to key on; the event is
/// skipped rather than merging every such image into one `digest: ""` node.
pub fn container_image_created(p: ContainerImageCreatedPayload) -> Vec<GraphOp> {
    trace!(
        "ContainerImageCreated: {} ({} layers)",
        p.image_name,
        p.layers.len()
    );
    if p.digest.is_empty() {
        warn!(
            image = %p.image_name,
            "ContainerImageCreated without a manifest digest — not recorded"
        );
        return vec![];
    }

    let image = ArtifactNodeKey::OCIArtifact {
        digest: p.digest.clone(),
    };

    let mut props = vec![
        Property("name".into(), string(&p.image_name)),
        Property("tarball_hash".into(), string(&p.tarball_hash)),
        Property("uri".into(), string(&p.uri)),
    ];
    for (key, value) in [
        ("os", &p.os),
        ("arch", &p.arch),
        ("created", &p.created),
        ("entrypoint", &p.entrypoint),
        ("cmd", &p.cmd),
    ] {
        if !value.is_empty() {
            props.push(Property(key.into(), string(value)));
        }
    }
    // Comma-separated: tags are informational, not identity, and Neo4j list
    // properties are awkward under MERGE.
    if !p.repo_tags.is_empty() {
        props.push(Property("repo_tags".into(), string(p.repo_tags.join(","))));
    }

    let mut ops = vec![upsert(image.clone(), props), type_edge(image.clone())];
    for layer in &p.layers {
        let diff_id = ArtifactNodeKey::OCILayerDiffId {
            diff_id: layer.diff_id.clone(),
        };
        ops.push(upsert(diff_id.clone(), vec![]));
        ops.push(edge(
            image.clone(),
            rel::HAS_DIFF_ID,
            diff_id,
            vec![Property(
                "order".into(),
                GraphValue::I64(layer.order as i64),
            )],
        ));
    }
    debug!(
        "ContainerImageCreated: {} with {} layers (config: {})",
        p.image_name,
        p.layers.len(),
        p.config_digest,
    );
    ops
}
