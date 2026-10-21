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

//! The topics an agent uses: those it declares (`PolarAgent::TOPICS`) and those it uses because it is an
//! agent at all, or because of what it needs, which are implied (docs/architecture/topics.md, "Implied
//! topics"). The set is computed here, once, and nowhere else: the scaffold prints it, the catalog and the
//! certificates take it from the scaffold, and the agent's client refuses anything outside it. Every topic
//! is an exact name.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What an agent does with a topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Publish,
    Subscribe,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Publish => "publish",
            Access::Subscribe => "subscribe",
        }
    }
}

/// A topic an agent declares it uses, and what for. A topic it both publishes and subscribes to is
/// declared twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopicDecl {
    pub name: &'static str,
    pub access: Access,
    pub purpose: &'static str,
}

impl TopicDecl {
    pub const fn publish(name: &'static str, purpose: &'static str) -> Self {
        Self {
            name,
            access: Access::Publish,
            purpose,
        }
    }

    pub const fn subscribe(name: &'static str, purpose: &'static str) -> Self {
        Self {
            name,
            access: Access::Subscribe,
            purpose,
        }
    }
}

/// Why a topic implied for an agent is in its set: each rule is a fact about the agent, and these are all
/// the rules there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// Every agent that is a Cassini client: its configuration and command topics, asking for a replay and
    /// reporting outcomes, and the credentials agent's authority topics.
    Agent,
    /// An agent whose self-test is on probes itself on its own topic.
    SelfTest,
    /// An agent that needs `Encryption` has a crypto actor, which makes the credentials agent's requests.
    Encryption,
}

impl Rule {
    pub fn describe(self) -> &'static str {
        match self {
            Rule::Agent => "every agent that is a Cassini client",
            Rule::SelfTest => "the agent's self-test is on",
            Rule::Encryption => "the agent needs encryption (its crypto actor)",
        }
    }
}

/// The facts about an agent the rules read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts<'a> {
    /// The agent's name, which is its workload's.
    pub name: &'a str,
    pub cassini_client: bool,
    pub self_test: bool,
    pub encryption: bool,
}

/// The topics a rule implies for an agent.
pub fn implied_by(rule: Rule, name: &str) -> Vec<(String, Access)> {
    use credentials_agent_common::authority_update::{
        AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
        AUTHORITY_UPDATES_TOPIC,
    };
    use polar_scheduler_common as scheduler;
    match rule {
        Rule::Agent => vec![
            (scheduler::configuration_topic(name), Access::Subscribe),
            (scheduler::command_topic(name), Access::Subscribe),
            (
                scheduler::CONFIGURATION_REPLAY_TOPIC.to_string(),
                Access::Publish,
            ),
            (
                scheduler::CONFIGURATION_OUTCOME_TOPIC.to_string(),
                Access::Publish,
            ),
            (
                scheduler::COMMAND_OUTCOME_TOPIC.to_string(),
                Access::Publish,
            ),
            (AUTHORITY_UPDATES_TOPIC.to_string(), Access::Subscribe),
            (
                AUTHORITY_SNAPSHOT_REPLIES_TOPIC.to_string(),
                Access::Subscribe,
            ),
            (
                AUTHORITY_SNAPSHOT_REQUESTS_TOPIC.to_string(),
                Access::Publish,
            ),
        ],
        Rule::SelfTest => {
            let topic = crate::infrastructure::self_test_topic(name);
            vec![(topic.clone(), Access::Publish), (topic, Access::Subscribe)]
        }
        Rule::Encryption => polar_crypto::REQUEST_TOPICS
            .iter()
            .map(|t| (t.to_string(), Access::Publish))
            .chain(
                polar_crypto::REPLY_TOPICS
                    .iter()
                    .map(|t| (t.to_string(), Access::Subscribe)),
            )
            .collect(),
    }
}

/// The rules that apply to an agent.
pub fn rules_for(facts: &Facts) -> Vec<Rule> {
    let mut rules = Vec::new();
    if facts.cassini_client {
        rules.push(Rule::Agent);
        if facts.self_test {
            rules.push(Rule::SelfTest);
        }
        if facts.encryption {
            rules.push(Rule::Encryption);
        }
    }
    rules
}

/// Where a topic in the set comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Origin {
    /// The agent declared it, for this purpose.
    Declared { purpose: String },
    /// A rule implied it.
    Implied { rule: Rule, why: String },
}

/// One topic of the set, as the scaffold prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicEntry {
    pub name: String,
    pub access: Access,
    #[serde(flatten)]
    pub origin: Origin,
}

/// An agent's whole set of topics: declared and implied, each once, in a fixed order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopicSet {
    entries: BTreeMap<(String, Access), Origin>,
}

