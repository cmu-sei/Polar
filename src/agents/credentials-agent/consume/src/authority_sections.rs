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

//! The two runtime sections the credentials agent declares (docs/architecture/credentials-policy.md,
//! authority-distribution.md): the Cedar rules, and who is in which group. Each is signed with a capability of
//! its own, and policy never governs them (a rule that governed its own authorship could lock out every
//! author). Each is checked when it arrives, before it is signed again by this agent and sent to every other.

use cert_issuer_common::capability::Capability;
use polar_agent_builder::RuntimeSection;
use polar_agent_builder::runtime::SectionAuthority;
use serde_json::Value;

/// Most identities named in a membership document.
pub const MAX_IDENTITIES: usize = 10_000;
/// Most groups one identity is in.
pub const MAX_GROUPS_PER_IDENTITY: usize = 64;
/// Longest identity or group name.
pub const MAX_NAME: usize = 128;

pub static POLICY: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Capability(Capability::UpdatePolicy),
    name: "policy",
    doc: "The Cedar rules every agent decides by. Replaced whole by each version. Signed by a holder of c:update-policy; policy never governs who may change it.",
    body: r#"{"source": {"type": "Cedar policy text", "required": true, "doc": "the rules, in Cedar syntax; an empty text is a policy that says nothing"}}"#,
    validate: validate_policy,
};

pub static GROUPS: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Capability(Capability::UpdateGroups),
    name: "groups",
    doc: "Who is in which group, for the rules that say `principal in Group::\"...\"`. Replaced whole by each version, so a removal is an absence. Signed by a holder of c:update-groups.",
    body: r#"{"memberships": {"type": "map from an identity (a username, or an agent's name) to the list of groups it is in", "required": true}}"#,
    validate: validate_groups,
};

fn only_fields(body: &Value, allowed: &[&str]) -> Result<(), Vec<String>> {
    let Some(object) = body.as_object() else {
        return Err(vec!["the body must be an object".to_string()]);
    };
    let strangers: Vec<String> = object
        .keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .map(|k| format!("`{k}` is not a field of this section"))
        .collect();
    if strangers.is_empty() {
        Ok(())
    } else {
        Err(strangers)
    }
}

fn validate_policy(body: &Value) -> Result<(), Vec<String>> {
    only_fields(body, &["source"])?;
    let Some(source) = body.get("source").and_then(Value::as_str) else {
        return Err(vec!["`source` must be the policy text".to_string()]);
    };
    polar_policy::validate_source(source).map_err(|why| vec![format!("`source`: {why}")])
}

fn name_problem(what: &str, name: &str) -> Option<String> {
    if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
        Some(format!(
            "{what} `{}` must be 1 to {MAX_NAME} characters, none a control character",
            name.chars().take(32).collect::<String>().escape_debug()
        ))
    } else {
        None
    }
}

