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

//! Whether the state agent answers a request, as one pure function (docs/architecture/soft-state.md, "The rules it
//! keeps"): a request with no verified asker is refused, and so is one whose label or property is not a plain
//! identifier; then Cedar decides (`Action::"read-state"` on `Label::"<label>"`), a permit adding a read and a forbid
//! removing one; where policy is silent, the floor decides; and what nothing admits is refused. Policy that could not be
//! consulted is no decision, so the asker is told to try again.
//!
//! A refusal names only what the asker sent and its own identity: never another label, another workload, or what the
//! asker may read.

use crate::floor::Floor;
use polar_agent_builder::graph_schema::is_identifier;
use polar_agent_builder::topic_gate::PolicySaid;
use polar_policy::Decision;
use soft_state_common::StateRequest;

/// What the state agent does with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judged {
    /// Read the largest `property` of the nodes labelled `label`; both are plain identifiers.
    Max { label: String, property: String },
    /// Refused, for this reason, which goes back to the asker.
    Refused(String),
    /// Not decided: the asker should ask again.
    Unavailable(String),
}

/// What is asked of policy, once the request has passed [`checked`]: the asker, the action, the resource, and whether
/// the floor declares the read (`context.declared`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyQuestion<'a> {
    pub identity: &'a str,
    pub label: &'a str,
    pub declared: bool,
}

impl PolicyQuestion<'_> {
    pub const ACTION: &'static str = r#"Action::"read-state""#;

    /// `Label::"<label>"`; the label is a plain identifier, so it needs no escaping.
    pub fn resource(&self) -> String {
        format!(r#"Label::"{}""#, self.label)
    }
}

/// The request's label and property, and its asker, if the request is one the state agent will put to policy: an
/// asker the broker vouched for, and names that can go into a query. A refusal otherwise.
pub fn checked<'a>(
    floor: &Floor,
    identity: Option<&'a str>,
    request: &'a StateRequest,
) -> Result<PolicyQuestion<'a>, String> {
    let Some(identity) = identity else {
        return Err("a request with no verified asker is refused".to_string());
    };
    let StateRequest::Max { label, property } = request;
    if !is_identifier(label) {
        return Err("the label is not a plain identifier ([A-Za-z_][A-Za-z0-9_]*)".to_string());
    }
    if !is_identifier(property) {
        return Err("the property is not a plain identifier ([A-Za-z_][A-Za-z0-9_]*)".to_string());
    }
    Ok(PolicyQuestion {
        identity,
        label,
        declared: floor.declares_read(identity, label),
    })
}

