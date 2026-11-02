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

//! What the graph keeps so a MERGE seeks its node instead of scanning its label (docs/architecture/graph-write-path.md,
//! "Constraints and indexes"): for each label an agent owns, on the properties that label's node key merges on,
//! derived from the agent's node key vocabularies ([`crate::graph::controller::shapes_of`]) and made by its graph
//! controller at start.

use crate::graph::controller::NodeShape;
use neo4rs::{Graph, Query};
use std::collections::{BTreeMap, BTreeSet};
use tracing::{error, info, warn};

/// What the graph keeps for a label's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyIndexKind {
    /// A property uniqueness constraint (and the range index behind it): the label has one key, so no two of its
    /// nodes may share it, and concurrent MERGEs of one new node make one node.
    Unique,
    /// A range index: the label is merged on more than one key (FluxOCIRepository on `uid`, and on `name` and
    /// `namespace` from a reference), so one node may be written under each and a uniqueness on either could refuse
    /// a write.
    Index,
}

/// A constraint or index on a label's key properties.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyIndex {
    pub label: String,
    pub keys: Vec<String>,
    pub kind: KeyIndexKind,
}

impl KeyIndex {
    /// Its name in the graph, a function of the rest: `polar_unique_JiraIssue_key`, `polar_index_PodState_pod_uid_valid_from`.
    pub fn name(&self) -> String {
        let kind = match self.kind {
            KeyIndexKind::Unique => "unique",
            KeyIndexKind::Index => "index",
        };
        format!("polar_{kind}_{}_{}", self.label, self.keys.join("_"))
    }

    /// The statement that makes it, nothing when it is there already (by name).
    pub fn create(&self) -> String {
        let props = self
            .keys
            .iter()
            .map(|k| format!("n.{k}"))
            .collect::<Vec<_>>()
            .join(", ");
        let name = self.name();
        let label = &self.label;
        match self.kind {
            KeyIndexKind::Unique if self.keys.len() == 1 => {
                format!(
                    "CREATE CONSTRAINT {name} IF NOT EXISTS FOR (n:{label}) REQUIRE {props} IS UNIQUE"
                )
            }
            KeyIndexKind::Unique => {
                format!(
                    "CREATE CONSTRAINT {name} IF NOT EXISTS FOR (n:{label}) REQUIRE ({props}) IS UNIQUE"
                )
            }
            KeyIndexKind::Index => {
                format!("CREATE INDEX {name} IF NOT EXISTS FOR (n:{label}) ON ({props})")
            }
        }
    }

