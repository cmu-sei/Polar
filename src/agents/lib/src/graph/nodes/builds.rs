use crate::graph::controller::GraphNodeKey;
use neo4rs::BoltType;

// ── Node key vocabulary ────────────────────────────────────────────────────────

/// Node key enum for the Cyclops provenance processor.
///
/// Only two node types are *owned* by this processor — `BuildJob` and
/// `BuildJobState`. All other node types (`GitCommit`, `Image`,
/// `PodContainer`) are owned by other agents. This processor only creates
/// edges to them — never upserts their properties — to avoid clobbering
/// data that the authoritative agent manages.
///
/// The `cypher_match` implementation generates parameterized MERGE clauses
/// for each node type, following the same prefix convention as `KubeNodeKey`
/// so that `compile_graph_op` can compose multi-node queries without
/// parameter name collisions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuildNodeKey {
    /// Cannonical state node
    State,
    /// A single Cyclops build execution. This is the linchpin node — it
    /// connects a git commit to the image that was built from it.
    /// Identified by the UUID assigned at build request time.
    BuildJob { build_id: String },

    /// An immutable state snapshot of a BuildJob at a point in time.
    /// Follows the same temporal state pattern as KubeNodeKey::PodState —
    /// one state node per transition, linked via TRANSITIONED_TO edges.
    BuildJobState {
        build_id: String,
        valid_from: String,
    },

    /// A job on a backend execution system.
    /// Self-describing — the label and merge properties come from
    /// JobGraphIdentity, so this variant works for any backend
    /// without the processor knowing which one it is.
    BackendJob {
        node_label: String,
        identity_props: Vec<(String, String)>,
    },
}

impl GraphNodeKey for BuildNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            BuildNodeKey::State => ("(:State)".to_string(), vec![]),
            BuildNodeKey::BackendJob {
                node_label,
                identity_props,
            } => {
                let mut params = vec![];
                let prop_str = identity_props
                    .iter()
                    .map(|(k, v)| {
                        let param_key = format!("{prefix}_{k}");
                        params.push((param_key.clone(), BoltType::String(v.clone().into())));
                        format!("{k}: ${param_key}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");

                (format!("({prefix}:{node_label} {{ {prop_str} }})"), params)
            }
            BuildNodeKey::BuildJob { build_id } => {
                let id_k = format!("{prefix}_build_id");
                (
                    format!("({prefix}:BuildJob {{ build_id: ${id_k} }})"),
                    vec![(id_k, BoltType::String(build_id.clone().into()))],
                )
            }

            BuildNodeKey::BuildJobState {
                build_id,
                valid_from,
            } => {
                let id_k = format!("{prefix}_build_id");
                let vf_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:BuildJobState {{ build_id: ${id_k}, valid_from: ${vf_k} }})"
                    ),
                    vec![
                        (id_k, BoltType::String(build_id.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                    ],
                )
            }
        }
    }
}
