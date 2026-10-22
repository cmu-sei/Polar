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

//! What a pair allows, the invariants' goals, and the comparison of two pairs (docs/architecture/authority-model.md,
//! "Acceptance as a comparison").

use crate::facts::{Model, Pair, Person};
use crate::rules::{Node, Program};
use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, AUTHORITY_UPDATES_TOPIC,
};
use polar_agent_builder::policy_authority::entity;
use polar_agent_builder::topic_gate::{PolicySaid, rule};
use polar_agent_builder::topics::{Access, CauseEntry, Origin};
use polar_policy::{Evaluator, Facts, Requester, TopicFacts};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// `(identity, topic, access)` a pair allows: Cedar first, then the floor.
pub type Allowed = BTreeSet<(String, String, Access)>;

/// The two invariants the authority's code names ("Decisions": two invariants refuse; renewal is derived as their
/// dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Invariant {
    /// The credentials agent's updates reach every agent, and every agent's request for the authority is answered.
    HearingAuthority,
    /// A push to the git server reaches the credentials agent's configuration, where a new policy arrives.
    Correction,
}

impl fmt::Display for Invariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Invariant::HearingAuthority => "hearing authority",
            Invariant::Correction => "correction",
        })
    }
}

/// One thing an invariant needs, of the two shapes ("Decisions": the vocabulary of causes).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Goal {
    /// A delivery: what `entry` starts at `workload` reaches `to`'s subscription to `topic`.
    Delivers {
        workload: String,
        entry: CauseEntry,
        topic: String,
        to: String,
    },
    /// A round trip: what `entry` makes `workload` publish on `sent` comes back to where it is answered.
    Closes {
        workload: String,
        entry: CauseEntry,
        sent: String,
    },
}

impl fmt::Display for Goal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Goal::Delivers {
                workload,
                entry,
                topic,
                to,
            } => write!(
                f,
                "what {} at {workload} starts reaches {to} on `{topic}`",
                EntryName(entry)
            ),
            Goal::Closes {
                workload,
                entry,
                sent,
            } => write!(
                f,
                "{workload}'s round trip on `{sent}`, from {}, comes back",
                EntryName(entry)
            ),
        }
    }
}

/// An entry, as a reason says it.
struct EntryName<'a>(&'a CauseEntry);

impl fmt::Display for EntryName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            CauseEntry::Topic(t) => write!(f, "a message on `{t}`"),
            CauseEntry::Http(route) => write!(f, "a request on {route}"),
            CauseEntry::Registered => f.write_str("its registration"),
            CauseEntry::Timer(name) => write!(f, "its {name} timer"),
            CauseEntry::Outside(source) => write!(f, "{source}"),
        }
    }
}

/// A goal, judged under one pair: whether it holds, and the path that shows it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalOutcome {
    pub invariant: Invariant,
    pub goal: Goal,
    pub held: bool,
    /// The shortest path that holds it, from where it starts; none when it does not hold.
    pub witness: Option<Vec<Node>>,
}

impl GoalOutcome {
    /// The goal, said.
    pub fn describe(&self) -> String {
        format!("{}: {}", self.invariant, self.goal)
    }
}

/// What the rules derive under one pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    /// Every goal of every invariant this model has, in a fixed order.
    pub goals: Vec<GoalOutcome>,
    /// The invariants this model has no goal for (a deployment without the git server has no correction to check).
    pub not_applicable: Vec<Invariant>,
    pub edges: BTreeSet<(Node, Node)>,
    /// The edges between agents that live: what the invariants are judged over.
    pub held: BTreeSet<(Node, Node)>,
    pub lives: BTreeSet<String>,
    /// `(workload, entry, sent)`: the round trips that come back, over held edges.
    pub closes: BTreeSet<(String, CauseEntry, String)>,
    /// Renewal's round trip, when it closes.
    pub renewal_witness: Option<Vec<Node>>,
}

/// Why a goal that held would not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Broken {
    /// The proposed pair denies this request, which the current one allows.
    Forbidden {
        identity: String,
        topic: String,
        access: Access,
    },
    /// `workload` would no longer be renewed, so within one certificate lifetime it would carry nothing.
    NotRenewed {
        workload: String,
        because: Box<Broken>,
    },
    /// No request explains it (it should not happen: what flows changes only with what is allowed).
    Unexplained,
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Broken::Forbidden {
                identity,
                topic,
                access,
            } => write!(
                f,
                "the policy forbids {identity} to {} on `{topic}`",
                access.as_str()
            ),
            Broken::NotRenewed { workload, because } => write!(
                f,
                "within one certificate lifetime: renewal for {workload} no longer closes, since {because}"
            ),
            Broken::Unexplained => f.write_str("no single request explains it"),
        }
    }
}