/// The decision about `request` from `identity`, given what policy `said` about [`checked`]'s question. Pure.
pub fn decide(
    floor: &Floor,
    identity: Option<&str>,
    request: &StateRequest,
    said: &PolicySaid,
) -> Judged {
    let question = match checked(floor, identity, request) {
        Ok(question) => question,
        Err(why) => return Judged::Refused(why),
    };
    let StateRequest::Max { property, .. } = request;
    let admitted = || Judged::Max {
        label: question.label.to_string(),
        property: property.clone(),
    };
    match said {
        PolicySaid::Decided(Decision::Allow) => admitted(),
        PolicySaid::Decided(Decision::Deny) => Judged::Refused(format!(
            "policy forbids {} to read {}",
            question.identity, question.label
        )),
        PolicySaid::Decided(Decision::NotDetermined)
        | PolicySaid::NoPolicyLoaded
        | PolicySaid::NotRunning => {
            if question.declared {
                admitted()
            } else {
                Judged::Refused(format!(
                    "{} may not read {}: the catalog does not say it does, and no policy permits it",
                    question.identity, question.label
                ))
            }
        }
        // Why it could not be consulted is the agent's to log, not the asker's to read.
        PolicySaid::Unavailable(_) => {
            Judged::Unavailable("policy could not be consulted; ask again".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floor::Entry;
    use proptest::prelude::*;

    fn id(workload: &str) -> String {
        format!("{workload}.polar.serviceaccount.cluster.local")
    }

    fn max(label: &str, property: &str) -> StateRequest {
        StateRequest::Max {
            label: label.to_string(),
            property: property.to_string(),
        }
    }

    fn floor() -> Floor {
        Floor {
            format: 1,
            workloads: vec![
                Entry {
                    workload: "jira-observer".into(),
                    identity: id("jira-observer"),
                    owns: vec![],
                    reads: vec!["JiraIssue".into()],
                },
                Entry {
                    workload: "jira-processor".into(),
                    identity: id("jira-processor"),
                    owns: vec!["JiraIssue".into(), "JiraUser".into()],
                    reads: vec![],
                },
            ],
        }
    }

    const SILENT: PolicySaid = PolicySaid::Decided(Decision::NotDetermined);

    #[test]
    fn the_floor_admits_a_declared_read_and_nothing_else() {
        let floor = floor();
        let asker = id("jira-observer");
        assert_eq!(
            decide(
                &floor,
                Some(&asker),
                &max("JiraIssue", "updated_ms"),
                &SILENT
            ),
            Judged::Max {
                label: "JiraIssue".into(),
                property: "updated_ms".into()
            }
        );
        assert!(matches!(
            decide(&floor, Some(&asker), &max("JiraUser", "x"), &SILENT),
            Judged::Refused(_)
        ));
        // The owner does not read through the state agent unless it says so.
        assert!(matches!(
            decide(
                &floor,
                Some(&id("jira-processor")),
                &max("JiraIssue", "x"),
                &SILENT
            ),
            Judged::Refused(_)
        ));
        for said in [PolicySaid::NoPolicyLoaded, PolicySaid::NotRunning] {
            assert!(matches!(
                decide(&floor, Some(&asker), &max("JiraIssue", "x"), &said),
                Judged::Max { .. }
            ));
        }
    }

    #[test]
    fn policy_overrides_the_floor_both_ways() {
        let floor = floor();
        let asker = id("jira-observer");
        assert!(matches!(
            decide(
                &floor,
                Some(&asker),
                &max("JiraIssue", "x"),
                &PolicySaid::Decided(Decision::Deny)
            ),
            Judged::Refused(ref why) if why.contains("policy forbids")
        ));
        assert!(matches!(
            decide(
                &floor,
                Some(&asker),
                &max("KubePod", "x"),
                &PolicySaid::Decided(Decision::Allow)
            ),
            Judged::Max { .. }
        ));
        assert!(matches!(
            decide(
                &floor,
                Some(&asker),
                &max("JiraIssue", "x"),
                &PolicySaid::Unavailable("timeout talking to KubePod's agent".into())
            ),
            Judged::Unavailable(ref why) if !why.contains("KubePod")
        ));
    }

    #[test]
    fn no_asker_or_a_name_that_is_not_an_identifier_is_refused_before_policy() {
        let floor = floor();
        let allow = PolicySaid::Decided(Decision::Allow);
        assert!(matches!(
            decide(&floor, None, &max("JiraIssue", "updated_ms"), &allow),
            Judged::Refused(ref why) if why.contains("no verified asker")
        ));
        let asker = id("jira-observer");
        for (label, property) in [
            ("JiraIssue`) DETACH DELETE n //", "x"),
            ("JiraIssue", "x } RETURN 1 //"),
            ("", "x"),
            ("JiraIssue", "1x"),
        ] {
            assert!(
                matches!(
                    decide(&floor, Some(&asker), &max(label, property), &allow),
                    Judged::Refused(_)
                ),
                "{label} {property}"
            );
            assert!(checked(&floor, Some(&asker), &max(label, property)).is_err());
        }
        let request = max("JiraIssue", "updated_ms");
        let question = checked(&floor, Some(&asker), &request).unwrap();
        assert!(question.declared);
        assert_eq!(question.resource(), r#"Label::"JiraIssue""#);
    }

    // ── Properties ──────────────────────────────────────────────────────────

    const WORKLOADS: &[&str] = &["alpha", "beta", "gamma"];

    fn label() -> impl Strategy<Value = String> {
        "[A-E][a-c]{0,2}"
    }

    fn name() -> impl Strategy<Value = String> {
        prop_oneof![
            4 => "[A-Ea-c_][A-Ea-c0-9_]{0,3}",
            1 => "\\PC{0,6}",
        ]
    }

    fn floor_strategy() -> impl Strategy<Value = Floor> {
        prop::collection::vec(
            (
                prop::collection::vec(label(), 0..4),
                prop::collection::vec(label(), 0..4),
            ),
            WORKLOADS.len(),
        )
        .prop_map(|labels| Floor {
            format: 1,
            workloads: WORKLOADS
                .iter()
                .zip(labels)
                .map(|(workload, (owns, reads))| Entry {
                    workload: workload.to_string(),
                    identity: id(workload),
                    owns,
                    reads,
                })
                .collect(),
        })
    }

    fn asker() -> impl Strategy<Value = Option<String>> {
        prop_oneof![
            1 => Just(None),
            4 => prop::sample::select(WORKLOADS).prop_map(|w| Some(id(w))),
            1 => "[a-z]{1,6}".prop_map(|w| Some(format!("{w}.other.serviceaccount.cluster.local"))),
        ]
    }

    fn said() -> impl Strategy<Value = PolicySaid> {
        prop_oneof![
            Just(PolicySaid::Decided(Decision::Allow)),
            Just(PolicySaid::Decided(Decision::Deny)),
            Just(PolicySaid::Decided(Decision::NotDetermined)),
            Just(PolicySaid::NoPolicyLoaded),
            Just(PolicySaid::NotRunning),
            label().prop_map(PolicySaid::Unavailable),
        ]
    }

    fn plain(name: &str) -> bool {
        is_identifier(name)
    }

    proptest! {
        /// The decision is the stated rule for every input: no asker or a name that is not an identifier, refused;
        /// then a permit admits, a forbid refuses, an unavailable policy decides nothing; where policy is silent the
        /// floor admits exactly the reads it declares for that very identity; anything else is refused.
        #[test]
        fn the_decision_follows_the_rule(
            floor in floor_strategy(),
            asker in asker(),
            label in name(),
            property in name(),
            said in said(),
        ) {
            let request = max(&label, &property);
            let got = decide(&floor, asker.as_deref(), &request, &said);
            let declared = asker.as_deref().is_some_and(|a| {
                floor.workloads.iter().any(|e| e.identity == a && e.reads.contains(&label))
            });
            let expected = if asker.is_none() || !plain(&label) || !plain(&property) {
                "refused"
            } else {
                match said {
                    PolicySaid::Decided(Decision::Allow) => "read",
                    PolicySaid::Decided(Decision::Deny) => "refused",
                    PolicySaid::Unavailable(_) => "unavailable",
                    _ if declared => "read",
                    _ => "refused",
                }
            };
            let kind = match &got {
                Judged::Max { label: l, property: p } => {
                    prop_assert_eq!(l, &label);
                    prop_assert_eq!(p, &property);
                    "read"
                }
                Judged::Refused(_) => "refused",
                Judged::Unavailable(_) => "unavailable",
            };
            prop_assert_eq!(kind, expected);
        }

        /// Default deny: with policy silent or absent, an asker the floor does not list, or a label the floor does not
        /// say it reads, is never read for, whatever it owns or others read.
        #[test]
        fn what_the_floor_does_not_declare_is_never_read_unless_policy_permits(
            floor in floor_strategy(),
            asker in asker(),
            label in label(),
            said in prop_oneof![
                Just(PolicySaid::Decided(Decision::NotDetermined)),
                Just(PolicySaid::NoPolicyLoaded),
                Just(PolicySaid::NotRunning),
            ],
        ) {
            let undeclared = asker.as_deref().is_none_or(|a| !floor.declares_read(a, &label));
            prop_assume!(undeclared);
            let got = decide(&floor, asker.as_deref(), &max(&label, "p"), &said);
            prop_assert!(matches!(got, Judged::Refused(_)), "{:?}", got);
        }

        /// A refusal or an unavailable answer names nothing the asker did not send: no other label of the floor, no
        /// other workload, and nothing from policy's own failure.
        #[test]
        fn an_answer_that_is_not_a_read_never_leaks_another_label_or_workload(
            floor in floor_strategy(),
            asker in asker(),
            label in name(),
            property in name(),
            said in said(),
        ) {
            let why = match decide(&floor, asker.as_deref(), &max(&label, &property), &said) {
                Judged::Refused(why) | Judged::Unavailable(why) => why,
                Judged::Max { .. } => return Ok(()),
            };
            let sent: Vec<&str> = [label.as_str(), property.as_str()]
                .into_iter()
                .chain(asker.as_deref())
                .collect();
            let words: Vec<&str> = why
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .filter(|w| !w.is_empty())
                .collect();
            let floor_labels: std::collections::BTreeSet<&str> = floor
                .workloads
                .iter()
                .flat_map(|e| e.owns.iter().chain(e.reads.iter()))
                .map(String::as_str)
                .collect();
            for word in words {
                if floor_labels.contains(word) || WORKLOADS.contains(&word) {
                    prop_assert!(
                        sent.iter().any(|s| s.split('.').any(|part| part == word) || *s == word),
                        "`{}` leaked into {:?}", word, why
                    );
                }
            }
        }
    }
}
