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

//! What the controller knows and what it announces: the signed `repositories` section, the signed `admission`
//! rules, and the proposals it holds. The actor around it does the input and output (the broker, the clock, the
//! name lookups); everything it decides is here, so the sequences that matter (nothing announced before
//! anything is known, nothing stopped before the proposers have been heard, a proposal accepted, then expired)
//! can be tested without a cluster.

use crate::admission::{Decision, Rules};
use crate::proposals::{self, Key, Received, Table};
use git_agent_common::RepoObservationConfig;
use git_agent_common::proposal::{Evidence, ProposalDecision};

#[derive(Debug, Clone)]
pub struct Control {
    /// When this controller started, in milliseconds since the epoch.
    started: i64,
    /// The latest accepted version of the `repositories` section; `None` until one has been received.
    explicit: Option<Vec<RepoObservationConfig>>,
    /// The latest accepted version of the `admission` section; `None` until one has been received, and then
    /// nothing is accepted.
    rules: Option<Rules>,
    table: Table,
}

impl Control {
    pub fn new(now: i64) -> Self {
        Self {
            started: now,
            explicit: None,
            rules: None,
            table: Table::default(),
        }
    }

    /// A new version of the `repositories` section.
    pub fn set_explicit(&mut self, configs: Vec<RepoObservationConfig>) {
        self.explicit = Some(configs);
    }

    /// A new version of the `admission` section.
    pub fn set_rules(&mut self, rules: Rules) {
        self.rules = Some(rules);
    }

    pub fn has_rules(&self) -> bool {
        self.rules.is_some()
    }

    /// A proposal arrived from `proposer` (as the broker named them). If it is refused outright, and so not held,
    /// the decision to publish.
    pub fn receive(
        &mut self,
        proposer: Option<&str>,
        url: &str,
        evidence: &Evidence,
        now: i64,
    ) -> Option<ProposalDecision> {
        match self
            .table
            .receive(proposer, url, evidence, self.rules.as_ref(), now)
        {
            Received::Held(_) => None,
            Received::Refused(decision) => Some(*decision),
        }
    }

    /// Drops the claims nobody has announced within `ttl_ms`.
    pub fn expire(&mut self, now: i64, ttl_ms: i64) -> Vec<ProposalDecision> {
        self.table.expire(now, ttl_ms)
    }

    /// What to judge, and by what: a copy, so the judging (which looks names up) can go on elsewhere while the
    /// controller goes on receiving.
    pub fn to_judge(&self) -> (Table, Option<Rules>) {
        (self.table.clone(), self.rules.clone())
    }

    /// What the judging found; the decisions that changed.
    pub fn judged(&mut self, results: Vec<(Key, Decision)>, now: i64) -> Vec<ProposalDecision> {
        self.table.apply(results, now)
    }

    /// The set to announce and whether it is complete, or `None` if there is nothing yet that the controller
    /// may say: neither section received, and no proposal accepted. A set announced before the controller
    /// knows anything would, if complete, stop everything the observer is doing.
    ///
    /// It is complete only once `warmup_ms` have passed since the controller started: until then the signed
    /// sections may not have been replayed to it and the proposers have not announced again, and a set
    /// missing what they would add would make the observer stop it and start it again a minute later.
    pub fn to_announce(
        &self,
        now: i64,
        warmup_ms: i64,
    ) -> Option<(Vec<RepoObservationConfig>, bool)> {
        // A proposal is only ever accepted under rules, so the rules being absent means nothing is proposed.
        if self.explicit.is_none() && self.rules.is_none() {
            return None;
        }
        let set = proposals::merge(
            self.explicit.as_deref().unwrap_or_default(),
            self.table.accepted_configs(),
        );
        Some((set, now - self.started >= warmup_ms))
    }