/// A goal the current pair holds and the proposed one would not: a reason to refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loss {
    pub invariant: Invariant,
    pub goal: Goal,
    /// What used to hold it, under the current pair.
    pub witness: Vec<Node>,
    pub broken: Broken,
}

impl fmt::Display for Loss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} would stop: {} no longer: {}",
            self.invariant, self.goal, self.broken
        )
    }
}

/// What a change does, whether or not it is refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effect {
    pub gained: Vec<(String, String, Access)>,
    pub lost: Vec<(String, String, Access)>,
    pub began_closing: Vec<(String, CauseEntry, String)>,
    pub stopped_closing: Vec<(String, CauseEntry, String)>,
}

/// The judgement of a proposed pair against the current one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Judgement {
    /// Every goal lost: one is enough to refuse.
    pub losses: Vec<Loss>,
    pub effect: Effect,
    /// The groups document's keys that name no identity the authority knows: reported, not refused.
    pub unknown_keys: Vec<String>,
}

impl Judgement {
    pub fn refuses(&self) -> bool {
        !self.losses.is_empty()
    }
}

const ACCESSES: [Access; 2] = [Access::Publish, Access::Subscribe];

/// What `pair` allows every workload and every person to do on every topic the model knows: the policy first, then
/// the floor (a workload's own set), by the rule the topic gate and the broker apply (`topic_gate::rule`), so the
/// judgement and the enforcement cannot disagree. A policy that would not load is an error.
pub fn allowed_under(model: &Model, pair: &Pair, persons: &[Person]) -> Result<Allowed, String> {
    let evaluator = match &pair.policy {
        Some(source) => Some(Evaluator::new(source, &pair.hierarchy).map_err(|e| e.to_string())?),
        None => None,
    };
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let topics = model.topic_names();
    let mut allowed = Allowed::new();
    let mut judge_one = |identity: &str,
                         groups: &[String],
                         capabilities: &[cert_issuer_common::capability::Capability],
                         set: &BTreeMap<(String, Access), Origin>| {
        let groups: Vec<String> = pair
            .memberships
            .get(identity)
            .cloned()
            .unwrap_or_else(|| groups.to_vec());
        for topic in &topics {
            for access in ACCESSES {
                let origin = set.get(&(topic.clone(), access));
                let facts = TopicFacts {
                    declared: matches!(
                        origin,
                        Some(Origin::Declared { .. } | Origin::Family { .. })
                    ),
                    implied: matches!(origin, Some(Origin::Implied { .. })),
                };
                let said = match &evaluator {
                    None => PolicySaid::NoPolicyLoaded,
                    Some(evaluator) => {
                        match (entity("Action", access.as_str()), entity("Topic", topic)) {
                            (Some(action), Some(resource)) => match evaluator.decide(
                                &Requester {
                                    identity,
                                    groups: &groups,
                                    capabilities,
                                    permissions: &[],
                                },
                                &action,
                                &resource,
                                &Facts {
                                    topic: Some(facts),
                                    ..Facts::default()
                                },
                                now_secs,
                            ) {
                                Ok(decision) => PolicySaid::Decided(decision),
                                Err(e) => PolicySaid::Unavailable(e.to_string()),
                            },
                            _ => PolicySaid::Unavailable(format!(
                                "`{topic}` is not a name policy can be asked about"
                            )),
                        }
                    }
                };
                if rule(&said, origin.is_some(), topic, access).is_ok() {
                    allowed.insert((identity.to_string(), topic.clone(), access));
                }
            }
        }
    };
    for w in &model.workloads {
        let set: BTreeMap<(String, Access), Origin> = w
            .topics
            .iter()
            .map(|t| ((t.name.clone(), t.access), t.origin.clone()))
            .collect();
        judge_one(&w.identity, &w.groups, &[], &set);
    }
    for person in persons {
        judge_one(
            &person.identity,
            &person.groups,
            &person.capabilities,
            &BTreeMap::new(),
        );
    }
    Ok(allowed)
}

