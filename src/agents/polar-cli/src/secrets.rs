//! Deciding what Secrets to make, and with what.
//!
//! Pure: the executor asks the cluster which Secrets exist and what the ones to be copied hold, and
//! this decides. A Secret that exists is left alone. Secrets in a group are made together, sharing the
//! random values they name, or not at all: a group that is partly there is an error, because making the
//! rest would put two values for one password in the cluster.
//!
//! Nothing here prints a value. [`Secret`] shows `<secret>` for itself in `Debug`, and an error names
//! secrets and keys, never what they hold.

use crate::plan::{self, Source, Value};
use anyhow::{Result, anyhow, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A value that must not be shown.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Secret {
        Secret(text)
    }

    /// The text, for putting in the cluster and nowhere else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<secret>")
    }
}

/// What to do about one Secret of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Exists { namespace: String, name: String },
    Create { namespace: String, name: String, data: BTreeMap<String, Secret> },
}

impl Action {
    pub fn name(&self) -> String {
        match self {
            Action::Exists { namespace, name } | Action::Create { namespace, name, .. } => {
                format!("{namespace}/{name}")
            }
        }
    }
}

/// The Secrets whose keys the plan copies from, so the executor can read them first.
pub fn copy_sources(secrets: &[plan::Secret]) -> Vec<Source> {
    let mut sources: Vec<Source> = secrets
        .iter()
        .flat_map(|s| s.keys.values())
        .filter_map(|v| match v {
            Value::Copy(c) => Some(c.copy.clone()),
            _ => None,
        })
        .collect();
    sources.sort_by(|a, b| (&a.namespace, &a.name, &a.key).cmp(&(&b.namespace, &b.name, &b.key)));
    sources.dedup();
    sources
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decide what to do about each Secret of a stage.
///
/// `present` is the namespace and name of each that exists; `copies` holds what each copy source
/// holds; `random` makes that many random bytes.
pub fn decide(
    secrets: &[plan::Secret],
    present: &BTreeSet<(String, String)>,
    copies: &BTreeMap<Source, String>,
    random: &mut dyn FnMut(usize) -> Vec<u8>,
) -> Result<Vec<Action>> {
    // The units that are made together, in the order they first appear: a group, or one secret.
    let mut units: Vec<Vec<&plan::Secret>> = Vec::new();
    let mut grouped: BTreeMap<&str, usize> = BTreeMap::new();
    for secret in secrets {
        match secret.group.as_deref() {
            Some(group) => match grouped.get(group) {
                Some(&at) => units[at].push(secret),
                None => {
                    grouped.insert(group, units.len());
                    units.push(vec![secret]);
                }
            },
            None => units.push(vec![secret]),
        }
    }

    let mut actions = Vec::new();
    for unit in units {
        let exist: Vec<bool> = unit
            .iter()
            .map(|s| present.contains(&(s.namespace.clone(), s.name.clone())))
            .collect();

        if exist.iter().all(|e| *e) {
            actions.extend(unit.iter().map(|s| Action::Exists {
                namespace: s.namespace.clone(),
                name: s.name.clone(),
            }));
            continue;
        }
        if exist.iter().any(|e| *e) {
            let there: Vec<String> = unit
                .iter()
                .zip(&exist)
                .filter(|(_, e)| **e)
                .map(|(s, _)| format!("{}/{}", s.namespace, s.name))
                .collect();
            let missing: Vec<String> = unit
                .iter()
                .zip(&exist)
                .filter(|(_, e)| !**e)
                .map(|(s, _)| format!("{}/{}", s.namespace, s.name))
                .collect();
            bail!(
                "the secrets of group '{}' are made together, but only {} exist; {} do not. \
                 Delete the ones that exist (their values are lost either way) and run again",
                unit[0].group.as_deref().unwrap_or("?"),
                there.join(", "),
                missing.join(", ")
            );
        }

        // None exist: make the values, naming the shared ones once.
        let mut ids: BTreeMap<String, String> = BTreeMap::new();
        for secret in &unit {
            for value in secret.keys.values() {
                if let Value::Random(r) = value
                    && let Some(id) = &r.id
                {
                    ids.insert(id.clone(), hex(&random(r.random)));
                }
            }
        }
        for secret in unit {
            let mut data = BTreeMap::new();
            for (key, value) in &secret.keys {
                let text = match value {
                    Value::Literal(l) => l.literal.clone(),
                    Value::Random(r) => match &r.id {
                        Some(id) => ids[id].clone(),
                        None => hex(&random(r.random)),
                    },
                    Value::Template(t) => fill(&t.template, &ids)
                        .map_err(|e| anyhow!("{}/{} key '{key}': {e}", secret.namespace, secret.name))?,
                    Value::Copy(c) => copies.get(&c.copy).cloned().ok_or_else(|| {
                        anyhow!(
                            "{}/{} key '{key}' copies {}/{} key '{}', which is not there",
                            secret.namespace,
                            secret.name,
                            c.copy.namespace,
                            c.copy.name,
                            c.copy.key
                        )
                    })?,
                };
                data.insert(key.clone(), Secret::new(text));
            }
            actions.push(Action::Create {
                namespace: secret.namespace.clone(),
                name: secret.name.clone(),
                data,
            });
        }
    }
    Ok(actions)
}

fn fill(template: &str, ids: &BTreeMap<String, String>) -> Result<String> {
    let mut out = template.to_string();
    for id in plan::placeholders(template).map_err(|e| anyhow!(e))? {
        let value = ids.get(&id).ok_or_else(|| anyhow!("no random value is named '{id}'"))?;
        out = out.replace(&format!("{{{id}}}"), value);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Plan;

    fn stage_secrets(yaml: &str) -> Vec<plan::Secret> {
        Plan::from_yaml(&format!("name: p\nstages:\n  - name: s\n    secrets:\n{yaml}"))
            .unwrap()
            .stages
            .remove(0)
            .secrets
    }

    /// Random bytes that count up, so a test can tell values apart without a real source.
    fn counting() -> impl FnMut(usize) -> Vec<u8> {
        let mut next = 0u8;
        move |n| {
            (0..n)
                .map(|_| {
                    next += 1;
                    next
                })
                .collect()
        }
    }

    const GRAPH: &str = "
      - { namespace: polar, name: pw, group: graph, keys: { secret: { random: 2, id: pw } } }
      - { namespace: polar, name: auth, group: graph, keys: { secret: { template: 'neo4j/{pw}' } } }
      - { namespace: polar, name: token, keys: { token: { random: 3 } } }
";

    fn data(action: &Action) -> BTreeMap<String, String> {
        match action {
            Action::Create { data, .. } => data.iter().map(|(k, v)| (k.clone(), v.expose().to_string())).collect(),
            Action::Exists { .. } => panic!("expected a create"),
        }
    }

    #[test]
    fn a_group_shares_its_random_value_and_a_plain_secret_has_its_own() {
        let actions = decide(&stage_secrets(GRAPH), &BTreeSet::new(), &BTreeMap::new(), &mut counting()).unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(data(&actions[0])["secret"], "0102");
        assert_eq!(data(&actions[1])["secret"], "neo4j/0102", "the template uses the group's value");
        assert_eq!(data(&actions[2])["token"], "030405");
    }

    #[test]
    fn a_secret_that_exists_is_left_alone() {
        let present = BTreeSet::from([("polar".to_string(), "token".to_string())]);
        let actions = decide(&stage_secrets(GRAPH), &present, &BTreeMap::new(), &mut counting()).unwrap();
        assert!(matches!(&actions[2], Action::Exists { name, .. } if name == "token"));
        assert!(matches!(&actions[0], Action::Create { .. }));
    }

    #[test]
    fn a_whole_group_that_exists_is_left_alone() {
        let present = BTreeSet::from([
            ("polar".to_string(), "pw".to_string()),
            ("polar".to_string(), "auth".to_string()),
        ]);
        let actions = decide(&stage_secrets(GRAPH), &present, &BTreeMap::new(), &mut counting()).unwrap();
        assert!(matches!(&actions[0], Action::Exists { .. }));
        assert!(matches!(&actions[1], Action::Exists { .. }));
    }

    #[test]
    fn a_group_that_is_partly_there_is_an_error_that_names_both_halves() {
        let present = BTreeSet::from([("polar".to_string(), "pw".to_string())]);
        let error = decide(&stage_secrets(GRAPH), &present, &BTreeMap::new(), &mut counting())
            .unwrap_err()
            .to_string();
        assert!(error.contains("group 'graph'"), "{error}");
        assert!(error.contains("only polar/pw exist") && error.contains("polar/auth do not"), "{error}");
    }

    #[test]
    fn a_copy_takes_the_value_of_a_secret_that_exists() {
        let secrets = stage_secrets("      - { namespace: blitz, name: db, keys: { password: { copy: { namespace: polar, name: pw, key: secret } } } }\n");
        let source = copy_sources(&secrets);
        assert_eq!(source.len(), 1);
        let copies = BTreeMap::from([(source[0].clone(), "swordfish-not-real".to_string())]);
        let actions = decide(&secrets, &BTreeSet::new(), &copies, &mut counting()).unwrap();
        assert_eq!(data(&actions[0])["password"], "swordfish-not-real");

        let error = decide(&secrets, &BTreeSet::new(), &BTreeMap::new(), &mut counting()).unwrap_err().to_string();
        assert!(error.contains("polar/pw key 'secret', which is not there"), "{error}");
    }

    #[test]
    fn a_literal_is_used_as_written() {
        let secrets = stage_secrets("      - { namespace: n, name: admin, keys: { username: { literal: admin } } }\n");
        let actions = decide(&secrets, &BTreeSet::new(), &BTreeMap::new(), &mut counting()).unwrap();
        assert_eq!(data(&actions[0])["username"], "admin");
    }

    #[test]
    fn nothing_that_is_made_can_be_read_back_from_a_debug_print_or_an_error() {
        let secrets = stage_secrets("      - { namespace: n, name: a, keys: { k: { random: 4 } } }\n      - { namespace: n, name: b, keys: { k: { copy: { namespace: x, name: y, key: z } } } }\n");
        let copies = BTreeMap::from([(copy_sources(&secrets)[0].clone(), "hunter2-not-real".to_string())]);
        let actions = decide(&secrets, &BTreeSet::new(), &copies, &mut counting()).unwrap();
        let shown = format!("{actions:?}");
        assert!(!shown.contains("hunter2") && !shown.contains("01020304"), "{shown}");
        assert!(shown.contains("<secret>"));
    }

    #[test]
    fn hex_is_lower_case_and_two_digits_a_byte() {
        assert_eq!(hex(&[0, 15, 16, 255]), "000f10ff");
    }
}
