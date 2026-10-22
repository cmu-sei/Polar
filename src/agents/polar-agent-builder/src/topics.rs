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

use polar_env::{Defaulted, Described, Section};
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

/// What makes an agent publish on a topic, as its code knows it (docs/architecture/authority-model.md, "Decisions":
/// local causes, derived paths, and the vocabulary of causes). An agent says only what its own code does: the
/// authority derives the paths through the system from everyone's causes. Every cause but `Topic` is an entry, a way
/// in from outside the agent's subscriptions, which a subscription may answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A message arrived on a topic the agent subscribes to.
    Topic(&'static str),
    /// A request on a route the agent serves.
    Http(&'static str),
    /// The agent registered with Cassini (again).
    Registered,
    /// The agent's own interval.
    Timer(&'static str),
    /// Something outside Polar the agent watches.
    Outside(&'static str),
}

/// A [`Cause`] as the set holds it and the scaffold prints it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum CauseEntry {
    Topic(String),
    Http(String),
    Registered,
    Timer(String),
    Outside(String),
}

impl From<Cause> for CauseEntry {
    fn from(cause: Cause) -> Self {
        match cause {
            Cause::Topic(name) => CauseEntry::Topic(name.to_string()),
            Cause::Http(route) => CauseEntry::Http(route.to_string()),
            Cause::Registered => CauseEntry::Registered,
            Cause::Timer(name) => CauseEntry::Timer(name.to_string()),
            Cause::Outside(source) => CauseEntry::Outside(source.to_string()),
        }
    }
}

impl CauseEntry {
    /// The name it carries, if any: a topic, a route, a timer, a source.
    fn name(&self) -> Option<&str> {
        match self {
            CauseEntry::Topic(n)
            | CauseEntry::Http(n)
            | CauseEntry::Timer(n)
            | CauseEntry::Outside(n) => Some(n),
            CauseEntry::Registered => None,
        }
    }
}

/// What a topic of the set says about causes: for a publish, what causes it, or why nothing does; for a
/// subscription, the entry it answers, if it answers one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Causes {
    causes: Vec<CauseEntry>,
    answers: Option<CauseEntry>,
    unprompted: Option<String>,
}

/// A topic an agent declares it uses, and what for. A topic it both publishes and subscribes to is
/// declared twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopicDecl {
    pub name: &'static str,
    pub access: Access,
    pub purpose: &'static str,
    /// Why nothing in the catalog is on the other side of it yet, when nothing is: no publisher yet, or a reader
    /// outside the catalog. A declared topic with no other side and no reason fails the catalog's check
    /// (docs/architecture/policy-enforcement.md, "Decisions").
    pub waits: Option<&'static str>,
    /// For a publish, what causes it ([`because`](Self::because)).
    pub causes: &'static [Cause],
    /// For a subscription, the entry it answers ([`answers`](Self::answers)).
    pub answers: Option<Cause>,
    /// For a publish nothing causes, why the agent publishes it of its own accord.
    pub unprompted: Option<&'static str>,
}

impl TopicDecl {
    pub const fn publish(name: &'static str, purpose: &'static str) -> Self {
        Self {
            name,
            access: Access::Publish,
            purpose,
            waits: None,
            causes: &[],
            answers: None,
            unprompted: None,
        }
    }

    pub const fn subscribe(name: &'static str, purpose: &'static str) -> Self {
        Self {
            name,
            access: Access::Subscribe,
            purpose,
            waits: None,
            causes: &[],
            answers: None,
            unprompted: None,
        }
    }

    /// The same declaration, saying why nothing in the catalog is on its other side.
    pub const fn waiting(self, reason: &'static str) -> Self {
        Self {
            waits: Some(reason),
            ..self
        }
    }

    /// The same publish, saying what causes the agent to make it.
    pub const fn because(self, causes: &'static [Cause]) -> Self {
        Self { causes, ..self }
    }

    /// The same subscription, saying it answers `entry`: what the agent sent when the entry came in comes back here.
    pub const fn answers(self, entry: Cause) -> Self {
        Self {
            answers: Some(entry),
            ..self
        }
    }

    /// The same publish, which nothing causes: the agent publishes it of its own accord, for `reason`.
    pub const fn of_its_own_accord(self, reason: &'static str) -> Self {
        Self {
            unprompted: Some(reason),
            ..self
        }
    }

