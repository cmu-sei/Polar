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

//! The vocabulary Polar's policies speak, as a Cedar schema carried in the code that evaluates
//! (docs/architecture/policy-enforcement.md, "Decisions"): the entity types, their attributes and membership, and the
//! actions in use with what each applies to. A policy is validated against it, strictly, when it arrives and before it
//! is signed, and every request and its entities are checked against it when evaluated, so a misspelt attribute, an
//! unknown type, or a rule on an attribute that may be absent without `has` is refused before it can decide anything.
//!
//! - A principal is always an `Identity`, by its certificate's first DNS name: a workload's identity, or a person's
//!   account ID (an author writes the username, and the credentials agent converts it before the policy is in force). Its
//!   parents are its groups (from its certificate's `g:` names and the signed groups document), the roles they bring,
//!   and its capabilities (`c:` names) as `Capability` entities, so a rule says `principal in Group::"operators"`,
//!   `principal in Role::"policy-author"` or `principal in Capability::"update-policy"`. Its `p:` communication
//!   permissions are parents too (`Permission::"s2s"`, or `Permission::"s2s:<target>"` with a target): a rule cannot
//!   test a set of records with an optional field strictly. Groups nest, and a role is a group of capabilities,
//!   permissions and roles.
//! - A secret's labels are its tags (`resource.hasTag("kind") && resource.getTag("kind") == "token"`), free-form; its
//!   owner, namespace and `frozen` are attributes, each optional.
//! - A topic request carries in its context whether the topic is in the requester's declared set (family topics
//!   included) or its implied one.

use cedar_policy::{PolicySet, Schema, ValidationMode, Validator};
use std::sync::OnceLock;

/// The schema, in Cedar's schema language.
pub const SCHEMA: &str = r#"
entity Group in [Group, Role];
entity Capability;
entity Permission;
entity Role in [Role, Capability, Permission];
entity Identity in [Group, Role, Capability, Permission] = {
  "workload"?: String,
};
entity Secret = {
  "owner"?: Identity,
  "namespace"?: Identity,
  "frozen"?: Bool,
} tags String;
entity KubeSecret, Topic, Variable, Section, Agent, Command;

action "retrieve", "store" appliesTo {
  principal: Identity,
  resource: Secret,
  context: { "now_secs": Long },
};
action "decrypt" appliesTo {
  principal: Identity,
  resource: KubeSecret,
  context: { "now_secs": Long },
};
action "publish", "subscribe" appliesTo {
  principal: Identity,
  resource: Topic,
  context: { "now_secs": Long, "declared": Bool, "implied": Bool },
};
action "update-configuration" appliesTo {
  principal: Identity,
  resource: [Variable, Agent],
  context: { "now_secs": Long },
};
action "update-runtime" appliesTo {
  principal: Identity,
  resource: Section,
  context: { "now_secs": Long },
};
action "run-command" appliesTo {
  principal: Identity,
  resource: Command,
  context: { "now_secs": Long },
};
"#;

/// The schema, parsed once.
pub fn schema() -> &'static Schema {
    static PARSED: OnceLock<Schema> = OnceLock::new();
    PARSED.get_or_init(|| {
        Schema::from_cedarschema_str(SCHEMA)
            .map(|(schema, _warnings)| schema)
            .expect("the schema carried in the code parses")
    })
}

/// Whether `policies` speak the schema, strictly; every problem, if not. Cedar's warnings are problems too: a rule
/// that can never apply (an action on a resource type it does not apply to) is almost certainly a mistake, and so
/// is one whose text could be read two ways.
pub fn validate(policies: &PolicySet) -> Result<(), Vec<String>> {
    let result = Validator::new(schema().clone()).validate(policies, ValidationMode::Strict);
    let problems: Vec<String> = result
        .validation_errors()
        .map(|e| e.to_string())
        .chain(result.validation_warnings().map(|w| w.to_string()))
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(source: &str) -> Result<(), Vec<String>> {
        validate(&source.parse::<PolicySet>().expect("it parses"))
    }

    #[test]
    fn the_schema_parses() {
        schema();
    }

    #[test]
    fn the_policies_polar_ships_and_documents_speak_the_schema() {
        check(include_str!("../../../../infra/seeds/signed/policy.cedar")).unwrap();
        check(r#"permit (principal in Group::"credential-managers", action == Action::"retrieve", resource)
                 when { resource.hasTag("kind") && resource.getTag("kind") == "git-token" };"#)
        .unwrap();
        check(r#"permit (principal in Role::"policy-author", action == Action::"update-runtime", resource == Section::"credentials-agent/policy");"#)
            .unwrap();
        check(r#"permit (principal in Capability::"run-command", action == Action::"run-command", resource);"#)
            .unwrap();
        check(r#"forbid (principal, action == Action::"publish", resource == Topic::"kubernetes.cluster.consumer.Pod")
                 unless { context.declared };"#)
        .unwrap();
        check(r#"permit (principal, action == Action::"subscribe", resource) when { principal has workload && principal.workload == "kube-consumer" };"#)
            .unwrap();
    }

    #[test]
    fn a_policy_that_does_not_speak_the_schema_is_refused_with_the_reason() {
        for (source, why) in [
            // a misspelt attribute
            (
                r#"permit (principal, action == Action::"retrieve", resource) when { resource.ownr == principal };"#,
                "ownr",
            ),
            // an optional attribute read without `has`
            (
                r#"permit (principal, action == Action::"retrieve", resource) when { resource.owner == principal };"#,
                "owner",
            ),
            // a label read as an attribute, not a tag
            (
                r#"permit (principal, action == Action::"retrieve", resource) when { resource.kind == "token" };"#,
                "kind",
            ),
            // an action that does not exist
            (
                r#"permit (principal, action == Action::"reap", resource);"#,
                "reap",
            ),
            // an action on a resource it does not apply to
            (
                r#"permit (principal, action == Action::"publish", resource == Secret::"x");"#,
                "impossible",
            ),
            // a context the action does not carry
            (
                r#"permit (principal, action == Action::"retrieve", resource) when { context.declared };"#,
                "declared",
            ),
            // capabilities are parents now, not an attribute
            (
                r#"permit (principal, action, resource) when { principal.capabilities.contains("grant") };"#,
                "capabilities",
            ),
        ] {
            let problems = match check(source) {
                Err(problems) => problems.join("; "),
                Ok(()) => panic!("this validated, and must not: {source}"),
            };
            assert!(problems.contains(why), "{source}: {problems}");
        }
    }
}