/// The goals of this model's invariants, and the invariants it has none for. The authority publisher is whichever
/// workload publishes the authority updates; correction is what a push to the git server (whichever workload takes
/// that entry) delivers to the publisher's own configuration topic; renewal is the round trip of whichever workload
/// takes `/workload/issue`. Nothing here names a workload.
fn goals(model: &Model) -> (Vec<(Invariant, Goal)>, Vec<Invariant>) {
    let publishes = |w: &crate::Workload, topic: &str| {
        w.topics
            .iter()
            .any(|t| t.name == topic && t.access == Access::Publish)
    };
    let subscribers = |topic: &str| -> Vec<String> {
        model
            .workloads
            .iter()
            .filter(|w| {
                w.topics
                    .iter()
                    .any(|t| t.name == topic && t.access == Access::Subscribe)
            })
            .map(|w| w.workload.clone())
            .collect()
    };
    let takes = |w: &crate::Workload, entry: &CauseEntry| {
        w.topics
            .iter()
            .any(|t| t.access == Access::Publish && t.causes.contains(entry))
    };
    let publishers: Vec<&crate::Workload> = model
        .workloads
        .iter()
        .filter(|w| publishes(w, AUTHORITY_UPDATES_TOPIC))
        .collect();
    let mut goals = Vec::new();
    let mut not_applicable = Vec::new();

    let mut hearing = Vec::new();
    for publisher in &publishers {
        if takes(publisher, &CauseEntry::Registered) {
            for to in subscribers(AUTHORITY_UPDATES_TOPIC) {
                hearing.push(Goal::Delivers {
                    workload: publisher.workload.clone(),
                    entry: CauseEntry::Registered,
                    topic: AUTHORITY_UPDATES_TOPIC.to_string(),
                    to,
                });
            }
        }
    }
    for w in model
        .workloads
        .iter()
        .filter(|w| publishes(w, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC))
    {
        hearing.push(Goal::Closes {
            workload: w.workload.clone(),
            entry: CauseEntry::Registered,
            sent: AUTHORITY_SNAPSHOT_REQUESTS_TOPIC.to_string(),
        });
    }
    if hearing.is_empty() {
        not_applicable.push(Invariant::HearingAuthority);
    }
    goals.extend(
        hearing
            .into_iter()
            .map(|g| (Invariant::HearingAuthority, g)),
    );

    let push = CauseEntry::Http(polar_git_server_common::RECEIVE_PACK_ROUTE.to_string());
    let mut correction = Vec::new();
    for taker in model.workloads.iter().filter(|w| takes(w, &push)) {
        for publisher in &publishers {
            let configuration = polar_scheduler_common::configuration_topic(&publisher.workload);
            for to in subscribers(&configuration) {
                correction.push(Goal::Delivers {
                    workload: taker.workload.clone(),
                    entry: push.clone(),
                    topic: configuration.clone(),
                    to,
                });
            }
        }
    }
    if correction.is_empty() {
        not_applicable.push(Invariant::Correction);
    }
    goals.extend(correction.into_iter().map(|g| (Invariant::Correction, g)));
    goals.sort();
    (goals, not_applicable)
}

fn renewal_entry() -> CauseEntry {
    CauseEntry::Http(credentials_agent_common::routes::WORKLOAD_ISSUE.to_string())
}