fn validate_groups(body: &Value) -> Result<(), Vec<String>> {
    only_fields(body, &["memberships"])?;
    let Some(memberships) = body.get("memberships").and_then(Value::as_object) else {
        return Err(vec![
            "`memberships` must map each identity to its groups".to_string(),
        ]);
    };
    let mut problems = Vec::new();
    if memberships.len() > MAX_IDENTITIES {
        problems.push(format!(
            "`memberships` names {} identities, and at most {MAX_IDENTITIES} are taken",
            memberships.len()
        ));
    }
    for (identity, groups) in memberships {
        problems.extend(name_problem("the identity", identity));
        match groups.as_array() {
            None => problems.push(format!("the groups of `{identity}` must be a list")),
            Some(groups) => {
                if groups.len() > MAX_GROUPS_PER_IDENTITY {
                    problems.push(format!(
                        "`{identity}` is in {} groups, and at most {MAX_GROUPS_PER_IDENTITY} are taken",
                        groups.len()
                    ));
                }
                for group in groups {
                    match group.as_str() {
                        None => problems.push(format!("a group of `{identity}` is not text")),
                        Some(group) => problems.extend(name_problem("the group", group)),
                    }
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn problems(section: &RuntimeSection, body: Value) -> Vec<String> {
        (section.validate)(&body).unwrap_err()
    }

    #[test]
    fn each_section_is_for_its_own_capability_and_policy_never_governs_either() {
        assert_eq!(
            POLICY.authority,
            SectionAuthority::Capability(Capability::UpdatePolicy)
        );
        assert_eq!(
            GROUPS.authority,
            SectionAuthority::Capability(Capability::UpdateGroups)
        );
        assert_ne!(POLICY.authority, GROUPS.authority);
    }

    #[test]
    fn both_declarations_are_sound() {
        assert!(polar_agent_builder::runtime::check_declarations(&[&POLICY, &GROUPS]).is_empty());
    }

    #[test]
    fn a_policy_that_is_cedar_is_accepted_and_one_that_is_not_says_why() {
        assert!(
            (POLICY.validate)(&json!({"source": r#"permit(principal, action, resource);"#}))
                .is_ok()
        );
        assert!((POLICY.validate)(&json!({"source": ""})).is_ok());
        let bad = problems(&POLICY, json!({"source": "this is not Cedar"}));
        assert!(bad[0].starts_with("`source`:"), "{bad:?}");
    }

    #[test]
    fn a_policy_that_does_not_speak_polar_s_schema_is_refused_on_arrival_with_the_reason() {
        // A label read as an attribute, not a tag: it parses, and is still refused whole.
        let wrong = json!({ "source": r#"permit (principal, action == Action::"retrieve", resource) when { resource.kind == "token" };"# });
        let problems = validate_policy(&wrong).unwrap_err().join("; ");
        assert!(
            problems.contains("schema") && problems.contains("kind"),
            "{problems}"
        );
        // The first policy a cluster is seeded with speaks it.
        let seed =
            json!({ "source": include_str!("../../../../../infra/seeds/signed/policy.cedar") });
        assert!(validate_policy(&seed).is_ok());
    }

    #[test]
    fn a_policy_body_has_one_field_and_it_is_text() {
        for body in [
            json!({}),
            json!({"source": 5}),
            json!({"source": "", "extra": 1}),
            json!([1]),
            json!("text"),
        ] {
            assert!((POLICY.validate)(&body).is_err(), "{body}");
        }
        let strange = problems(&POLICY, json!({"source": "", "extra": 1}));
        assert!(strange[0].contains("extra"), "{strange:?}");
    }

    #[test]
    fn a_membership_maps_identities_to_lists_of_group_names() {
        assert!(
            (GROUPS.validate)(&json!({"memberships": {
                "alice": ["operators", "auditors"], "git-repo-controller": []
            }}))
            .is_ok()
        );
        assert!((GROUPS.validate)(&json!({"memberships": {}})).is_ok());
    }

    #[test]
    fn every_problem_with_a_membership_is_reported_naming_who() {
        let bad = problems(
            &GROUPS,
            json!({"memberships": {
                "alice": "operators",
                "bob": [1, ""],
                "": ["x"],
                "carol": ["ok"]
            }}),
        );
        assert!(
            bad.iter()
                .any(|p| p.contains("alice") && p.contains("list")),
            "{bad:?}"
        );
        assert!(
            bad.iter()
                .any(|p| p.contains("bob") && p.contains("not text")),
            "{bad:?}"
        );
        assert!(bad.iter().any(|p| p.contains("the group")), "{bad:?}");
        assert!(bad.iter().any(|p| p.contains("the identity")), "{bad:?}");
        assert!(!bad.iter().any(|p| p.contains("carol")), "{bad:?}");
    }

    #[test]
    fn names_with_control_characters_or_too_long_are_refused_and_so_are_stray_fields() {
        let long = "x".repeat(MAX_NAME + 1);
        for body in [
            json!({"memberships": {"new\nline": ["g"]}}),
            json!({"memberships": {"alice": ["g\u{0}"]}}),
            json!({"memberships": {"alice": [long]}}),
            json!({"memberships": {}, "extra": 1}),
            json!({"nope": 1}),
            json!({"memberships": []}),
        ] {
            assert!((GROUPS.validate)(&body).is_err(), "{body}");
        }
    }

    #[test]
    fn the_size_of_a_membership_is_bounded() {
        let many: serde_json::Map<String, Value> = (0..=MAX_IDENTITIES)
            .map(|i| (format!("user-{i}"), json!(["g"])))
            .collect();
        let too_many = problems(&GROUPS, json!({"memberships": many}));
        assert!(too_many[0].contains("at most"), "{too_many:?}");
        let groups: Vec<String> = (0..=MAX_GROUPS_PER_IDENTITY)
            .map(|i| format!("g{i}"))
            .collect();
        let too_many = problems(&GROUPS, json!({"memberships": {"alice": groups}}));
        assert!(too_many[0].contains("at most"), "{too_many:?}");
    }
}
