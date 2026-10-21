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

//! How groups and roles are arranged, beside who is in them (docs/architecture/policy-enforcement.md, "Decisions"):
//! the part of the signed groups document that Cedar's hierarchy is built from. A group may be in other groups and
//! hold roles; a role may hold other roles, capabilities (`update-policy`) and communication permissions (`s2s`). A
//! rule's `principal in Group::"x"` or `principal in Role::"y"` follows the whole chain. Certificates carry only
//! direct membership; this is the ceiling's, never the floor's.

use crate::capability::Capability;
use crate::permission::Permission;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The most entries of each kind a document may define, and the longest name, as the groups document bounds
/// its memberships.
pub const MAX_DEFINED: usize = 10_000;
pub const MAX_NAME: usize = 128;

/// A group's parents: the groups it is in and the roles it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupDef {
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// A role's parents: the roles it holds, and the capabilities and permissions it gives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleDef {
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// The groups and roles a groups document defines. A name used but not defined is a group or role with no parents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hierarchy {
    #[serde(default)]
    pub groups: BTreeMap<String, GroupDef>,
    #[serde(default)]
    pub roles: BTreeMap<String, RoleDef>,
}

fn name_problem(what: &str, name: &str) -> Option<String> {
    if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
        Some(format!(
            "{what} `{}` must be 1 to {MAX_NAME} characters with no control characters",
            name.escape_debug()
        ))
    } else {
        None
    }
}

/// The first name that leads back to itself through `parents`, if any.
fn cycle_in(parents: &BTreeMap<&str, Vec<&str>>) -> Option<String> {
    // Depth first, colouring: a name met again while still on the path closes a loop.
    fn visit<'a>(
        name: &'a str,
        parents: &BTreeMap<&'a str, Vec<&'a str>>,
        on_path: &mut BTreeSet<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<String> {
        if done.contains(name) {
            return None;
        }
        if !on_path.insert(name) {
            return Some(name.to_string());
        }
        for parent in parents.get(name).into_iter().flatten() {
            if let Some(found) = visit(parent, parents, on_path, done) {
                return Some(found);
            }
        }
        on_path.remove(name);
        done.insert(name);
        None
    }
    let mut done = BTreeSet::new();
    for name in parents.keys() {
        if let Some(found) = visit(name, parents, &mut BTreeSet::new(), &mut done) {
            return Some(found);
        }
    }
    None
}

impl Hierarchy {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.roles.is_empty()
    }

    /// Everything wrong with it: a name that is not one, a capability or permission Polar does not have, too many
    /// definitions, or a group or role that is, through its parents, its own ancestor.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.groups.len() > MAX_DEFINED || self.roles.len() > MAX_DEFINED {
            problems.push(format!(
                "at most {MAX_DEFINED} groups and {MAX_DEFINED} roles are defined"
            ));
        }
        for (name, def) in &self.groups {
            problems.extend(name_problem("the group", name));
            for parent in &def.groups {
                problems.extend(name_problem(&format!("a group `{name}` is in,"), parent));
            }
            for role in &def.roles {
                problems.extend(name_problem(&format!("a role `{name}` holds,"), role));
            }
        }
        for (name, def) in &self.roles {
            problems.extend(name_problem("the role", name));
            for parent in &def.roles {
                problems.extend(name_problem(&format!("a role `{name}` holds,"), parent));
            }
            for capability in &def.capabilities {
                if Capability::from_san_string(&format!("c:{capability}")).is_err() {
                    problems.push(format!(
                        "the role `{name}` gives `{capability}`, which is not a capability"
                    ));
                }
            }
            for permission in &def.permissions {
                if Permission::from_san_string(&format!("p:{permission}")).is_err() {
                    problems.push(format!(
                        "the role `{name}` gives `{permission}`, which is not a permission"
                    ));
                }
            }
        }
        let group_parents: BTreeMap<&str, Vec<&str>> = self
            .groups
            .iter()
            .map(|(name, def)| {
                (
                    name.as_str(),
                    def.groups.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        if let Some(name) = cycle_in(&group_parents) {
            problems.push(format!(
                "the group `{name}` is, through the groups it is in, in itself"
            ));
        }
        let role_parents: BTreeMap<&str, Vec<&str>> = self
            .roles
            .iter()
            .map(|(name, def)| {
                (
                    name.as_str(),
                    def.roles.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        if let Some(name) = cycle_in(&role_parents) {
            problems.push(format!(
                "the role `{name}` holds, through the roles it holds, itself"
            ));
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn group(groups: &[&str], roles: &[&str]) -> GroupDef {
        GroupDef {
            groups: groups.iter().map(|s| s.to_string()).collect(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_hierarchy_of_nested_groups_and_roles_is_sound() {
        let h = Hierarchy {
            groups: [
                (
                    "operators".to_string(),
                    group(&["staff"], &["policy-author"]),
                ),
                ("staff".to_string(), group(&[], &[])),
            ]
            .into(),
            roles: [(
                "policy-author".to_string(),
                RoleDef {
                    roles: vec!["reader".into()],
                    capabilities: vec!["update-policy".into()],
                    permissions: vec!["s2s".into()],
                },
            )]
            .into(),
        };
        assert_eq!(h.problems(), Vec::<String>::new());
        // As the document carries it.
        let json = serde_json::to_value(&h).unwrap();
        assert_eq!(serde_json::from_value::<Hierarchy>(json).unwrap(), h);
    }

    #[test]
    fn what_is_wrong_with_a_hierarchy_is_said_every_time() {
        let h = Hierarchy {
            groups: [
                ("a".to_string(), group(&["b"], &[])),
                ("b".to_string(), group(&["a"], &[])),
                ("".to_string(), group(&[], &[])),
            ]
            .into(),
            roles: [(
                "r".to_string(),
                RoleDef {
                    roles: vec!["r".into()],
                    capabilities: vec!["fly".into()],
                    permissions: vec!["teleport".into()],
                },
            )]
            .into(),
        };
        let said = h.problems().join("\n");
        assert!(said.contains("in itself"), "{said}");
        assert!(
            said.contains("holds, through the roles it holds, itself"),
            "{said}"
        );
        assert!(said.contains("`fly`, which is not a capability"), "{said}");
        assert!(
            said.contains("`teleport`, which is not a permission"),
            "{said}"
        );
        assert!(said.contains("the group ``"), "{said}");
        assert!(
            serde_json::from_value::<Hierarchy>(serde_json::json!({"groups": {}, "extra": 1}))
                .is_err()
        );
    }

    proptest! {
        /// A chain of groups, each in the next, is sound however long; closing it into a loop anywhere is not.
        #[test]
        fn a_chain_is_sound_and_a_loop_is_not(length in 1usize..12, back_to in 0usize..12) {
            let names: Vec<String> = (0..length).map(|i| format!("g{i}")).collect();
            let chain = |close: bool| Hierarchy {
                groups: names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let next = if i + 1 < length {
                            vec![names[i + 1].clone()]
                        } else if close {
                            vec![names[back_to % length].clone()]
                        } else {
                            vec![]
                        };
                        (name.clone(), GroupDef { groups: next, roles: vec![] })
                    })
                    .collect(),
                roles: BTreeMap::new(),
            };
            prop_assert!(chain(false).problems().is_empty());
            prop_assert!(!chain(true).problems().is_empty());
        }
    }
}