impl TopicSet {
    /// The set for an agent with these facts that declares `declared`. A topic both declared and implied
    /// is implied (and its declaration a problem, see [`problems`]).
    pub fn compute(facts: &Facts, declared: &[TopicDecl]) -> Self {
        let mut entries = BTreeMap::new();
        for decl in declared {
            entries
                .entry((decl.name.to_string(), decl.access))
                .or_insert_with(|| Origin::Declared {
                    purpose: decl.purpose.to_string(),
                });
        }
        for rule in rules_for(facts) {
            for key in implied_by(rule, facts.name) {
                entries.insert(
                    key,
                    Origin::Implied {
                        rule,
                        why: rule.describe().to_string(),
                    },
                );
            }
        }
        Self { entries }
    }

    /// Whether the agent may use `topic` this way.
    pub fn allows(&self, topic: &str, access: Access) -> bool {
        self.entries.contains_key(&(topic.to_string(), access))
    }

    pub fn entries(&self) -> Vec<TopicEntry> {
        self.entries
            .iter()
            .map(|((name, access), origin)| TopicEntry {
                name: name.clone(),
                access: *access,
                origin: origin.clone(),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What is wrong with an agent's declarations: an empty or padded name, a topic declared twice the same
/// way, one with no purpose, or one that a rule already implies.
pub fn problems(facts: &Facts, declared: &[TopicDecl]) -> Vec<String> {
    let mut problems = Vec::new();
    let implied: BTreeMap<(String, Access), Rule> = rules_for(facts)
        .into_iter()
        .flat_map(|rule| {
            implied_by(rule, facts.name)
                .into_iter()
                .map(move |key| (key, rule))
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for decl in declared {
        let what = format!("the topic `{}` ({})", decl.name, decl.access.as_str());
        if decl.name.is_empty()
            || decl.name.trim() != decl.name
            || decl.name.contains(char::is_whitespace)
        {
            problems.push(format!("{what} is not a topic name"));
        }
        if decl.purpose.trim().is_empty() {
            problems.push(format!(
                "{what} has no purpose: say what the agent uses it for"
            ));
        }
        if !seen.insert((decl.name, decl.access)) {
            problems.push(format!("{what} is declared twice"));
        }
        if let Some(rule) = implied.get(&(decl.name.to_string(), decl.access)) {
            problems.push(format!(
                "{what} is implied ({}): do not declare it",
                rule.describe()
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn agent(name: &str) -> Facts<'_> {
        Facts {
            name,
            cassini_client: true,
            self_test: true,
            encryption: false,
        }
    }

    const DECLARED: &[TopicDecl] = &[
        TopicDecl::publish("kubernetes.cluster.consumer.Pod", "the events it observes"),
        TopicDecl::subscribe(
            "polar.kubernetes.cluster.resolution",
            "what the resolver found",
        ),
    ];

    #[test]
    fn an_agent_uses_its_own_configuration_and_command_topics_and_the_authority_topics() {
        let set = TopicSet::compute(&agent("kube-observer"), &[]);
        assert!(set.allows("agent.kube-observer.configuration", Access::Subscribe));
        assert!(set.allows("agent.kube-observer.command", Access::Subscribe));
        assert!(set.allows("polar-authority-updates", Access::Subscribe));
        assert!(set.allows("polar-authority-snapshot-requests", Access::Publish));
        assert!(set.allows("selftest.kube-observer", Access::Publish));
        assert!(set.allows("selftest.kube-observer", Access::Subscribe));
        // Not another agent's, and not the other way round.
        assert!(!set.allows("agent.kube-consumer.configuration", Access::Subscribe));
        assert!(!set.allows("agent.kube-observer.configuration", Access::Publish));
    }

    #[test]
    fn encryption_brings_the_credentials_agents_requests_and_replies() {
        let facts = Facts {
            encryption: true,
            ..agent("kube-consumer")
        };
        let set = TopicSet::compute(&facts, &[]);
        assert!(set.allows("credentials-store-secret-requests", Access::Publish));
        assert!(set.allows("credentials-store-secret-replies", Access::Subscribe));
        assert!(
            !TopicSet::compute(&agent("kube-consumer"), &[])
                .allows("credentials-store-secret-requests", Access::Publish)
        );
    }

    #[test]
    fn a_service_that_is_not_a_cassini_client_has_no_implied_topics() {
        let facts = Facts {
            cassini_client: false,
            ..agent("cassini")
        };
        assert!(TopicSet::compute(&facts, &[]).is_empty());
    }

    #[test]
    fn each_topic_says_where_it_came_from() {
        let set = TopicSet::compute(&agent("kube-observer"), DECLARED);
        let entries = set.entries();
        let pod = entries
            .iter()
            .find(|e| e.name == "kubernetes.cluster.consumer.Pod")
            .unwrap();
        assert_eq!(
            pod.origin,
            Origin::Declared {
                purpose: "the events it observes".into()
            }
        );
        let own = entries
            .iter()
            .find(|e| e.name == "agent.kube-observer.configuration")
            .unwrap();
        assert!(matches!(
            own.origin,
            Origin::Implied {
                rule: Rule::Agent,
                ..
            }
        ));
    }

    #[test]
    fn a_declaration_that_is_wrong_is_a_problem() {
        let facts = agent("kube-observer");
        let declared = [
            TopicDecl::publish("", "nothing"),
            TopicDecl::publish("a topic", "spaced"),
            TopicDecl::publish("x", ""),
            TopicDecl::publish("y", "twice"),
            TopicDecl::publish("y", "twice"),
            TopicDecl::subscribe("agent.kube-observer.configuration", "its configuration"),
        ];
        let said = problems(&facts, &declared).join("\n");
        assert!(said.contains("`` (publish) is not a topic name"), "{said}");
        assert!(
            said.contains("`a topic` (publish) is not a topic name"),
            "{said}"
        );
        assert!(said.contains("`x` (publish) has no purpose"), "{said}");
        assert!(said.contains("`y` (publish) is declared twice"), "{said}");
        assert!(said.contains("is implied (every agent"), "{said}");
        assert!(problems(&facts, DECLARED).is_empty());
    }

    fn topic() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9.:-]{0,24}"
    }

    fn decls() -> impl Strategy<Value = Vec<(String, bool, String)>> {
        prop::collection::vec((topic(), any::<bool>(), "[a-z ]{1,12}"), 0..12)
    }

    fn facts_strategy() -> impl Strategy<Value = (String, bool, bool, bool)> {
        (
            "[a-z][a-z0-9-]{0,16}",
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        )
    }

    /// Leaks the generated strings, so they can stand where the code has `&'static str`.
    fn leak(raw: &[(String, bool, String)]) -> Vec<TopicDecl> {
        raw.iter()
            .map(|(name, publish, purpose)| {
                let name: &'static str = Box::leak(name.clone().into_boxed_str());
                let purpose: &'static str = Box::leak(purpose.clone().into_boxed_str());
                if *publish {
                    TopicDecl::publish(name, purpose)
                } else {
                    TopicDecl::subscribe(name, purpose)
                }
            })
            .collect()
    }

    proptest! {
        /// The set is a function of the facts and of what is declared, not of the order it is declared in.
        #[test]
        fn the_set_does_not_depend_on_the_order_of_the_declarations(
            raw in decls(),
            (name, cassini, self_test, encryption) in facts_strategy(),
            seed in any::<u64>(),
        ) {
            let facts = Facts { name: &name, cassini_client: cassini, self_test, encryption };
            let declared = leak(&raw);
            let mut shuffled = declared.clone();
            // A deterministic permutation from the seed.
            let n = shuffled.len();
            if n > 1 {
                let mut s = seed;
                for i in (1..n).rev() {
                    s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    shuffled.swap(i, (s >> 33) as usize % (i + 1));
                }
            }
            prop_assert_eq!(
                TopicSet::compute(&facts, &declared).entries(),
                TopicSet::compute(&facts, &shuffled).entries()
            );
        }

        /// The set is exactly the declared topics and those the agent's rules imply: nothing is lost and
        /// nothing is added, and an implied topic always says so.
        #[test]
        fn the_set_is_the_union_of_the_declared_and_the_implied(
            raw in decls(),
            (name, cassini, self_test, encryption) in facts_strategy(),
        ) {
            let facts = Facts { name: &name, cassini_client: cassini, self_test, encryption };
            let declared = leak(&raw);
            let set = TopicSet::compute(&facts, &declared);
            let implied: std::collections::BTreeSet<(String, Access)> = rules_for(&facts)
                .into_iter()
                .flat_map(|r| implied_by(r, &name))
                .collect();
            let mut expected: std::collections::BTreeSet<(String, Access)> = declared
                .iter()
                .map(|d| (d.name.to_string(), d.access))
                .collect();
            expected.extend(implied.iter().cloned());
            let got: std::collections::BTreeSet<(String, Access)> =
                set.entries().into_iter().map(|e| (e.name, e.access)).collect();
            prop_assert_eq!(&got, &expected);
            for entry in set.entries() {
                let key = (entry.name.clone(), entry.access);
                prop_assert_eq!(
                    matches!(entry.origin, Origin::Implied { .. }),
                    implied.contains(&key)
                );
            }
        }
    }
}
