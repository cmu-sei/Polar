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

//! The rules over the model, as one Datalog program in `ascent` (docs/architecture/authority-model.md, "Rules over
//! the model"; `ceri1989datalog` for the language). Each evaluation builds a fresh program from the whole set of facts
//! and runs it once (ascent's `run` is one-shot: its issue #30), and the witnesses are read after the run and never
//! joined on their lattice column (its issue #22).

// What ascent generates for a relation with no columns (`renewal_closes`) binds a tuple it never reads, and its lattice
// relations' types are long: both are the macro's, not this module's.
#![allow(unused_variables, clippy::type_complexity)]

use ascent::{Dual, ascent};
use polar_agent_builder::topics::{Access, CauseEntry};

/// A point a message passes: an agent publishing on a topic, or an agent hearing one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Node {
    /// `(workload, topic)`: the workload publishes on the topic.
    Out(String, String),
    /// `(workload, topic)`: the workload hears the topic.
    In(String, String),
}

impl Node {
    /// The workload at this point.
    pub fn workload(&self) -> &str {
        match self {
            Node::Out(w, _) | Node::In(w, _) => w,
        }
    }

    /// The topic at this point.
    pub fn topic(&self) -> &str {
        match self {
            Node::Out(_, t) | Node::In(_, t) => t,
        }
    }
}

/// `path` with `next` after it.
fn extended(path: &[Node], next: &Node) -> Vec<Node> {
    let mut longer = path.to_vec();
    longer.push(next.clone());
    longer
}

ascent! {
    /// The program. Its inputs are the relations before `edge`; everything after is derived.
    pub(crate) struct Program;

    // -- the facts ---------------------------------------------------------------------------------------------------
    /// `(workload, topic)` it publishes on, and hears.
    relation publishes(String, String);
    relation subscribes(String, String);
    /// `(workload, topic, access)` the pair judged allows: Cedar first, then the floor.
    relation allowed(String, String, Access);
    /// `(workload, cause, topic)`: the workload publishes on the topic because of the cause.
    relation causes(String, CauseEntry, String);
    /// `(workload, entry, topic)`: the subscription to the topic answers the entry.
    relation answers(String, CauseEntry, String);
    /// `(workload, issuer)`: `cert-issuer` or `credentials-agent`.
    relation issuer(String, String);
    /// The entry renewal's round trip starts at (the observer's `/workload/issue`).
    relation renewal_entry(CauseEntry);
    /// Where a witness is wanted from.
    relation start(Node);

    // -- message flow -----------------------------------------------------------------------------------------------
    /// A message on a topic goes from one agent to another when both may use it, and inside an agent from what
    /// arrives to what that causes it to publish.
    relation edge(Node, Node);
    edge(Node::Out(a.clone(), t.clone()), Node::In(b.clone(), t.clone())) <--
        publishes(a, t), subscribes(b, t), allowed(a, t, Access::Publish), allowed(b, t, Access::Subscribe);
    edge(Node::In(a.clone(), i.clone()), Node::Out(a.clone(), o.clone())) <--
        causes(a, c, o), if let CauseEntry::Topic(i) = c;
    relation reach(Node, Node);
    reach(x.clone(), y.clone()) <-- edge(x, y);
    reach(x.clone(), z.clone()) <-- reach(x, y), edge(y, z);

    // -- round trips, and renewal -----------------------------------------------------------------------------------
    /// `(workload, entry, sent, answer)`: the entry makes the workload publish on `sent`, and `answer` answers it.
    relation entry(String, CauseEntry, String, String);
    entry(a.clone(), e.clone(), s.clone(), r.clone()) <-- causes(a, e, s), answers(a, e, r);
    /// `(workload, entry, sent)`: the round trip comes back, over any edge.
    relation closes(String, CauseEntry, String);
    closes(a.clone(), e.clone(), s.clone()) <--
        entry(a, e, s, r), reach(Node::Out(a.clone(), s.clone()), Node::In(a.clone(), r.clone()));
    relation renewal_closes();
    renewal_closes() <-- renewal_entry(e), closes(_, e, _);
    /// An agent stays up only while its certificates can be renewed: a bootstrap service renews through cert-issuer,
    /// outside policy; any other, through the credentials agent's renewal round trip ("Decisions": renewal is a
    /// dependency, not an invariant).
    relation lives(String);
    lives(w.clone()) <-- issuer(w, i), if i == "cert-issuer";
    lives(w.clone()) <-- issuer(w, i), if i == "credentials-agent", renewal_closes();

    // -- what the invariants are judged over -------------------------------------------------------------------------
    relation node_of(Node, String);
    node_of(x.clone(), x.workload().to_string()) <-- edge(x, _);
    node_of(y.clone(), y.workload().to_string()) <-- edge(_, y);
    /// An edge between agents that both live. Not recursive through `lives`: renewal's own round trip is over `edge`.
    relation held(Node, Node);
    held(x.clone(), y.clone()) <-- edge(x, y), node_of(x, a), lives(a), node_of(y, b), lives(b);
    relation reach_held(Node, Node);
    reach_held(x.clone(), y.clone()) <-- held(x, y);
    reach_held(x.clone(), z.clone()) <-- reach_held(x, y), held(y, z);
    relation closes_held(String, CauseEntry, String);
    closes_held(a.clone(), e.clone(), s.clone()) <--
        entry(a, e, s, r), reach_held(Node::Out(a.clone(), s.clone()), Node::In(a.clone(), r.clone()));

    // -- witnesses: the shortest path from each start, for a refusal's reason ----------------------------------------
    lattice witness(Node, Node, Dual<(usize, Vec<Node>)>);
    witness(x.clone(), y.clone(), Dual((1, vec![x.clone(), y.clone()]))) <-- start(x), edge(x, y);
    witness(x.clone(), z.clone(), Dual((n + 1, extended(path, z)))) <-- witness(x, y, ?Dual((n, path))), edge(y, z);
    lattice witness_held(Node, Node, Dual<(usize, Vec<Node>)>);
    witness_held(x.clone(), y.clone(), Dual((1, vec![x.clone(), y.clone()]))) <-- start(x), held(x, y);
    witness_held(x.clone(), z.clone(), Dual((n + 1, extended(path, z)))) <--
        witness_held(x, y, ?Dual((n, path))), held(y, z);
}
