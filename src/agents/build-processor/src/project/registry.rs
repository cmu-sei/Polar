//! Registry: what a registry actually serves, as the resolver verified it.
//!
//! The resolver is the only agent that fetches manifests, and this module is
//! the only place that records them. Other agents' claims about images (k8s's
//! `PodContainer-[:USES_IMAGE]->OCIArtifact`, a build's
//! `BuildArtifact-[:INSTANCE_OF]->OCIArtifact`) meet these facts on the shared
//! `OCIArtifact {digest}` key. Whether the claims agree is a graph query, not
//! something decided here.
//!
//! ```text
//! (Artifact)-[:IS]->(OCIArtifact)-[:HOSTED_BY]->(OCIRegistry)-[:CONTAINS]->(Artifact)
//! (OCIArtifact)-[:HAS_LAYER {order}]->(OCILayer)          image manifests
//! (OCIArtifact)-[:HAS_MANIFEST {platform_*}]->(OCIArtifact)  image indexes
//! ```

use super::{edge, upsert};
use oci_client::manifest::OciManifest;
use polar::graph::controller::{GraphOp, GraphValue, Property, rel};
use polar::graph::nodes::builds::ArtifactNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

fn string(s: impl Into<String>) -> GraphValue {
    GraphValue::String(s.into())
}

/// The registry node and the artifact's place in the type hierarchy. Shared
/// by image manifests and indexes.
fn hosting(artifact: &ArtifactNodeKey, registry: String) -> Vec<GraphOp> {
    let registry_k = ArtifactNodeKey::OCIRegistry {
        hostname: registry.clone(),
    };
    vec![
        upsert(
            registry_k.clone(),
            vec![Property("hostname".into(), string(registry))],
        ),
        edge(ArtifactNodeKey::Artifact, rel::IS, artifact.clone(), vec![]),
        edge(
            registry_k.clone(),
            rel::CONTAINS,
            ArtifactNodeKey::Artifact,
            vec![],
        ),
        edge(artifact.clone(), rel::HOSTED_BY, registry_k, vec![]),
    ]
}

/// A manifest the resolver fetched for `digest`. Errors if `manifest_data`
/// isn't an OCI image manifest or index; nothing is written in that case.
pub fn artifact_resolved(
    uri: String,
    digest: String,
    manifest_data: &[u8],
    registry: String,
    observed_at: &str,
) -> Result<Vec<GraphOp>, ActorProcessingErr> {
    debug!("OCIArtifact resolved: {uri}");
    let artifact = ArtifactNodeKey::OCIArtifact {
        digest: digest.clone(),
    };

    let ops = match serde_json::from_slice(manifest_data)? {
        OciManifest::Image(manifest) => {
            let mut ops = vec![upsert(
                artifact.clone(),
                vec![
                    Property("digest".into(), string(&digest)),
                    Property("uri".into(), string(uri)),
                    Property(
                        "media_type".into(),
                        string(manifest.media_type.unwrap_or_else(|| "null".into())),
                    ),
                    Property("observed_at".into(), string(observed_at)),
                ],
            )];
            ops.extend(hosting(&artifact, registry));

            // Layer blobs, by compressed digest, in manifest order. `order`
            // pairs each blob with the image config's diff ID at the same
            // position (HAS_DIFF_ID, written from the pipeline in `outputs`).
            for (order, layer) in manifest.layers.into_iter().enumerate() {
                let layer_k = ArtifactNodeKey::OCILayer {
                    digest: layer.digest.clone(),
                };
                // TODO: layer URLs (foreign layers) are a lead for chasing the
                // rest of the supply chain; recorded but not followed yet.
                let urls = layer
                    .urls
                    .unwrap_or_default()
                    .into_iter()
                    .map(GraphValue::String)
                    .collect();
                ops.push(upsert(
                    layer_k.clone(),
                    vec![
                        Property("media_type".into(), string(layer.media_type)),
                        Property("size".into(), GraphValue::I64(layer.size)),
                        Property("urls".into(), GraphValue::List(urls)),
                    ],
                ));
                ops.push(edge(
                    artifact.clone(),
                    rel::HAS_LAYER,
                    layer_k,
                    vec![Property("order".into(), GraphValue::I64(order as i64))],
                ));
            }
            ops
        }

        // An index points at per-platform manifests. Platform belongs on the
        // edge, not the child: it describes the descriptor, and the same child
        // manifest can appear in several indexes with different descriptors.
        // Children are written as stubs; the resolver may not have fetched
        // them yet. That is what makes recursive hydration possible later.
        //
        // TODO: an is_index property for cheaper filtering, or rely on the
        // index media types (application/vnd.oci.image.index.v1+json,
        // application/vnd.docker.distribution.manifest.list.v2+json).
        OciManifest::ImageIndex(index) => {
            let mut ops = vec![upsert(
                artifact.clone(),
                vec![
                    Property("digest".into(), string(&digest)),
                    Property("uri".into(), string(uri)),
                    Property(
                        "media_type".into(),
                        string(index.media_type.unwrap_or_else(|| "null".into())),
                    ),
                    Property(
                        "schema_version".into(),
                        GraphValue::I64(index.schema_version as i64),
                    ),
                    Property(
                        "artifact_type".into(),
                        string(index.artifact_type.unwrap_or_else(|| "null".into())),
                    ),
                    Property("observed_at".into(), string(observed_at)),
                ],
            )];
            ops.extend(hosting(&artifact, registry));

            for entry in index.manifests {
                let child = ArtifactNodeKey::OCIArtifact {
                    digest: entry.digest.clone(),
                };
                ops.push(upsert(
                    child.clone(),
                    vec![
                        Property("digest".into(), string(&entry.digest)),
                        Property("media_type".into(), string(&entry.media_type)),
                        Property("size".into(), GraphValue::I64(entry.size)),
                    ],
                ));

                let mut edge_props = vec![
                    Property("descriptor_media_type".into(), string(entry.media_type)),
                    Property("descriptor_size".into(), GraphValue::I64(entry.size)),
                ];
                if let Some(platform) = entry.platform {
                    edge_props.push(Property("platform_os".into(), string(platform.os)));
                    edge_props.push(Property(
                        "platform_arch".into(),
                        string(platform.architecture),
                    ));
                    if let Some(variant) = platform.variant {
                        edge_props.push(Property("platform_variant".into(), string(variant)));
                    }
                }
                ops.push(edge(artifact.clone(), "HAS_MANIFEST", child, edge_props));
            }
            ops
        }
    };
    Ok(ops)
}

/// A registry an observer saw configured (e.g. a GitLab project's container
/// registry), before any artifact in it has been resolved.
pub fn registry_discovered(hostname: String) -> Vec<GraphOp> {
    debug!("OCI registry discovered: {hostname}");
    vec![upsert(ArtifactNodeKey::OCIRegistry { hostname }, vec![])]
}