    pub fn claims(&self) -> usize {
        self.table.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{Binding, Limits, Resolution};
    use git_agent_common::proposal::ProposalState;
    use git_agent_common::remote::canonical_repository;
    use polar::graph::nodes::git::RepoId;
    use std::collections::{BTreeMap, HashSet};
    use std::net::{IpAddr, Ipv4Addr};

    const JIRA: &str = "jira-consumer.polar.serviceaccount.cluster.local";
    const T0: i64 = 1_000_000_000;
    const WARMUP: i64 = 600_000;
    const TTL: i64 = 86_400_000;

    fn rules() -> Rules {
        Rules {
            hosts: vec!["github.com".into()],
            organisations: BTreeMap::new(),
            proposers: vec![JIRA.into()],
            require_approval: false,
            approved: HashSet::new(),
            denied: HashSet::new(),
            credentials: Vec::<Binding>::new(),
            anonymous: vec!["github.com".into()],
            limits: Limits {
                max_entries: 500,
                per_proposer_per_hour: 20,
            },
        }
    }

    fn id(url: &str) -> RepoId {
        canonical_repository(url).unwrap().repo_id()
    }

    fn explicit(url: &str) -> RepoObservationConfig {
        RepoObservationConfig::new(
            id(url),
            url.to_string(),
            vec!["origin".into()],
            None,
            vec![],
        )
    }

    fn public(_: &str) -> Resolution {
        Ok(vec![IpAddr::V4(Ipv4Addr::new(140, 82, 112, 3))])
    }

    fn evidence() -> Evidence {
        Evidence {
            source: "jira".into(),
            reference: Some("ABC-1".into()),
        }
    }

    /// Judges and applies, as the actor does.
    fn judge(c: &mut Control, now: i64) -> Vec<ProposalDecision> {
        let (table, rules) = c.to_judge();
        let results = table.evaluate(rules.as_ref(), &public, now);
        c.judged(results, now)
    }

    fn urls(set: &[RepoObservationConfig]) -> Vec<String> {
        set.iter().map(|c| c.repo_url.clone()).collect()
    }

    #[test]
    fn a_controller_that_knows_nothing_announces_nothing() {
        let c = Control::new(T0);
        assert!(c.to_announce(T0, WARMUP).is_none());
        assert!(
            c.to_announce(T0 + 10 * WARMUP, WARMUP).is_none(),
            "not even late"
        );
    }

    #[test]
    fn once_the_signed_section_is_known_it_is_announced_incomplete_until_the_warm_up_is_over() {
        let mut c = Control::new(T0);
        c.set_explicit(vec![explicit("https://github.com/o/a.git")]);
        let (set, complete) = c.to_announce(T0 + 1, WARMUP).unwrap();
        assert_eq!(urls(&set), ["https://github.com/o/a.git"]);
        assert!(!complete);
        assert!(!c.to_announce(T0 + WARMUP - 1, WARMUP).unwrap().1);
        assert!(c.to_announce(T0 + WARMUP, WARMUP).unwrap().1);
    }

    #[test]
    fn a_signed_section_that_names_nothing_is_a_set_that_names_nothing_once_it_is_complete() {
        let mut c = Control::new(T0);
        c.set_explicit(vec![]);
        let (set, complete) = c.to_announce(T0 + WARMUP, WARMUP).unwrap();
        assert!(
            set.is_empty() && complete,
            "this is how a last repository is taken away"
        );
    }

    #[test]
    fn rules_alone_make_the_controller_know_enough_to_announce() {
        let mut c = Control::new(T0);
        c.set_rules(rules());
        assert!(c.has_rules());
        let (set, _) = c.to_announce(T0, WARMUP).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn a_proposal_held_before_any_rules_is_accepted_when_they_arrive_and_joins_the_set() {
        let mut c = Control::new(T0);
        assert!(
            c.receive(Some(JIRA), "https://github.com/o/p", &evidence(), T0 + 1)
                .is_none()
        );
        let waiting = judge(&mut c, T0 + 2);
        assert_eq!(waiting[0].state, ProposalState::Pending);
        assert!(
            c.to_announce(T0 + 3, WARMUP).is_none(),
            "a pending proposal is nothing to announce"
        );

        c.set_rules(rules());
        let decided = judge(&mut c, T0 + 4);
        assert_eq!(decided[0].state, ProposalState::Accepted);
        let (set, complete) = c.to_announce(T0 + 5, WARMUP).unwrap();
        assert_eq!(urls(&set), ["https://github.com/o/p"]);
        assert!(!complete);
    }

    #[test]
    fn the_signed_section_and_the_accepted_proposals_are_one_set_and_the_signed_entry_wins() {
        let mut c = Control::new(T0);
        c.set_rules(rules());
        let mut signed = explicit("https://github.com/o/a.git");
        signed.frequency = 3600;
        c.set_explicit(vec![signed]);
        c.receive(Some(JIRA), "https://github.com/o/a", &evidence(), T0);
        c.receive(Some(JIRA), "https://github.com/o/b", &evidence(), T0 + 1);
        judge(&mut c, T0 + 2);
        let (set, _) = c.to_announce(T0 + 3, WARMUP).unwrap();
        assert_eq!(
            urls(&set),
            ["https://github.com/o/a.git", "https://github.com/o/b"]
        );
        assert_eq!(set[0].frequency, 3600);
    }

    #[test]
    fn a_proposal_its_proposer_stops_announcing_leaves_the_set() {
        let mut c = Control::new(T0);
        c.set_rules(rules());
        c.set_explicit(vec![]);
        c.receive(Some(JIRA), "https://github.com/o/p", &evidence(), T0);
        judge(&mut c, T0 + 1);
        assert_eq!(c.to_announce(T0 + 2, WARMUP).unwrap().0.len(), 1);
        let gone = c.expire(T0 + TTL + 1, TTL);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].state, ProposalState::Expired);
        assert!(c.to_announce(T0 + TTL + 2, WARMUP).unwrap().0.is_empty());
        assert_eq!(c.claims(), 0);
    }

