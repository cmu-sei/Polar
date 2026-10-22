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

//! The authority model (docs/architecture/authority-model.md): the deployment as facts, the rules over them, and the
//! judging of a policy or groups document before the credentials agent accepts it.
//!
//! - [`facts`]: the model document the render gives the credentials agent (every workload by its one identity, with
//!   its issuer, its certificate's groups and its topics with their causes), and the pair a document is judged as:
//!   a policy and a groups document, as they stand or as they would.
//! - [`rules`]: the Datalog over the facts, in `ascent` (`sahebolamri2022ascent`, docs/reference): message flow
//!   between agents both allowed to use a topic, flow inside an agent from its causes, reachability, round trips,
//!   which agents live (renewal), and the shortest witness of each path.
//! - [`judge`]: what is allowed under a pair (Cedar first, then the floor, by the topic gate's own rule), the
//!   invariants' goals and whether each holds, and the comparison of two pairs: what would be lost, and why.
//!
//! No actor and no Cassini: the credentials agent's consumer uses it where it accepts a section.

pub mod facts;
pub mod judge;
pub mod rules;

pub use facts::{Model, Pair, Person, Workload};
pub use judge::{
    Allowed, Broken, Effect, Evaluation, Goal, GoalOutcome, Invariant, Judgement, Loss,
    allowed_under, compare, evaluate, judge,
};
pub use rules::Node;
