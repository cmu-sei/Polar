//! Deciding what ConfigMaps to write.
//!
//! A ConfigMap of a plan is public: it holds a literal or a copy of one key of a Secret, such as the
//! certificate half of the cluster's CA. Unlike a Secret it is not left alone when it exists. It is
//! made to match what it copies, so that replacing the Secret (deliberately, by deleting it) cannot
//! leave a ConfigMap in another namespace vouching for a certificate that is gone.
//!
//! Pure, like [`crate::secrets`]: the executor reads the cluster and this decides.

use crate::plan::{self, Source, Value};
use anyhow::{Result, anyhow};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Already holds what the plan says.
    Unchanged { namespace: String, name: String },
    /// Make it, or bring it into line (`existed` says which).
    Write { namespace: String, name: String, existed: bool, data: BTreeMap<String, String> },
    /// A key copies a Secret key that is not there.
    Blocked { namespace: String, name: String, source: Source },
}

impl Action {
    pub fn name(&self) -> String {
        match self {
            Action::Unchanged { namespace, name }
            | Action::Write { namespace, name, .. }
            | Action::Blocked { namespace, name, .. } => format!("{namespace}/{name}"),
        }
    }
}

/// The Secret keys the ConfigMaps copy, so the executor can read them first.
pub fn copy_sources(config_maps: &[plan::ConfigMap]) -> Vec<Source> {
    let mut sources: Vec<Source> = config_maps
        .iter()
        .flat_map(|c| c.keys.values())
        .filter_map(|v| match v {
            Value::Copy(c) => Some(c.copy.clone()),
            _ => None,
        })
        .collect();
    sources.sort();
    sources.dedup();
    sources
}

/// Decide what to do about each ConfigMap. `held` is what each that exists holds, by namespace and name;
/// `copies` is what each copy source holds.
pub fn decide(
    config_maps: &[plan::ConfigMap],
    held: &BTreeMap<(String, String), BTreeMap<String, String>>,
    copies: &BTreeMap<Source, String>,
) -> Result<Vec<Action>> {
    let mut actions = Vec::new();
    'each: for config_map in config_maps {
        let mut wanted = BTreeMap::new();
        for (key, value) in &config_map.keys {
            let text = match value {
                Value::Literal(l) => l.literal.clone(),
                Value::Copy(c) => match copies.get(&c.copy) {
                    Some(text) => text.clone(),
                    None => {
                        actions.push(Action::Blocked {
                            namespace: config_map.namespace.clone(),
                            name: config_map.name.clone(),
                            source: c.copy.clone(),
                        });
                        continue 'each;
                    }
                },
                other => {
                    return Err(anyhow!(
                        "{}/{} key '{key}' is {other:?}; a ConfigMap may only hold a literal or a copy",
                        config_map.namespace,
                        config_map.name
                    ));
                }
            };
            wanted.insert(key.clone(), text);
        }
        let there = held.get(&(config_map.namespace.clone(), config_map.name.clone()));
        let current = there.is_some_and(|have| wanted.iter().all(|(k, v)| have.get(k) == Some(v)));
        actions.push(if current {
            Action::Unchanged { namespace: config_map.namespace.clone(), name: config_map.name.clone() }
        } else {
            Action::Write {
                namespace: config_map.namespace.clone(),
                name: config_map.name.clone(),
                existed: there.is_some(),
                data: wanted,
            }
        });
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plan::Plan;

    fn config_maps(yaml: &str) -> Vec<plan::ConfigMap> {
        Plan::from_yaml(&format!("name: p\nstages:\n  - name: s\n    configMaps:\n{yaml}"))
            .unwrap()
            .stages
            .remove(0)
            .config_maps
    }

    const CA: &str = "
      - { namespace: blitz, name: polar-ca, keys: { ca.pem: { copy: { namespace: polar, name: cert-issuer-ca, key: ca.crt } } } }
";

    fn source() -> Source {
        Source { namespace: "polar".into(), name: "cert-issuer-ca".into(), key: "ca.crt".into() }
    }

    fn copies(text: &str) -> BTreeMap<Source, String> {
        BTreeMap::from([(source(), text.to_string())])
    }

    fn held(text: &str) -> BTreeMap<(String, String), BTreeMap<String, String>> {
        BTreeMap::from([(
            ("blitz".to_string(), "polar-ca".to_string()),
            BTreeMap::from([("ca.pem".to_string(), text.to_string())]),
        )])
    }

    #[test]
    fn one_that_is_absent_is_written_with_what_it_copies() {
        let actions = decide(&config_maps(CA), &BTreeMap::new(), &copies("CERT")).unwrap();
        assert_eq!(
            actions,
            [Action::Write {
                namespace: "blitz".into(),
                name: "polar-ca".into(),
                existed: false,
                data: BTreeMap::from([("ca.pem".to_string(), "CERT".to_string())]),
            }]
        );
    }

    #[test]
    fn one_that_holds_the_same_is_left_alone() {
        let actions = decide(&config_maps(CA), &held("CERT"), &copies("CERT")).unwrap();
        assert_eq!(actions, [Action::Unchanged { namespace: "blitz".into(), name: "polar-ca".into() }]);
    }

    #[test]
    fn one_that_holds_a_certificate_that_is_gone_is_brought_into_line() {
        // The CA was replaced on purpose; a pod that pins the old one would trust nothing the new one signs.
        let actions = decide(&config_maps(CA), &held("OLD"), &copies("NEW")).unwrap();
        assert!(matches!(&actions[0], Action::Write { existed: true, data, .. } if data["ca.pem"] == "NEW"), "{actions:?}");
    }

    #[test]
    fn a_key_the_plan_does_not_name_does_not_make_it_differ() {
        let mut have = held("CERT");
        have.get_mut(&("blitz".to_string(), "polar-ca".to_string())).unwrap().insert("extra".into(), "x".into());
        let actions = decide(&config_maps(CA), &have, &copies("CERT")).unwrap();
        assert!(matches!(&actions[0], Action::Unchanged { .. }));
    }

    #[test]
    fn a_source_that_is_not_there_blocks_it_and_names_the_source() {
        let actions = decide(&config_maps(CA), &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(actions, [Action::Blocked { namespace: "blitz".into(), name: "polar-ca".into(), source: source() }]);
    }

    #[test]
    fn a_literal_is_used_as_written() {
        let maps = config_maps("      - { namespace: n, name: c, keys: { mode: { literal: strict } } }\n");
        let actions = decide(&maps, &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert!(matches!(&actions[0], Action::Write { data, .. } if data["mode"] == "strict"));
    }

    #[test]
    fn the_sources_are_listed_once_each() {
        let maps = config_maps(&format!("{CA}{}", CA.replace("blitz", "build-jobs")));
        assert_eq!(copy_sources(&maps), [source()]);
    }
}