    #[test]
    fn a_proposal_that_is_refused_outright_is_returned_to_be_published_and_not_held() {
        let mut c = Control::new(T0);
        c.set_rules(rules());
        let refused = c
            .receive(Some(JIRA), "ext::sh -c id", &evidence(), T0)
            .unwrap();
        assert_eq!(refused.state, ProposalState::Rejected);
        let stranger = c
            .receive(Some("stranger"), "https://github.com/o/r", &evidence(), T0)
            .unwrap();
        assert_eq!(stranger.rule.as_deref(), Some("proposer"));
        assert_eq!(c.claims(), 0);
    }

    #[test]
    fn denying_a_repository_takes_it_out_of_the_set_on_the_next_judging() {
        let mut c = Control::new(T0);
        c.set_rules(rules());
        c.receive(Some(JIRA), "https://github.com/o/p", &evidence(), T0);
        judge(&mut c, T0 + 1);
        assert_eq!(c.to_announce(T0 + 2, WARMUP).unwrap().0.len(), 1);
        let mut denying = rules();
        denying.denied.insert(id("https://github.com/o/p"));
        c.set_rules(denying);
        let changed = judge(&mut c, T0 + 3);
        assert_eq!(changed[0].state, ProposalState::Rejected);
        assert!(c.to_announce(T0 + 4, WARMUP).unwrap().0.is_empty());
    }

    #[test]
    fn what_is_judged_is_a_copy_so_receiving_can_go_on_meanwhile() {
        let mut c = Control::new(T0);
        c.set_rules(rules());
        c.receive(Some(JIRA), "https://github.com/o/a", &evidence(), T0);
        let (table, rules) = c.to_judge();
        c.receive(Some(JIRA), "https://github.com/o/b", &evidence(), T0 + 1);
        let results = table.evaluate(rules.as_ref(), &public, T0 + 2);
        assert_eq!(results.len(), 1, "the copy does not have b");
        let changed = c.judged(results, T0 + 2);
        assert_eq!(changed.len(), 1);
        assert_eq!(c.claims(), 2, "b is still held, and is judged next time");
    }
}
