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
use oci_client::manifest::{OciManifest, Platform};
use polar::graph::controller::{GraphOp, GraphValue, Property, rel};
use polar::graph::nodes::builds::ArtifactNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

/// The os and architecture an image index gives for an entry, as the manifest spelled them: oci-spec
/// parses both into enums, and their `Display` gives the manifest's string back, unknown values too.
fn platform_os_and_arch(platform: &Platform) -> (String, String) {
    (platform.os.to_string(), platform.architecture.to_string())
}

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
                    let (os, arch) = platform_os_and_arch(&platform);
                    edge_props.push(Property("platform_os".into(), string(os)));
                    edge_props.push(Property("platform_arch".into(), string(arch)));
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

#[cfg(test)]
mod tests {
    use super::platform_os_and_arch;
    use oci_client::manifest::OciImageIndex;
    use proptest::prelude::*;

    /// An image index with one entry for the given platform.
    fn index_for(os: &str, architecture: &str) -> OciImageIndex {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "size": 1,
                "platform": { "architecture": architecture, "os": os }
            }]
        }))
        .unwrap()
    }

    fn recorded(index: &OciImageIndex) -> (String, String) {
        platform_os_and_arch(index.manifests[0].platform.as_ref().unwrap())
    }

    #[test]
    fn an_index_edge_records_the_platform_strings_the_manifest_gave() {
        for (os, arch) in [("linux", "amd64"), ("linux", "386"), ("windows", "arm64")] {
            assert_eq!(
                recorded(&index_for(os, arch)),
                (os.to_string(), arch.to_string())
            );
        }
        // Values oci-spec has no variant for are kept too, not dropped or renamed.
        assert_eq!(
            recorded(&index_for("plan10", "quantum")),
            ("plan10".to_string(), "quantum".to_string())
        );
    }

    proptest! {
        /// oci-spec 0.10 parses os and architecture into enums; what the linker records is still
        /// exactly the string the manifest gave, a known value or not, as with 0.8's plain strings.
        #[test]
        fn any_platform_is_recorded_as_the_manifest_gave_it(
            os in "[a-z0-9]{1,12}",
            arch in "[a-z0-9]{1,12}",
        ) {
            prop_assert_eq!(recorded(&index_for(&os, &arch)), (os, arch));
        }
    }
}