    /// How many groups of nodes share a key, and how many nodes they hold: what keeps a uniqueness constraint from
    /// being made.
    fn duplicates(&self) -> String {
        let present = self
            .keys
            .iter()
            .map(|k| format!("n.{k} IS NOT NULL"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let values = self
            .keys
            .iter()
            .enumerate()
            .map(|(i, k)| format!("n.{k} AS k{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "MATCH (n:{}) WHERE {present} WITH {values}, count(*) AS c WHERE c > 1 \
             RETURN count(*) AS groups, coalesce(sum(c), 0) AS nodes",
            self.label
        )
    }
}

/// The constraints and indexes for the labels in `owned`, from the shapes of the nodes the agent writes: for each
/// owned label, each distinct set of key properties a shape merges it on (a shape with no key, the `State` anchor,
/// needs none); [`KeyIndexKind::Unique`] when the label has one such set, [`KeyIndexKind::Index`] for each when it has
/// several. A label owned but in no shape (written by raw Cypher, or named at run time) gets nothing. Ordered and each
/// once, whatever the order or repetition of the declarations.
pub fn key_indexes(owned: &[&str], shapes: &[NodeShape]) -> Vec<KeyIndex> {
    let owned: BTreeSet<&str> = owned.iter().copied().collect();
    let mut by_label: BTreeMap<&str, BTreeSet<&Vec<String>>> = BTreeMap::new();
    for shape in shapes {
        if shape.keys.is_empty() {
            continue;
        }
        for label in &shape.labels {
            if let Some(label) = owned.get(label.as_str()) {
                by_label.entry(label).or_default().insert(&shape.keys);
            }
        }
    }
    by_label
        .into_iter()
        .flat_map(|(label, key_sets)| {
            let kind = if key_sets.len() == 1 {
                KeyIndexKind::Unique
            } else {
                KeyIndexKind::Index
            };
            key_sets.into_iter().map(move |keys| KeyIndex {
                label: label.to_string(),
                keys: keys.clone(),
                kind,
            })
        })
        .collect()
}

/// What became of one wanted constraint or index at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ensured {
    /// It was there already: a second start is a no-op.
    Present(KeyIndex),
    /// Made now.
    Created(KeyIndex),
    /// The graph refused to make it, most often a uniqueness constraint over nodes that already share a key (counted
    /// in `duplicates`, groups and nodes; reported, never deleted). For a uniqueness constraint, the range index on
    /// the same keys is made in its place, so MERGE still seeks.
    Refused {
        wanted: KeyIndex,
        reason: String,
        duplicates: Option<(i64, i64)>,
        instead: Option<Box<Ensured>>,
    },
}

async fn names(graph: &Graph, show: &str) -> Result<BTreeSet<String>, String> {
    let mut rows = graph
        .execute(Query::new(format!("{show} YIELD name RETURN name")))
        .await
        .map_err(|e| format!("{show}: {e}"))?;
    let mut names = BTreeSet::new();
    while let Some(row) = rows.next().await.map_err(|e| format!("{show}: {e}"))? {
        names.insert(row.get::<String>("name").map_err(|e| e.to_string())?);
    }
    Ok(names)
}

async fn duplicates(graph: &Graph, index: &KeyIndex) -> Option<(i64, i64)> {
    let mut rows = graph.execute(Query::new(index.duplicates())).await.ok()?;
    let row = rows.next().await.ok()??;
    Some((
        row.get::<i64>("groups").ok()?,
        row.get::<i64>("nodes").ok()?,
    ))
}

async fn ensure_one(graph: &Graph, present: &BTreeSet<String>, index: &KeyIndex) -> Ensured {
    if present.contains(&index.name()) {
        return Ensured::Present(index.clone());
    }
    match graph.run(Query::new(index.create())).await {
        Ok(()) => Ensured::Created(index.clone()),
        Err(e) => {
            let (duplicates, instead) = match index.kind {
                KeyIndexKind::Unique => {
                    let range = KeyIndex {
                        kind: KeyIndexKind::Index,
                        ..index.clone()
                    };
                    let instead = Box::pin(ensure_one(graph, present, &range)).await;
                    (duplicates(graph, index).await, Some(Box::new(instead)))
                }
                KeyIndexKind::Index => (None, None),
            };
            Ensured::Refused {
                wanted: index.clone(),
                reason: e.to_string(),
                duplicates,
                instead,
            }
        }
    }
}

/// Makes each of `wanted` the graph does not have yet, by name, and logs what became of each: made (info), refused
/// (error, with the nodes that share a key), or already there (nothing, so a second start is quiet). `Err` when the
/// graph could not be asked what it has: nothing was made, and the caller asks again later.
pub async fn ensure_key_indexes(
    graph: &Graph,
    wanted: &[KeyIndex],
) -> Result<Vec<Ensured>, String> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let mut present = names(graph, "SHOW CONSTRAINTS").await?;
    present.extend(names(graph, "SHOW INDEXES").await?);
    let mut outcomes = Vec::new();
    for index in wanted {
        let outcome = ensure_one(graph, &present, index).await;
        log(&outcome);
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

fn log(outcome: &Ensured) {
    match outcome {
        Ensured::Present(_) => {}
        Ensured::Created(index) => info!(
            "the graph keeps {} on :{}({}) now ({})",
            match index.kind {
                KeyIndexKind::Unique => "a uniqueness constraint",
                KeyIndexKind::Index => "a range index",
            },
            index.label,
            index.keys.join(", "),
            index.name()
        ),
        Ensured::Refused {
            wanted,
            reason,
            duplicates,
            instead,
        } => {
            match duplicates {
                Some((groups, nodes)) if *groups > 0 => error!(
                    "the graph refused {} on :{}({}): {groups} keys are each held by more than one node ({nodes} \
                     nodes in all), left as they are; {reason}",
                    wanted.name(),
                    wanted.label,
                    wanted.keys.join(", ")
                ),
                _ => error!(
                    "the graph refused {} on :{}({}): {reason}",
                    wanted.name(),
                    wanted.label,
                    wanted.keys.join(", ")
                ),
            }
            if let Some(instead) = instead {
                match instead.as_ref() {
                    Ensured::Created(range) | Ensured::Present(range) => warn!(
                        "{} keeps MERGE on :{} seeking instead, without uniqueness",
                        range.name(),
                        range.label
                    ),
                    other => log(other),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn shape(labels: &[&str], keys: &[&str]) -> NodeShape {
        NodeShape {
            labels: labels.iter().map(|s| s.to_string()).collect(),
            keys: keys.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn an_owned_label_with_one_key_is_unique_and_one_with_two_is_indexed_on_each() {
        let shapes = [
            shape(&["JiraIssue"], &["key"]),
            shape(&["FluxOCIRepository"], &["uid"]),
            shape(&["FluxOCIRepository"], &["name", "namespace"]),
            shape(&["PodState"], &["pod_uid", "valid_from"]),
            shape(&["State"], &[]),
            shape(&["Schedule", "Configuration"], &["id"]),
            shape(&["NotOwned"], &["id"]),
        ];
        let got = key_indexes(
            &[
                "JiraIssue",
                "FluxOCIRepository",
                "PodState",
                "State",
                "Configuration",
            ],
            &shapes,
        );
        let names: Vec<String> = got.iter().map(KeyIndex::name).collect();
        assert_eq!(
            names,
            [
                "polar_unique_Configuration_id",
                "polar_index_FluxOCIRepository_name_namespace",
                "polar_index_FluxOCIRepository_uid",
                "polar_unique_JiraIssue_key",
                "polar_unique_PodState_pod_uid_valid_from",
            ]
        );
        assert_eq!(
            got[3].create(),
            "CREATE CONSTRAINT polar_unique_JiraIssue_key IF NOT EXISTS FOR (n:JiraIssue) REQUIRE n.key IS UNIQUE"
        );
        assert_eq!(
            got[4].create(),
            "CREATE CONSTRAINT polar_unique_PodState_pod_uid_valid_from IF NOT EXISTS FOR (n:PodState) \
             REQUIRE (n.pod_uid, n.valid_from) IS UNIQUE"
        );
        assert_eq!(
            got[1].create(),
            "CREATE INDEX polar_index_FluxOCIRepository_name_namespace IF NOT EXISTS FOR (n:FluxOCIRepository) \
             ON (n.name, n.namespace)"
        );
    }

    #[test]
    fn a_reader_owns_nothing_and_makes_nothing() {
        assert!(key_indexes(&[], &[shape(&["JiraIssue"], &["key"])]).is_empty());
    }

    fn any_shapes() -> impl Strategy<Value = Vec<NodeShape>> {
        prop::collection::vec(
            (
                prop::collection::vec("[A-D]", 1..3),
                prop::collection::vec("[a-c]", 0..3),
            )
                .prop_map(|(labels, keys)| NodeShape { labels, keys }),
            0..10,
        )
    }

    fn shuffled<T: Clone>(items: &[T], seed: u64) -> Vec<T> {
        let mut out = items.to_vec();
        let mut s = seed | 1;
        for i in (1..out.len()).rev() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            out.swap(i, (s % (i as u64 + 1)) as usize);
        }
        out
    }

    proptest! {
        /// The derivation is a function of what is declared: the same however the shapes and the owned labels are
        /// ordered or repeated; each owned label merged on a key has an entry for exactly each key it is merged on,
        /// unique exactly when that is one key; and nothing for a label not owned.
        #[test]
        fn the_derivation_is_deterministic_order_independent_and_exact(
            shapes in any_shapes(),
            owned in prop::collection::vec("[A-D]", 0..4),
            seed in any::<u64>(),
        ) {
            let owned_refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            let got = key_indexes(&owned_refs, &shapes);
            let mut again_owned = shuffled(&owned_refs, seed);
            again_owned.extend(owned_refs.iter().take(1));
            let mut again_shapes = shuffled(&shapes, seed.rotate_left(7));
            again_shapes.extend(shapes.iter().take(1).cloned());
            prop_assert_eq!(&key_indexes(&again_owned, &again_shapes), &got);

            let mut sorted = got.clone();
            sorted.sort_by(|a, b| (&a.label, &a.keys).cmp(&(&b.label, &b.keys)));
            sorted.dedup();
            prop_assert_eq!(&sorted, &got, "ordered, each once");

            for label in &owned {
                let key_sets: BTreeSet<&Vec<String>> = shapes
                    .iter()
                    .filter(|s| !s.keys.is_empty() && s.labels.contains(label))
                    .map(|s| &s.keys)
                    .collect();
                let found: BTreeSet<&Vec<String>> =
                    got.iter().filter(|i| &i.label == label).map(|i| &i.keys).collect();
                prop_assert_eq!(&found, &key_sets);
                for index in got.iter().filter(|i| &i.label == label) {
                    prop_assert_eq!(index.kind == KeyIndexKind::Unique, key_sets.len() == 1);
                }
            }
            prop_assert!(got.iter().all(|i| owned.contains(&i.label)));
        }
    }
}