    fn causes(&self) -> Causes {
        Causes {
            causes: self.causes.iter().map(|c| CauseEntry::from(*c)).collect(),
            answers: self.answers.map(CauseEntry::from),
            unprompted: self.unprompted.map(str::to_string),
        }
    }
}

/// Where a family's workload goes in its pattern.
pub const WORKLOAD_SLOT: &str = "{workload}";

/// A topic an agent uses once for each workload it serves (the scheduler publishes each workload's configuration):
/// `pattern` holds [`WORKLOAD_SLOT`] once, and the builder expands it into one exact name per workload in
/// [`SERVED_WORKLOADS`], so no pattern reaches the set, the certificates or policy (docs/architecture/topics.md,
/// "Families").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyDecl {
    pub pattern: &'static str,
    pub access: Access,
    pub purpose: &'static str,
    /// For a publish, what causes it, as for a [`TopicDecl`].
    pub causes: &'static [Cause],
    /// For a publish nothing causes, why the agent publishes it of its own accord.
    pub unprompted: Option<&'static str>,
}

impl FamilyDecl {
    pub const fn publish(pattern: &'static str, purpose: &'static str) -> Self {
        Self {
            pattern,
            access: Access::Publish,
            purpose,
            causes: &[],
            unprompted: None,
        }
    }

    pub const fn subscribe(pattern: &'static str, purpose: &'static str) -> Self {
        Self {
            pattern,
            access: Access::Subscribe,
            purpose,
            causes: &[],
            unprompted: None,
        }
    }

    /// The same family, saying what causes the agent to publish on it.
    pub const fn because(self, causes: &'static [Cause]) -> Self {
        Self { causes, ..self }
    }

    /// The same family, which nothing causes: the agent publishes on it of its own accord, for `reason`.
    pub const fn of_its_own_accord(self, reason: &'static str) -> Self {
        Self {
            unprompted: Some(reason),
            ..self
        }
    }

    /// The family's topic for `workload`.
    pub fn topic_for(&self, workload: &str) -> String {
        self.pattern.replacen(WORKLOAD_SLOT, workload, 1)
    }
}

/// The workloads an agent with a family serves, comma-separated: the render writes the catalog's workloads here.
pub static SERVED_WORKLOADS: Defaulted<String> = Defaulted::text(
    "POLAR_SERVED_WORKLOADS",
    "the workloads this agent serves, comma-separated, from the catalog: each of its topic families is expanded \
     into one exact topic per workload (docs/architecture/topics.md, \"Families\")",
    "",
);

static FAMILY_VARIABLES: &[&dyn Described] = &[&SERVED_WORKLOADS];

/// The builder's section for an agent that declares a topic family; an agent with none does not have it.
pub static FAMILY_SECTION: Section = Section {
    name: "topic-families",
    vars: FAMILY_VARIABLES,
    relations: |_| Vec::new(),
};

/// The workloads named in `text` ([`SERVED_WORKLOADS`]'s value): each once, in order, without blanks.
pub fn served_workloads(text: &str) -> Vec<String> {
    let mut workloads: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    workloads.sort();
    workloads.dedup();
    workloads
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
    /// Every rule there is.
    pub const ALL: [Rule; 3] = [Rule::Agent, Rule::SelfTest, Rule::Encryption];

    /// The group whose members the rule applies to (docs/architecture/policy-enforcement.md, "Decisions"): the
    /// certificate carries it as `g:<group>`, so the broker knows what the agent knows.
    pub fn group(self) -> &'static str {
        match self {
            Rule::Agent => "cassini-client",
            Rule::SelfTest => "self-test",
            Rule::Encryption => "encryption",
        }
    }

    /// The rule a group stands for; none for a group that implies no topics.
    pub fn for_group(group: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|rule| rule.group() == group)
    }

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

/// The groups an agent with these facts is in: what its scaffold prints and its certificate carries.
pub fn groups_for(facts: &Facts) -> Vec<&'static str> {
    rules_for(facts).into_iter().map(Rule::group).collect()
}