/// Where each goal's paths start: what its entry makes its workload publish.
fn starts(model: &Model, entry: &CauseEntry, workload: &str) -> Vec<Node> {
    model
        .workload(workload)
        .map(|w| {
            w.topics
                .iter()
                .filter(|t| t.access == Access::Publish && t.causes.contains(entry))
                .map(|t| Node::Out(workload.to_string(), t.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// What the rules derive from `model` with `allowed` (keyed by identity, as [`allowed_under`] gives it): whether
/// each goal holds, with its witness, and what flows.
pub fn evaluate(model: &Model, allowed: &Allowed) -> Evaluation {
    let (goal_list, not_applicable) = goals(model);
    let workload_of: BTreeMap<&str, &str> = model
        .workloads
        .iter()
        .map(|w| (w.identity.as_str(), w.workload.as_str()))
        .collect();
    let mut program = Program::default();
    for w in &model.workloads {
        program.issuer.push((w.workload.clone(), w.issuer.clone()));
        for t in &w.topics {
            match t.access {
                Access::Publish => {
                    program.publishes.push((w.workload.clone(), t.name.clone()));
                    for cause in &t.causes {
                        program
                            .causes
                            .push((w.workload.clone(), cause.clone(), t.name.clone()));
                    }
                }
                Access::Subscribe => {
                    program
                        .subscribes
                        .push((w.workload.clone(), t.name.clone()));
                    for entry in &t.answers {
                        program
                            .answers
                            .push((w.workload.clone(), entry.clone(), t.name.clone()));
                    }
                }
            }
        }
    }
    for (identity, topic, access) in allowed {
        if let Some(workload) = workload_of.get(identity.as_str()) {
            program
                .allowed
                .push((workload.to_string(), topic.clone(), *access));
        }
    }
    let renewal = renewal_entry();
    program.renewal_entry.push((renewal.clone(),));
    let renewal_takers: Vec<String> = model
        .workloads
        .iter()
        .filter(|w| {
            w.topics
                .iter()
                .any(|t| t.access == Access::Publish && t.causes.contains(&renewal))
        })
        .map(|w| w.workload.clone())
        .collect();
    let mut start_nodes = BTreeSet::new();
    for (_, goal) in &goal_list {
        match goal {
            Goal::Delivers {
                workload, entry, ..
            } => start_nodes.extend(starts(model, entry, workload)),
            Goal::Closes { workload, sent, .. } => {
                start_nodes.insert(Node::Out(workload.clone(), sent.clone()));
            }
        }
    }
    for taker in &renewal_takers {
        start_nodes.extend(starts(model, &renewal, taker));
    }
    program.start = start_nodes.iter().map(|n| (n.clone(),)).collect();
    program.run();

    let reach_held: BTreeSet<(Node, Node)> = program.reach_held.iter().cloned().collect();
    let closes_held: BTreeSet<(String, CauseEntry, String)> =
        program.closes_held.iter().cloned().collect();
    let witness_held: BTreeMap<(Node, Node), Vec<Node>> = program
        .witness_held
        .iter()
        .map(|(x, y, path)| ((x.clone(), y.clone()), path.0.1.clone()))
        .collect();
    let witness: BTreeMap<(Node, Node), Vec<Node>> = program
        .witness
        .iter()
        .map(|(x, y, path)| ((x.clone(), y.clone()), path.0.1.clone()))
        .collect();
    let answers_of = |workload: &str, entry: &CauseEntry| -> Vec<String> {
        program
            .answers
            .iter()
            .filter(|(w, e, _)| w == workload && e == entry)
            .map(|(_, _, t)| t.clone())
            .collect()
    };
    // The shortest of the witnesses from any of `from` to any of `to`.
    let shortest = |paths: &BTreeMap<(Node, Node), Vec<Node>>,
                    from: &[Node],
                    to: &[Node]|
     -> Option<Vec<Node>> {
        from.iter()
            .flat_map(|x| {
                to.iter()
                    .filter_map(move |y| paths.get(&(x.clone(), y.clone())))
            })
            .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
            .cloned()
    };

    let goals = goal_list
        .into_iter()
        .map(|(invariant, goal)| {
            let (held, witness) = match &goal {
                Goal::Delivers {
                    workload,
                    entry,
                    topic,
                    to,
                } => {
                    let from = starts(model, entry, workload);
                    let target = [Node::In(to.clone(), topic.clone())];
                    let held = from
                        .iter()
                        .any(|x| reach_held.contains(&(x.clone(), target[0].clone())));
                    (
                        held,
                        if held {
                            shortest(&witness_held, &from, &target)
                        } else {
                            None
                        },
                    )
                }
                Goal::Closes {
                    workload,
                    entry,
                    sent,
                } => {
                    let held =
                        closes_held.contains(&(workload.clone(), entry.clone(), sent.clone()));
                    let from = [Node::Out(workload.clone(), sent.clone())];
                    let target: Vec<Node> = answers_of(workload, entry)
                        .into_iter()
                        .map(|t| Node::In(workload.clone(), t))
                        .collect();
                    (
                        held,
                        if held {
                            shortest(&witness_held, &from, &target)
                        } else {
                            None
                        },
                    )
                }
            };
            GoalOutcome {
                invariant,
                goal,
                held,
                witness,
            }
        })
        .collect();

    let renewal_witness = if program.renewal_closes.is_empty() {
        None
    } else {
        renewal_takers
            .iter()
            .filter_map(|taker| {
                let from = starts(model, &renewal, taker);
                let target: Vec<Node> = answers_of(taker, &renewal)
                    .into_iter()
                    .map(|t| Node::In(taker.clone(), t))
                    .collect();
                shortest(&witness, &from, &target)
            })
            .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
    };

    Evaluation {
        goals,
        not_applicable,
        edges: program.edge.iter().cloned().collect(),
        held: program.held.iter().cloned().collect(),
        lives: program.lives.iter().map(|(w,)| w.clone()).collect(),
        closes: closes_held,
        renewal_witness,
    }
}

/// The first step of `path` that `proposed` no longer has among `edges`, as the request whose refusal took it.
fn forbidden_step(
    model: &Model,
    path: &[Node],
    edges: &BTreeSet<(Node, Node)>,
    proposed_allowed: &Allowed,
) -> Option<Broken> {
    let identity = |workload: &str| {
        model
            .workload(workload)
            .map(|w| w.identity.clone())
            .unwrap_or_default()
    };
    path.windows(2)
        .find(|step| !edges.contains(&(step[0].clone(), step[1].clone())))
        .map(|step| match (&step[0], &step[1]) {
            (Node::Out(a, t), Node::In(b, _)) => {
                let publisher = identity(a);
                if !proposed_allowed.contains(&(publisher.clone(), t.clone(), Access::Publish)) {
                    Broken::Forbidden {
                        identity: publisher,
                        topic: t.clone(),
                        access: Access::Publish,
                    }
                } else {
                    Broken::Forbidden {
                        identity: identity(b),
                        topic: t.clone(),
                        access: Access::Subscribe,
                    }
                }
            }
            _ => Broken::Unexplained,
        })
}

/// Why the goal `path` held under the current pair would not under the proposed one.
fn explain(
    model: &Model,
    path: &[Node],
    current: &Evaluation,
    proposed: &Evaluation,
    proposed_allowed: &Allowed,
) -> Broken {
    let Some(step) = path
        .windows(2)
        .find(|step| !proposed.held.contains(&(step[0].clone(), step[1].clone())))
    else {
        return Broken::Unexplained;
    };
    if proposed.edges.contains(&(step[0].clone(), step[1].clone())) {
        // The edge is there, but an agent at it would not live: renewal is what was lost.
        let workload = [&step[0], &step[1]]
            .into_iter()
            .map(|n| n.workload().to_string())
            .find(|w| !proposed.lives.contains(w))
            .unwrap_or_default();
        let because = current
            .renewal_witness
            .as_deref()
            .and_then(|renewal| forbidden_step(model, renewal, &proposed.edges, proposed_allowed))
            .unwrap_or(Broken::Unexplained);
        return Broken::NotRenewed {
            workload,
            because: Box::new(because),
        };
    }
    forbidden_step(model, path, &proposed.edges, proposed_allowed).unwrap_or(Broken::Unexplained)
}

/// The proposed pair judged against the current one, from what each allows and what the rules derive from it: every
/// goal the current holds and the proposed would not, with why, and what the change does.
pub fn compare(
    model: &Model,
    current: &Evaluation,
    proposed: &Evaluation,
    current_allowed: &Allowed,
    proposed_allowed: &Allowed,
) -> Judgement {
    let losses = current
        .goals
        .iter()
        .zip(&proposed.goals)
        .filter(|(c, p)| c.held && !p.held)
        .map(|(c, _)| {
            let witness = c.witness.clone().unwrap_or_default();
            Loss {
                invariant: c.invariant,
                goal: c.goal.clone(),
                broken: explain(model, &witness, current, proposed, proposed_allowed),
                witness,
            }
        })
        .collect();
    Judgement {
        losses,
        effect: Effect {
            gained: proposed_allowed
                .difference(current_allowed)
                .cloned()
                .collect(),
            lost: current_allowed
                .difference(proposed_allowed)
                .cloned()
                .collect(),
            began_closing: proposed
                .closes
                .difference(&current.closes)
                .cloned()
                .collect(),
            stopped_closing: current
                .closes
                .difference(&proposed.closes)
                .cloned()
                .collect(),
        },
        unknown_keys: Vec::new(),
    }
}

/// `proposed` judged against `current`, over `model` and `persons` (docs/architecture/authority-model.md,
/// "Acceptance as a comparison"). A policy in either pair that would not load is an error.
pub fn judge(
    model: &Model,
    current: &Pair,
    proposed: &Pair,
    persons: &[Person],
) -> Result<Judgement, String> {
    let current_allowed = allowed_under(model, current, persons)?;
    let proposed_allowed = allowed_under(model, proposed, persons)?;
    let mut judgement = compare(
        model,
        &evaluate(model, &current_allowed),
        &evaluate(model, &proposed_allowed),
        &current_allowed,
        &proposed_allowed,
    );
    let known: BTreeSet<&str> = model
        .identities()
        .into_iter()
        .chain(persons.iter().map(|p| p.identity.as_str()))
        .collect();
    judgement.unknown_keys = proposed
        .memberships
        .keys()
        .filter(|k| !known.contains(k.as_str()))
        .cloned()
        .collect();
    Ok(judgement)
}