/// What causes the topics `rule` implies for the agent `name` (docs/architecture/authority-model.md, "Decisions"): its
/// requests for a replay and for the authority are made when it registers (and the latter when it sees an update it
/// cannot check), its outcomes when a document or a command arrives, its probe on its own interval; its crypto
/// actor's requests are made when the agent encrypts or decrypts, which no topic or entry causes.
fn implied_causes(rule: Rule, name: &str) -> BTreeMap<(String, Access), Causes> {
    use credentials_agent_common::authority_update::{
        AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
        AUTHORITY_UPDATES_TOPIC,
    };
    use polar_scheduler_common as scheduler;
    let caused = |causes: Vec<CauseEntry>| Causes {
        causes,
        ..Causes::default()
    };
    let answering = |entry: CauseEntry| Causes {
        answers: Some(entry),
        ..Causes::default()
    };
    match rule {
        Rule::Agent => BTreeMap::from([
            (
                (scheduler::CONFIGURATION_REPLAY_TOPIC.to_string(), Access::Publish),
                caused(vec![CauseEntry::Registered]),
            ),
            (
                (scheduler::CONFIGURATION_OUTCOME_TOPIC.to_string(), Access::Publish),
                caused(vec![CauseEntry::Topic(scheduler::configuration_topic(name))]),
            ),
            (
                (scheduler::COMMAND_OUTCOME_TOPIC.to_string(), Access::Publish),
                caused(vec![CauseEntry::Topic(scheduler::command_topic(name))]),
            ),
            (
                (AUTHORITY_SNAPSHOT_REQUESTS_TOPIC.to_string(), Access::Publish),
                caused(vec![
                    CauseEntry::Registered,
                    CauseEntry::Topic(AUTHORITY_UPDATES_TOPIC.to_string()),
                ]),
            ),
            (
                (AUTHORITY_SNAPSHOT_REPLIES_TOPIC.to_string(), Access::Subscribe),
                answering(CauseEntry::Registered),
            ),
        ]),
        Rule::SelfTest => {
            let topic = crate::infrastructure::self_test_topic(name);
            let tick = || CauseEntry::Timer("self-test".to_string());
            BTreeMap::from([
                ((topic.clone(), Access::Publish), caused(vec![tick()])),
                ((topic, Access::Subscribe), answering(tick())),
            ])
        }
        Rule::Encryption => polar_crypto::REQUEST_TOPICS
            .iter()
            .map(|t| {
                (
                    (t.to_string(), Access::Publish),
                    Causes {
                        unprompted: Some(
                            "its crypto actor asks the credentials agent when the agent encrypts or decrypts".to_string(),
                        ),
                        ..Causes::default()
                    },
                )
            })
            .collect(),
    }
}

/// The implied topics of the workload `name` in `groups`, each with the rule that implied it: the one function the
/// agent computes its set with and the broker its floor, from the certificate's identity and `g:` names. A group that
/// implies nothing adds nothing.
pub fn implied_topics<G: AsRef<str>>(name: &str, groups: &[G]) -> Vec<((String, Access), Rule)> {
    groups
        .iter()
        .filter_map(|group| Rule::for_group(group.as_ref()))
        .flat_map(|rule| {
            implied_by(rule, name)
                .into_iter()
                .map(move |key| (key, rule))
        })
        .collect()
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
    /// The agent declared it, for this purpose; `waits` says why nothing in the catalog is on its other side.
    Declared {
        purpose: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waits: Option<String>,
    },
    /// One of the agent's families, expanded for one workload it serves.
    Family {
        pattern: String,
        workload: String,
        purpose: String,
    },
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
    /// For a publish, what causes it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<CauseEntry>,
    /// For a subscription, the entry it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<CauseEntry>,
    /// For a publish nothing causes, why the agent publishes it of its own accord.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unprompted: Option<String>,
}

/// An agent's whole set of topics: declared and implied, each once, in a fixed order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopicSet {
    entries: BTreeMap<(String, Access), Origin>,
    causes: BTreeMap<(String, Access), Causes>,
}

impl TopicSet {
    /// The set for an agent with these facts that declares `declared`. A topic both declared and implied
    /// is implied (and its declaration a problem, see [`problems`]).
    pub fn compute(facts: &Facts, declared: &[TopicDecl]) -> Self {
        Self::compute_with(facts, declared, &[], &[])
    }

    /// As [`compute`](Self::compute), with the agent's families expanded for each of `workloads`. A family
    /// topic that is also declared is declared, and one that is implied is implied.
    pub fn compute_with(
        facts: &Facts,
        declared: &[TopicDecl],
        families: &[FamilyDecl],
        workloads: &[String],
    ) -> Self {
        let mut entries = BTreeMap::new();
        let mut causes = BTreeMap::new();
        for decl in declared {
            let key = (decl.name.to_string(), decl.access);
            if let std::collections::btree_map::Entry::Vacant(slot) = entries.entry(key.clone()) {
                slot.insert(Origin::Declared {
                    purpose: decl.purpose.to_string(),
                    waits: decl.waits.map(str::to_string),
                });
                causes.insert(key, decl.causes());
            }
        }
        for family in families {
            for workload in workloads {
                let key = (family.topic_for(workload), family.access);
                if let std::collections::btree_map::Entry::Vacant(slot) = entries.entry(key.clone())
                {
                    slot.insert(Origin::Family {
                        pattern: family.pattern.to_string(),
                        workload: workload.clone(),
                        purpose: family.purpose.to_string(),
                    });
                    causes.insert(
                        key,
                        Causes {
                            causes: family.causes.iter().map(|c| CauseEntry::from(*c)).collect(),
                            answers: None,
                            unprompted: family.unprompted.map(str::to_string),
                        },
                    );
                }
            }
        }
        for (key, rule) in implied_topics(facts.name, &groups_for(facts)) {
            let implied = implied_causes(rule, facts.name)
                .remove(&key)
                .unwrap_or_default();
            causes.insert(key.clone(), implied);
            entries.insert(
                key,
                Origin::Implied {
                    rule,
                    why: rule.describe().to_string(),
                },
            );
        }
        Self { entries, causes }
    }

    /// Whether the agent may use `topic` this way.
    pub fn allows(&self, topic: &str, access: Access) -> bool {
        self.entries.contains_key(&(topic.to_string(), access))
    }

    /// Where `topic`, used this way, comes from in the set; none when it is not in it.
    pub fn origin(&self, topic: &str, access: Access) -> Option<&Origin> {
        self.entries.get(&(topic.to_string(), access))
    }

    /// What policy is told about `topic`, used this way: whether it is declared (a family's included) or implied.
    pub fn facts(&self, topic: &str, access: Access) -> polar_policy::TopicFacts {
        match self.origin(topic, access) {
            Some(Origin::Declared { .. } | Origin::Family { .. }) => polar_policy::TopicFacts {
                declared: true,
                implied: false,
            },
            Some(Origin::Implied { .. }) => polar_policy::TopicFacts {
                declared: false,
                implied: true,
            },
            None => polar_policy::TopicFacts::default(),
        }
    }

    pub fn entries(&self) -> Vec<TopicEntry> {
        self.entries
            .iter()
            .map(|((name, access), origin)| {
                let causes = self
                    .causes
                    .get(&(name.clone(), *access))
                    .cloned()
                    .unwrap_or_default();
                TopicEntry {
                    name: name.clone(),
                    access: *access,
                    origin: origin.clone(),
                    causes: causes.causes,
                    answers: causes.answers,
                    unprompted: causes.unprompted,
                }
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

/// What is wrong with an agent's families: a pattern without exactly one workload slot, one with no purpose, or
/// one declared twice the same way.
pub fn family_problems(families: &[FamilyDecl]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for family in families {
        let what = format!(
            "the topic family `{}` ({})",
            family.pattern,
            family.access.as_str()
        );
        if family.pattern.matches(WORKLOAD_SLOT).count() != 1 {
            problems.push(format!("{what} must hold {WORKLOAD_SLOT} exactly once"));
        }
        if family.pattern.contains(char::is_whitespace) {
            problems.push(format!("{what} is not a topic name"));
        }
        if family.purpose.trim().is_empty() {
            problems.push(format!(
                "{what} has no purpose: say what the agent uses it for"
            ));
        }
        if !seen.insert((family.pattern, family.access)) {
            problems.push(format!("{what} is declared twice"));
        }
    }
    problems
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
        if decl.waits.is_some_and(|reason| reason.trim().is_empty()) {
            problems.push(format!("{what} waits, but gives no reason"));
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
    problems.extend(cause_problems(
        &TopicSet::compute(facts, declared).entries(),
    ));
    problems
}

/// What is wrong with the causes of a set's topics (docs/architecture/authority-model.md, "Decisions"): a cause on a
/// subscription, an answer on a publish, a topic cause the agent does not subscribe to, an answer to an entry none of
/// its publishes takes or to a topic, a cause that names nothing, one named twice, and a publish both caused and
/// unprompted, or unprompted for no reason.
pub fn cause_problems(entries: &[TopicEntry]) -> Vec<String> {
    let subscribed: std::collections::BTreeSet<&str> = entries
        .iter()
        .filter(|e| e.access == Access::Subscribe)
        .map(|e| e.name.as_str())
        .collect();
    let taken: std::collections::BTreeSet<&CauseEntry> = entries
        .iter()
        .filter(|e| e.access == Access::Publish)
        .flat_map(|e| e.causes.iter())
        .filter(|c| !matches!(c, CauseEntry::Topic(_)))
        .collect();
    let mut problems = Vec::new();
    for entry in entries {
        let what = format!("the topic `{}` ({})", entry.name, entry.access.as_str());
        match entry.access {
            Access::Subscribe => {
                if !entry.causes.is_empty() || entry.unprompted.is_some() {
                    problems.push(format!("{what} is subscribed to, so nothing causes it"));
                }
            }
            Access::Publish => {
                if entry.answers.is_some() {
                    problems.push(format!(
                        "{what} answers an entry, but only a subscription can"
                    ));
                }
                if !entry.causes.is_empty() && entry.unprompted.is_some() {
                    problems.push(format!(
                        "{what} has a cause and says it publishes of its own accord"
                    ));
                }
            }
        }
        if entry
            .unprompted
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            problems.push(format!(
                "{what} publishes of its own accord, but gives no reason"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for cause in &entry.causes {
            if !seen.insert(cause) {
                problems.push(format!("{what} names a cause twice"));
            }
            match cause {
                CauseEntry::Topic(topic) if topic.trim().is_empty() => {
                    problems.push(format!("{what} names no topic as its cause"))
                }
                CauseEntry::Topic(topic) if !subscribed.contains(topic.as_str()) => problems.push(
                    format!("{what} is caused by `{topic}`, which the agent does not subscribe to"),
                ),
                other if other.name().is_some_and(|n| n.trim().is_empty()) => {
                    problems.push(format!("{what} names no entry"))
                }
                _ => {}
            }
        }
        match &entry.answers {
            Some(CauseEntry::Topic(_)) => {
                problems.push(format!("{what} answers a topic: an answer is to an entry"))
            }
            Some(entry_cause) if entry_cause.name().is_some_and(|n| n.trim().is_empty()) => {
                problems.push(format!("{what} names no entry"))
            }
            Some(entry_cause) if !taken.contains(entry_cause) => {
                problems.push(format!("{what} answers an entry the agent does not take"))
            }
            _ => {}
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
                purpose: "the events it observes".into(),
                waits: None,
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
    fn a_declaration_says_why_it_waits_and_the_set_carries_it() {
        let declared = [
            TopicDecl::publish("polar.provenance.events", "what it resolved")
                .waiting("read by build-processor, a chart outside the catalog"),
        ];
        let set = TopicSet::compute(&agent("oci-registry-resolver"), &declared);
        let entry = set
            .entries()
            .into_iter()
            .find(|e| e.name == "polar.provenance.events")
            .unwrap();
        assert_eq!(
            entry.origin,
            Origin::Declared {
                purpose: "what it resolved".into(),
                waits: Some("read by build-processor, a chart outside the catalog".into()),
            }
        );
        // In the scaffold's JSON it is `waits`, and a topic that does not wait has none.
        let json = serde_json::to_value(set.entries()).unwrap();
        assert!(
            json.to_string()
                .contains("\"waits\":\"read by build-processor")
        );
        assert!(
            !serde_json::to_string(&TopicSet::compute(&agent("x"), &[]).entries())
                .unwrap()
                .contains("waits")
        );
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
            TopicDecl::publish("z", "silent").waiting(" "),
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
        assert!(
            said.contains("`z` (publish) waits, but gives no reason"),
            "{said}"
        );
        assert!(problems(&facts, DECLARED).is_empty());
    }

    const FAMILY: FamilyDecl = FamilyDecl::publish(
        "agent.{workload}.configuration",
        "each workload's documents",
    );

    #[test]
    fn a_family_is_one_exact_topic_for_each_workload_it_serves() {
        let workloads = served_workloads("kube-observer, jira-observer,,kube-observer ");
        assert_eq!(workloads, ["jira-observer", "kube-observer"]);
        let set = TopicSet::compute_with(&agent("polar-scheduler"), &[], &[FAMILY], &workloads);
        assert!(set.allows("agent.kube-observer.configuration", Access::Publish));
        assert!(set.allows("agent.jira-observer.configuration", Access::Publish));
        assert!(!set.allows("agent.git-repo-observer.configuration", Access::Publish));
        // No pattern reaches the set.
        assert!(
            set.entries()
                .iter()
                .all(|e| !e.name.contains(WORKLOAD_SLOT))
        );
        // Serving no workload, the family adds nothing.
        assert_eq!(
            TopicSet::compute_with(&agent("polar-scheduler"), &[], &[FAMILY], &[]),
            TopicSet::compute(&agent("polar-scheduler"), &[])
        );
    }

    #[test]
    fn a_family_that_is_wrong_is_a_problem() {
        let said = family_problems(&[
            FamilyDecl::publish("agent.configuration", "no slot"),
            FamilyDecl::publish("{workload}.{workload}", "two slots"),
            FamilyDecl::publish("agent.{workload}.x", ""),
            FAMILY,
            FAMILY,
        ])
        .join("\n");
        assert!(
            said.contains("`agent.configuration` (publish) must hold"),
            "{said}"
        );
        assert!(
            said.contains("`{workload}.{workload}` (publish) must hold"),
            "{said}"
        );
        assert!(
            said.contains("`agent.{workload}.x` (publish) has no purpose"),
            "{said}"
        );
        assert!(said.contains("is declared twice"), "{said}");
        assert!(family_problems(&[FAMILY]).is_empty());
    }

    #[test]
    fn each_rule_is_a_group_and_the_group_brings_the_rule_back() {
        for rule in Rule::ALL {
            assert_eq!(Rule::for_group(rule.group()), Some(rule));
        }
        assert_eq!(Rule::for_group("operators"), None);
        let facts = Facts {
            encryption: true,
            ..agent("kube-consumer")
        };
        assert_eq!(
            groups_for(&facts),
            ["cassini-client", "self-test", "encryption"]
        );
        let service = Facts {
            cassini_client: false,
            ..agent("cassini")
        };
        assert!(groups_for(&service).is_empty());
    }

    #[test]
    fn a_group_that_implies_nothing_adds_nothing() {
        assert!(implied_topics("kube-observer", &["operators"]).is_empty());
        assert_eq!(
            implied_topics("kube-observer", &["cassini-client", "operators"]).len(),
            implied_topics("kube-observer", &["cassini-client"]).len()
        );
    }

    fn topic() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9.:-]{0,24}"
    }

    /// Declarations without a topic declared twice the same way, which is a declaration problem (`problems`).
    fn decls() -> impl Strategy<Value = Vec<(String, bool, String)>> {
        prop::collection::vec((topic(), any::<bool>(), "[a-z ]{1,12}"), 0..12).prop_map(|raw| {
            let mut seen = std::collections::BTreeSet::new();
            raw.into_iter()
                .filter(|(name, publish, _)| seen.insert((name.clone(), *publish)))
                .collect()
        })
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

        /// A family is expanded for every workload served and for nothing else, whatever order the workloads
        /// are named in and however often.
        #[test]
        fn a_family_expands_for_exactly_the_workloads_served(
            workloads in prop::collection::vec("[a-z][a-z0-9-]{0,12}", 0..8),
            seed in any::<u64>(),
        ) {
            let facts = agent("polar-scheduler");
            let text = workloads.join(",");
            let mut shuffled = workloads.clone();
            shuffled.reverse();
            if seed % 2 == 0 { shuffled.extend(workloads.iter().cloned()); }
            let a = TopicSet::compute_with(&facts, &[], &[FAMILY], &served_workloads(&text));
            let b = TopicSet::compute_with(&facts, &[], &[FAMILY], &served_workloads(&shuffled.join(", ")));
            prop_assert_eq!(a.entries(), b.entries());
            let family: std::collections::BTreeSet<String> = a
                .entries()
                .into_iter()
                .filter(|e| matches!(e.origin, Origin::Family { .. }))
                .map(|e| e.name)
                .collect();
            let expected: std::collections::BTreeSet<String> =
                workloads.iter().map(|w| FAMILY.topic_for(w)).collect();
            prop_assert_eq!(family, expected);
        }

        /// What the broker derives from a certificate (its identity's name and its groups) is exactly the implied part
        /// of the set the agent computes, for any agent.
        #[test]
        fn the_broker_s_implied_topics_are_the_agent_s(
            (name, cassini, self_test, encryption) in facts_strategy(),
        ) {
            let facts = Facts { name: &name, cassini_client: cassini, self_test, encryption };
            let from_certificate: std::collections::BTreeSet<(String, Access)> =
                implied_topics(&name, &groups_for(&facts)).into_iter().map(|(key, _)| key).collect();
            let from_agent: std::collections::BTreeSet<(String, Access)> = TopicSet::compute(&facts, &[])
                .entries()
                .into_iter()
                .map(|e| (e.name, e.access))
                .collect();
            prop_assert_eq!(from_certificate, from_agent);
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

    // -- causes (docs/architecture/authority-model.md, "Decisions": local causes, derived paths) --------------------

    const ROUTE: &str = "/workload/issue";

    #[test]
    fn a_publish_says_what_causes_it_and_the_scaffold_prints_it() {
        let declared = [
            TopicDecl::publish("issuance-requests", "a workload's issuance")
                .because(&[Cause::Http(ROUTE)]),
            TopicDecl::subscribe("issuance-replies", "the answer").answers(Cause::Http(ROUTE)),
            TopicDecl::publish("heartbeat", "that it is alive")
                .of_its_own_accord("it says so every minute"),
        ];
        let set = TopicSet::compute(&agent("credentials-agent-observer"), &declared);
        let printed = serde_json::to_value(set.entries()).unwrap();
        let find = |name: &str| {
            printed
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["name"] == name)
                .cloned()
                .unwrap()
        };
        assert_eq!(
            find("issuance-requests")["causes"],
            serde_json::json!([{ "kind": "http", "name": ROUTE }])
        );
        assert_eq!(
            find("issuance-replies")["answers"],
            serde_json::json!({ "kind": "http", "name": ROUTE })
        );
        assert_eq!(find("heartbeat")["unprompted"], "it says so every minute");
        // A topic with nothing to say about causes prints none of it.
        let plain = TopicSet::compute(&agent("x"), &[TopicDecl::subscribe("news", "the news")]);
        let plain = serde_json::to_value(plain.entries()).unwrap();
        let news = plain
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "news")
            .unwrap();
        assert!(
            news.get("causes").is_none()
                && news.get("answers").is_none()
                && news.get("unprompted").is_none()
        );
        assert!(problems(&agent("credentials-agent-observer"), &declared).is_empty());
    }

    #[test]
    fn a_cause_that_does_not_fit_its_declaration_is_refused() {
        let facts = agent("kube-observer");
        let refused = |declared: &[TopicDecl], expect: &str| {
            let found = problems(&facts, declared);
            assert!(
                found.iter().any(|p| p.contains(expect)),
                "expected a problem containing {expect:?}, got {found:?}"
            );
        };
        refused(
            &[TopicDecl::subscribe("news", "p").because(&[Cause::Registered])],
            "is subscribed to, so nothing causes it",
        );
        refused(
            &[TopicDecl::publish("news", "p").answers(Cause::Http(ROUTE))],
            "answers an entry, but only a subscription can",
        );
        refused(
            &[TopicDecl::publish("news", "p").because(&[Cause::Topic("weather")])],
            "is caused by `weather`, which the agent does not subscribe to",
        );
        refused(
            &[TopicDecl::subscribe("replies", "p").answers(Cause::Http(ROUTE))],
            "answers an entry the agent does not take",
        );
        refused(
            &[TopicDecl::subscribe("replies", "p").answers(Cause::Topic("news"))],
            "answers a topic: an answer is to an entry",
        );
        refused(
            &[TopicDecl::publish("news", "p").because(&[Cause::Http(" ")])],
            "names no entry",
        );
        refused(
            &[TopicDecl::publish("news", "p").of_its_own_accord(" ")],
            "of its own accord, but gives no reason",
        );
        refused(
            &[TopicDecl::subscribe("news", "p").of_its_own_accord("why")],
            "is subscribed to, so nothing causes it",
        );
        refused(
            &[TopicDecl::publish("news", "p").because(&[Cause::Registered, Cause::Registered])],
            "names a cause twice",
        );
        refused(
            &[TopicDecl::publish("news", "p")
                .because(&[Cause::Registered])
                .of_its_own_accord("why")],
            "has a cause and says it publishes of its own accord",
        );
        // What it subscribes to by rule may cause a publish, as may an entry another of its publishes takes.
        assert!(
            problems(
                &facts,
                &[
                    TopicDecl::publish("kubernetes.cluster.consumer.Pod", "what it saw").because(
                        &[
                            Cause::Topic("agent.kube-observer.command"),
                            Cause::Outside("the Kubernetes API")
                        ]
                    )
                ]
            )
            .is_empty()
        );
    }

    #[test]
    fn the_topics_an_agent_uses_by_rule_carry_their_causes() {
        let set = TopicSet::compute(&agent("kube-observer"), &[]);
        let entries = set.entries();
        let entry = |name: &str, access: Access| {
            entries
                .iter()
                .find(|e| e.name == name && e.access == access)
                .cloned()
                .unwrap()
        };
        use credentials_agent_common::authority_update::{
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
        };
        assert_eq!(
            entry(AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, Access::Publish).causes,
            [
                CauseEntry::Registered,
                CauseEntry::Topic("polar-authority-updates".to_string())
            ]
        );
        assert_eq!(
            entry(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, Access::Subscribe).answers,
            Some(CauseEntry::Registered)
        );
        assert_eq!(
            entry(
                polar_scheduler_common::CONFIGURATION_OUTCOME_TOPIC,
                Access::Publish
            )
            .causes,
            [CauseEntry::Topic(
                "agent.kube-observer.configuration".to_string()
            )]
        );
    }

    fn causes_strategy() -> impl Strategy<Value = Vec<(String, bool, u8)>> {
        prop::collection::vec(("[a-z][a-z.-]{0,10}", any::<bool>(), 0u8..4), 0..6)
    }

    proptest! {
        /// What the scaffold prints of a set, causes and all, reads back as the same set's entries.
        #[test]
        fn the_printed_entries_read_back_as_they_were(raw in causes_strategy()) {
            let declared: Vec<TopicDecl> = raw
                .iter()
                .map(|(name, publish, cause)| {
                    let name: &'static str = Box::leak(name.clone().into_boxed_str());
                    let cause = match cause {
                        0 => Cause::Registered,
                        1 => Cause::Http("/r"),
                        2 => Cause::Timer("tick"),
                        _ => Cause::Outside("the world"),
                    };
                    if *publish {
                        TopicDecl::publish(name, "p").because(Box::leak(Box::new([cause])))
                    } else {
                        TopicDecl::subscribe(name, "p").answers(cause)
                    }
                })
                .collect();
            let entries = TopicSet::compute(&agent("x"), &declared).entries();
            let text = serde_json::to_string(&entries).unwrap();
            let back: Vec<TopicEntry> = serde_json::from_str(&text).unwrap();
            prop_assert_eq!(back, entries);
        }

        /// The topics an agent uses by rule pass the checks an agent's own declarations must: every publish has a
        /// cause or a reason, every topic that causes one is one the agent subscribes to, and every answer is to
        /// an entry the agent takes. For any agent.
        #[test]
        fn the_topics_used_by_rule_meet_the_checks_on_causes(
            (name, cassini, self_test, encryption) in facts_strategy(),
        ) {
            let facts = Facts { name: &name, cassini_client: cassini, self_test, encryption };
            prop_assert!(cause_problems(&TopicSet::compute(&facts, &[]).entries()).is_empty());
            for entry in TopicSet::compute(&facts, &[]).entries() {
                if entry.access == Access::Publish {
                    prop_assert!(!entry.causes.is_empty() || entry.unprompted.is_some(), "{entry:?}");
                }
            }
        }
    }
}
