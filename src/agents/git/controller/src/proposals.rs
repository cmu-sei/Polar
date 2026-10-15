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

//! What the controller holds about the repositories agents have proposed: a short-term cache, in memory, that is
//! explicitly not expected to survive (docs/architecture/proposals.md, "Persistence, replay, removal"). The
//! proposers announce again and a restarted controller has it back; an entry whose proposer stops announcing
//! it expires.
//!
//! Pure: the time is passed in and so is the resolver, so every rule here can be checked without a clock or a
//! network. The decision itself is `admission::decide`.

use crate::admission::{self, Decision, Proposal, Resolution, Rules, Standing};
use git_agent_common::RepoObservationConfig;
use git_agent_common::proposal::{Evidence, ProposalDecision, ProposalState};
use git_agent_common::remote::canonical_repository;
use polar::graph::nodes::git::RepoId;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// How much of a proposer's text is shown when it could not be read as a URL.
const SHOWN_CHARS: usize = 200;

/// A proposer's text made fit to show: one line, no control characters, cut short.
fn shown(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(SHOWN_CHARS)
        .collect()
}

/// What a claim's decision is, as a person reads it.
fn report(claim: &Claim, decision: &Decision) -> ProposalDecision {
    let (state, rule, reason) = match decision {
        Decision::Accepted(a) => (
            ProposalState::Accepted,
            None,
            match &a.credential {
                Some(name) => format!("accepted, with the credential `{name}`"),
                None => "accepted, and its host needs no credential".to_string(),
            },
        ),
        Decision::Rejected { rule, reason } => (
            ProposalState::Rejected,
            Some(rule.as_str().to_string()),
            reason.clone(),
        ),
        Decision::Pending { waiting, reason } => (
            ProposalState::Pending,
            Some(waiting.as_str().to_string()),
            reason.clone(),
        ),
    };
    ProposalDecision {
        repo_id: Some(claim.repo_id.clone()),
        url: claim.url.clone(),
        proposer: Some(claim.proposer.clone()),
        state,
        rule,
        reason,
        evidence: claim.evidence.clone(),
        first_seen: claim.first_seen,
        last_seen: claim.last_seen,
        announcements: claim.announcements,
        decided_at: claim.decided_at,
        held: true,
    }
}

/// Most claims the table holds in all, and from one proposer. A proposal is untrusted text from an agent that
/// may be compromised, and the table is memory; past these a new claim is refused (an existing one is still
/// refreshed).
pub const MAX_CLAIMS: usize = 5000;
pub const MAX_CLAIMS_PER_PROPOSER: usize = 500;
/// Most pieces of evidence kept for one claim: the newest.
pub const MAX_EVIDENCE_KEPT: usize = 5;
pub const HOUR_MS: i64 = 3_600_000;

/// One proposer's proposal of one repository.
pub type Key = (RepoId, String);

#[derive(Debug, Clone)]
pub struct Claim {
    pub repo_id: RepoId,
    /// The canonical URL.
    pub url: String,
    /// The identity the broker named.
    pub proposer: String,
    pub evidence: Vec<Evidence>,
    pub first_seen: i64,
    pub last_seen: i64,
    pub announcements: u32,
    /// `None` until the claim has been judged.
    pub decision: Option<Decision>,
    pub decided_at: i64,
}

/// What became of an announcement.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// It is held, under this key; it needs judging.
    Held(Key),
    /// It is not held, and this is what to publish about it: it could not be tied to a repository and a proposer,
    /// or the table has no room, or the rules do not let that proposer propose.
    Refused(Box<ProposalDecision>),
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    claims: HashMap<Key, Claim>,
}

impl Table {
    pub fn len(&self) -> usize {
        self.claims.len()
    }

    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    pub fn claim(&self, key: &Key) -> Option<&Claim> {
        self.claims.get(key)
    }

    /// An announcement from `proposer` (as the broker named them) of the repository at `url`.
    pub fn receive(
        &mut self,
        proposer: Option<&str>,
        url: &str,
        evidence: &Evidence,
        rules: Option<&Rules>,
        now: i64,
    ) -> Received {
        let evidence = evidence.tidy();
        let refuse = |repo_id: Option<RepoId>,
                      url: String,
                      proposer: Option<&str>,
                      rule: &str,
                      reason: String| {
            Received::Refused(Box::new(ProposalDecision {
                repo_id,
                url,
                proposer: proposer.map(str::to_string),
                state: ProposalState::Rejected,
                rule: Some(rule.to_string()),
                reason,
                evidence: vec![evidence.clone()],
                first_seen: now,
                last_seen: now,
                announcements: 1,
                decided_at: now,
                held: false,
            }))
        };

        let Some(proposer) = proposer else {
            return refuse(
                None,
                shown(url),
                None,
                "proposer",
                "the broker did not say who published the proposal".to_string(),
            );
        };
        let repository = match canonical_repository(url) {
            Ok(r) => r,
            Err(why) => return refuse(None, shown(url), Some(proposer), "url", why),
        };
        let repo_id = repository.repo_id();
        // Nothing is held for an identity the rules do not let propose: it costs the controller no memory.
        if let Some(rules) = rules
            && !rules.proposers.iter().any(|p| p == proposer)
        {
            return refuse(
                Some(repo_id),
                repository.url,
                Some(proposer),
                "proposer",
                format!("`{proposer}` is not one of the identities that may propose"),
            );
        }

        let key = (repo_id.clone(), proposer.to_string());
        if let Some(claim) = self.claims.get_mut(&key) {
            claim.last_seen = now;
            claim.announcements = claim.announcements.saturating_add(1);
            claim.evidence.retain(|e| *e != evidence);
            claim.evidence.push(evidence);
            let excess = claim.evidence.len().saturating_sub(MAX_EVIDENCE_KEPT);
            claim.evidence.drain(..excess);
            return Received::Held(key);
        }

        let from_this_proposer = self.claims.keys().filter(|(_, p)| p == proposer).count();
        if self.claims.len() >= MAX_CLAIMS || from_this_proposer >= MAX_CLAIMS_PER_PROPOSER {
            return refuse(
                Some(repo_id),
                repository.url,
                Some(proposer),
                "limits",
                "the controller is holding as many proposals as it will".to_string(),
            );
        }
        self.claims.insert(
            key.clone(),
            Claim {
                repo_id,
                url: repository.url,
                proposer: proposer.to_string(),
                evidence: vec![evidence],
                first_seen: now,
                last_seen: now,
                announcements: 1,
                decision: None,
                decided_at: now,
            },
        );
        Received::Held(key)
    }

    /// Drops the claims their proposers have not announced within `ttl_ms`, and says what to publish about each.
    pub fn expire(&mut self, now: i64, ttl_ms: i64) -> Vec<ProposalDecision> {
        let stale: Vec<Key> = self
            .claims
            .iter()
            .filter(|(_, c)| now - c.last_seen > ttl_ms)
            .map(|(k, _)| k.clone())
            .collect();
        let mut gone: Vec<ProposalDecision> = stale
            .iter()
            .filter_map(|k| self.claims.remove(k))
            .map(|claim| ProposalDecision {
                state: ProposalState::Expired,
                rule: None,
                reason: "its proposer has not announced it within the time limit".to_string(),
                decided_at: now,
                ..report(
                    &claim,
                    &Decision::Pending {
                        waiting: admission::Waiting::Approval,
                        reason: String::new(),
                    },
                )
            })
            .collect();
        gone.sort_by(|a, b| (&a.url, &a.proposer).cmp(&(&b.url, &b.proposer)));
        gone
    }

    /// Judges every claim against `rules`, in the order they were first seen so that the answer does not depend
    /// on the order they arrived in this run. Works on what the table holds now and changes nothing, so it can
    /// be run on a copy while the table goes on receiving.
    pub fn evaluate(
        &self,
        rules: Option<&Rules>,
        resolve: &dyn Fn(&str) -> Resolution,
        now: i64,
    ) -> Vec<(Key, Decision)> {
        let mut order: Vec<&Claim> = self.claims.values().collect();
        order.sort_by_key(|c| (c.first_seen, c.repo_id.to_string(), c.proposer.clone()));

        // A name is looked up once however many claims are on it, and only if a claim gets as far as the guard.
        let cache: RefCell<HashMap<String, Resolution>> = RefCell::new(HashMap::new());
        let resolver = |host: &str| -> Resolution {
            cache
                .borrow_mut()
                .entry(host.to_string())
                .or_insert_with(|| resolve(host))
                .clone()
        };

        let mut accepted: HashSet<RepoId> = HashSet::new();
        let mut earlier_this_hour: HashMap<&str, usize> = HashMap::new();
        let mut results = Vec::with_capacity(order.len());
        for claim in order {
            let standing = Standing {
                accepted_entries: accepted.len() - usize::from(accepted.contains(&claim.repo_id)),
                proposed_this_hour: earlier_this_hour
                    .get(claim.proposer.as_str())
                    .copied()
                    .unwrap_or(0),
            };
            let decision = admission::decide(
                &Proposal {
                    kind: admission::KIND,
                    url: &claim.url,
                    proposer: Some(&claim.proposer),
                },
                rules,
                &standing,
                &resolver,
            );
            if matches!(decision, Decision::Accepted(_)) {
                accepted.insert(claim.repo_id.clone());
            }
            // The flood limit is a window on when claims were first seen, so an old, established claim is
            // never limited and a flood clears as it ages out.
            if now - claim.first_seen < HOUR_MS {
                *earlier_this_hour
                    .entry(claim.proposer.as_str())
                    .or_default() += 1;
            }
            results.push(((claim.repo_id.clone(), claim.proposer.clone()), decision));
        }
        results
    }

    /// Records what `evaluate` found, for the claims still held, and says what to publish about each whose
    /// decision is not what it was.
    pub fn apply(&mut self, results: Vec<(Key, Decision)>, now: i64) -> Vec<ProposalDecision> {
        let mut changed = Vec::new();
        for (key, decision) in results {
            let Some(claim) = self.claims.get_mut(&key) else {
                continue; // it expired while it was being judged
            };
            if claim.decision.as_ref() == Some(&decision) {
                continue;
            }
            claim.decided_at = now;
            let said = report(claim, &decision);
            claim.decision = Some(decision);
            changed.push(said);
        }
        changed.sort_by(|a, b| (&a.url, &a.proposer).cmp(&(&b.url, &b.proposer)));
        changed
    }

    /// The repositories to add to the set: one per accepted repository, however many proposers named it, with
    /// the defaults and the credential the rules bind to its host. In a stable order.
    pub fn accepted_configs(&self) -> Vec<RepoObservationConfig> {
        let mut accepted: Vec<(&Claim, Option<&String>)> = self
            .claims
            .values()
            .filter_map(|c| match &c.decision {
                Some(Decision::Accepted(a)) => Some((c, a.credential.as_ref())),
                _ => None,
            })
            .collect();
        // The first claim of a repository, by when it was first seen, gives the URL it is observed by.
        accepted.sort_by_key(|(c, _)| (c.first_seen, c.proposer.clone()));
        let mut seen: HashSet<&RepoId> = HashSet::new();
        let mut configs: Vec<RepoObservationConfig> = Vec::new();
        for (claim, credential) in accepted {
            if !seen.insert(&claim.repo_id) {
                continue;
            }
            let mut config = RepoObservationConfig::new(
                claim.repo_id.clone(),
                claim.url.clone(),
                vec![crate::desired::REMOTE.to_string()],
                None,
                Vec::new(),
            );
            config.frequency = crate::desired::DEFAULT_FREQUENCY_SECS;
            config.max_depth = crate::desired::DEFAULT_MAX_DEPTH as usize;
            config.credential = credential.cloned();
            configs.push(config);
        }
        configs.sort_by_key(|c| c.repo_id.to_string());
        configs
    }
}

/// The set to announce: the signed `repositories` section, and the accepted proposals it does not already name.
/// A repository in both is observed as the signed section says.
pub fn merge(
    explicit: &[RepoObservationConfig],
    proposed: Vec<RepoObservationConfig>,
) -> Vec<RepoObservationConfig> {
    let named: HashSet<&RepoId> = explicit.iter().map(|c| &c.repo_id).collect();
    let mut set = explicit.to_vec();
    set.extend(proposed.into_iter().filter(|c| !named.contains(&c.repo_id)));
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{Binding, Limits, Rule, Waiting};
    use git_agent_common::remote::canonical_repository;
    use std::collections::{BTreeMap, HashSet};
    use std::net::{IpAddr, Ipv4Addr};

    const JIRA: &str = "jira-consumer.polar.serviceaccount.cluster.local";
    const GITLAB: &str = "gitlab-consumer.polar.serviceaccount.cluster.local";
    const T0: i64 = 1_000_000_000;

    fn evidence(reference: &str) -> Evidence {
        Evidence {
            source: "jira".into(),
            reference: Some(reference.into()),
        }
    }

    fn rules() -> Rules {
        Rules {
            hosts: vec!["github.com".into(), "gitlab.example.org".into()],
            organisations: BTreeMap::new(),
            proposers: vec![JIRA.into(), GITLAB.into()],
            require_approval: false,
            approved: HashSet::new(),
            denied: HashSet::new(),
            credentials: vec![Binding {
                host: "gitlab.example.org".into(),
                name: "gitlab-sei".into(),
            }],
            anonymous: vec!["github.com".into()],
            limits: Limits {
                max_entries: 500,
                per_proposer_per_hour: 20,
            },
        }
    }

    fn public(_: &str) -> Resolution {
        Ok(vec![IpAddr::V4(Ipv4Addr::new(140, 82, 112, 3))])
    }

    fn id(url: &str) -> RepoId {
        canonical_repository(url).unwrap().repo_id()
    }

    fn key(url: &str, proposer: &str) -> Key {
        (id(url), proposer.to_string())
    }

    fn held(table: &mut Table, proposer: &str, url: &str, now: i64) -> Key {
        match table.receive(Some(proposer), url, &evidence("X-1"), Some(&rules()), now) {
            Received::Held(k) => k,
            other => panic!("{url}: {other:?}"),
        }
    }

    /// Receives, judges and applies, as the controller does.
    fn settle(table: &mut Table, rules: Option<&Rules>, now: i64) -> Vec<ProposalDecision> {
        let results = table.evaluate(rules, &public, now);
        table.apply(results, now)
    }

    fn decision_of<'a>(table: &'a Table, k: &Key) -> &'a Decision {
        table.claim(k).unwrap().decision.as_ref().unwrap()
    }

    // --- receiving

    #[test]
    fn an_announcement_is_held_under_the_repository_and_the_proposer_with_the_canonical_url() {
        let mut t = Table::default();
        let k = held(&mut t, JIRA, "HTTPS://GitHub.com:443/Org/Repo.git#x", T0);
        assert_eq!(k, key("https://github.com/Org/Repo", JIRA));
        let c = t.claim(&k).unwrap();
        assert_eq!(c.url, "https://github.com/Org/Repo.git");
        assert_eq!((c.first_seen, c.last_seen, c.announcements), (T0, T0, 1));
        assert!(c.decision.is_none());
    }

    #[test]
    fn announcing_again_refreshes_the_claim_and_does_not_make_another() {
        let mut t = Table::default();
        let k = held(&mut t, JIRA, "https://github.com/o/r", T0);
        t.receive(
            Some(JIRA),
            "https://github.com/o/r.git",
            &evidence("X-2"),
            Some(&rules()),
            T0 + 5000,
        );
        assert_eq!(t.len(), 1);
        let c = t.claim(&k).unwrap();
        assert_eq!(
            (c.first_seen, c.last_seen, c.announcements),
            (T0, T0 + 5000, 2)
        );
        assert_eq!(c.evidence.len(), 2);
    }

    #[test]
    fn the_same_evidence_is_not_kept_twice_and_only_the_newest_few_are_kept() {
        let mut t = Table::default();
        let k = held(&mut t, JIRA, "https://github.com/o/r", T0);
        for n in 0..3 {
            t.receive(
                Some(JIRA),
                "https://github.com/o/r",
                &evidence("X-1"),
                Some(&rules()),
                T0 + n,
            );
        }
        assert_eq!(t.claim(&k).unwrap().evidence.len(), 1);
        for n in 0..20 {
            t.receive(
                Some(JIRA),
                "https://github.com/o/r",
                &evidence(&format!("N-{n}")),
                Some(&rules()),
                T0 + 100 + n,
            );
        }
        let kept = &t.claim(&k).unwrap().evidence;
        assert_eq!(kept.len(), MAX_EVIDENCE_KEPT);
        assert_eq!(kept.last().unwrap().reference.as_deref(), Some("N-19"));
        assert!(!kept.iter().any(|e| e.reference.as_deref() == Some("X-1")));
    }

    #[test]
    fn evidence_is_tidied_before_it_is_kept() {
        let mut t = Table::default();
        let k = match t.receive(
            Some(JIRA),
            "https://github.com/o/r",
            &Evidence {
                source: "ji\nra".into(),
                reference: Some("a\u{7}b".into()),
            },
            Some(&rules()),
            T0,
        ) {
            Received::Held(k) => k,
            other => panic!("{other:?}"),
        };
        assert_eq!(t.claim(&k).unwrap().evidence[0].source, "ji ra");
        assert_eq!(
            t.claim(&k).unwrap().evidence[0].reference.as_deref(),
            Some("ab")
        );
    }

    #[test]
    fn two_proposers_of_one_repository_are_two_claims() {
        let mut t = Table::default();
        held(&mut t, JIRA, "https://github.com/o/r", T0);
        held(&mut t, GITLAB, "https://github.com/o/r", T0 + 1);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn text_that_cannot_be_tied_to_a_repository_and_a_proposer_is_refused_and_not_held() {
        let mut t = Table::default();
        for (proposer, url, rule) in [
            (Some(JIRA), "ext::sh -c id", "url"),
            (Some(JIRA), "https://127.0.0.1/o/r", "url"),
            (None, "https://github.com/o/r", "proposer"),
        ] {
            match t.receive(proposer, url, &evidence("X"), Some(&rules()), T0) {
                Received::Refused(d) => {
                    assert_eq!(d.state, ProposalState::Rejected, "{url}");
                    assert_eq!(d.rule.as_deref(), Some(rule), "{url}");
                    assert_eq!(d.proposer.as_deref(), proposer);
                    assert!(d.repo_id.is_none() || proposer.is_none());
                }
                other => panic!("{url}: {other:?}"),
            }
        }
        assert!(t.is_empty());
    }

    #[test]
    fn a_refusal_is_not_held_and_a_decision_about_a_held_claim_is() {
        let mut t = Table::default();
        for (proposer, url) in [
            (None, "https://github.com/o/r"),
            (Some(JIRA), "ext::sh"),
            (Some("stranger"), "https://github.com/o/r"),
        ] {
            let Received::Refused(d) = t.receive(proposer, url, &evidence("X"), Some(&rules()), T0)
            else {
                panic!()
            };
            assert!(!d.held, "{url}");
        }
        held(&mut t, JIRA, "https://github.com/o/r", T0);
        let decided = settle(&mut t, Some(&rules()), T0 + 1);
        assert!(decided.iter().all(|d| d.held), "{decided:?}");
        let gone = t.expire(T0 + 10_000_000, 1000);
        assert!(
            gone.iter().all(|d| d.held),
            "an expired claim was held until it expired"
        );
    }

    #[test]
    fn what_a_refusal_shows_of_a_bad_url_is_cleaned_and_cut() {
        let mut t = Table::default();
        let Received::Refused(d) = t.receive(
            Some(JIRA),
            &format!("ext::sh -c 'curl\nevil'\u{7}{}", "x".repeat(1000)),
            &evidence("X"),
            Some(&rules()),
            T0,
        ) else {
            panic!()
        };
        assert!(!d.url.contains('\n') && !d.url.contains('\u{7}'));
        assert!(d.url.chars().count() <= 200, "{}", d.url.chars().count());
    }

    #[test]
    fn a_proposer_the_rules_do_not_list_is_refused_up_front_and_not_held() {
        let mut t = Table::default();
        let Received::Refused(d) = t.receive(
            Some("stranger.polar.serviceaccount.cluster.local"),
            "https://github.com/o/r",
            &evidence("X"),
            Some(&rules()),
            T0,
        ) else {
            panic!()
        };
        assert_eq!(d.rule.as_deref(), Some("proposer"));
        assert!(t.is_empty());
    }

    #[test]
    fn with_no_rules_yet_a_proposal_is_held_to_be_judged_when_there_are() {
        let mut t = Table::default();
        assert!(matches!(
            t.receive(
                Some(JIRA),
                "https://github.com/o/r",
                &evidence("X"),
                None,
                T0
            ),
            Received::Held(_)
        ));
    }

    #[test]
    fn one_proposer_cannot_fill_the_table() {
        let mut t = Table::default();
        for n in 0..MAX_CLAIMS_PER_PROPOSER {
            held(&mut t, JIRA, &format!("https://github.com/o/r{n}"), T0);
        }
        let r = t.receive(
            Some(JIRA),
            "https://github.com/o/one-more",
            &evidence("X"),
            Some(&rules()),
            T0,
        );
        let Received::Refused(d) = r else {
            panic!("{r:?}")
        };
        assert_eq!(d.rule.as_deref(), Some("limits"));
        assert_eq!(t.len(), MAX_CLAIMS_PER_PROPOSER);
        // another proposer still has room, and an existing claim is still refreshed
        held(&mut t, GITLAB, "https://github.com/o/other", T0);
        assert!(matches!(
            t.receive(
                Some(JIRA),
                "https://github.com/o/r0",
                &evidence("X"),
                Some(&rules()),
                T0 + 1
            ),
            Received::Held(_)
        ));
    }

    #[test]
    fn the_table_as_a_whole_has_a_limit_too() {
        let mut t = Table::default();
        let mut n = 0;
        'fill: for proposer in 0..(MAX_CLAIMS / MAX_CLAIMS_PER_PROPOSER + 1) {
            let name = format!("p{proposer}.polar.serviceaccount.cluster.local");
            let mut r = rules();
            r.proposers.push(name.clone());
            for _ in 0..MAX_CLAIMS_PER_PROPOSER {
                match t.receive(
                    Some(&name),
                    &format!("https://github.com/o/r{n}"),
                    &evidence("X"),
                    Some(&r),
                    T0,
                ) {
                    Received::Held(_) => n += 1,
                    Received::Refused(_) => break 'fill,
                }
            }
        }
        assert_eq!(t.len(), MAX_CLAIMS);
    }

    // --- expiry

    #[test]
    fn a_claim_its_proposer_stops_announcing_expires_and_one_that_is_announced_does_not() {
        let mut t = Table::default();
        let stale = held(&mut t, JIRA, "https://github.com/o/stale", T0);
        let fresh = held(&mut t, JIRA, "https://github.com/o/fresh", T0);
        let ttl = 86_400_000;
        t.receive(
            Some(JIRA),
            "https://github.com/o/fresh",
            &evidence("X"),
            Some(&rules()),
            T0 + ttl,
        );
        let gone = t.expire(T0 + ttl + 1, ttl);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].state, ProposalState::Expired);
        assert_eq!(gone[0].url, "https://github.com/o/stale");
        assert!(t.claim(&stale).is_none());
        assert!(t.claim(&fresh).is_some());
    }

    #[test]
    fn a_claim_is_kept_for_exactly_the_time_limit() {
        let mut t = Table::default();
        let k = held(&mut t, JIRA, "https://github.com/o/r", T0);
        assert!(
            t.expire(T0 + 1000, 1000).is_empty(),
            "at the limit it is still held"
        );
        assert!(t.claim(&k).is_some());
        assert_eq!(t.expire(T0 + 1001, 1000).len(), 1);
    }

    // --- judging

    #[test]
    fn with_no_rules_every_claim_waits_for_them() {
        let mut t = Table::default();
        let k = held(&mut t, JIRA, "https://github.com/o/r", T0);
        let changed = settle(&mut t, None, T0);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].state, ProposalState::Pending);
        assert_eq!(changed[0].rule.as_deref(), Some(Waiting::Rules.as_str()));
        assert!(matches!(
            decision_of(&t, &k),
            Decision::Pending {
                waiting: Waiting::Rules,
                ..
            }
        ));
    }

    #[test]
    fn the_rules_arriving_decide_what_was_waiting() {
        let mut t = Table::default();
        let k = held(&mut t, JIRA, "https://github.com/o/r", T0);
        settle(&mut t, None, T0);
        let changed = settle(&mut t, Some(&rules()), T0 + 1);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].state, ProposalState::Accepted);
        assert!(matches!(decision_of(&t, &k), Decision::Accepted(_)));
    }

    #[test]
    fn only_a_decision_that_changed_is_reported_and_the_same_one_again_is_not() {
        let mut t = Table::default();
        held(&mut t, JIRA, "https://github.com/o/r", T0);
        assert_eq!(settle(&mut t, Some(&rules()), T0).len(), 1);
        assert!(settle(&mut t, Some(&rules()), T0 + 60_000).is_empty());
        let mut denying = rules();
        denying.denied.insert(id("https://github.com/o/r"));
        let changed = settle(&mut t, Some(&denying), T0 + 120_000);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].state, ProposalState::Rejected);
        assert_eq!(changed[0].rule.as_deref(), Some("denied"));
        assert_eq!(changed[0].decided_at, T0 + 120_000);
    }

    #[test]
    fn a_decision_report_carries_what_a_person_needs() {
        let mut t = Table::default();
        held(&mut t, JIRA, "https://github.com/o/r", T0);
        t.receive(
            Some(JIRA),
            "https://github.com/o/r",
            &evidence("X-2"),
            Some(&rules()),
            T0 + 7,
        );
        let d = &settle(&mut t, Some(&rules()), T0 + 9)[0];
        assert_eq!(d.repo_id, Some(id("https://github.com/o/r")));
        assert_eq!(d.url, "https://github.com/o/r");
        assert_eq!(d.proposer.as_deref(), Some(JIRA));
        assert_eq!(
            (d.first_seen, d.last_seen, d.announcements),
            (T0, T0 + 7, 2)
        );
        assert_eq!(d.evidence.len(), 2);
    }

    #[test]
    fn claims_are_judged_in_the_order_they_were_first_seen_so_the_flood_limit_is_a_sliding_window()
    {
        let mut t = Table::default();
        let mut limited = rules();
        limited.limits.per_proposer_per_hour = 2;
        let mut keys = Vec::new();
        for n in 0..4 {
            // announced out of order on purpose: the order is by first_seen, not by arrival
            let at = T0 + (3 - n) * 1000;
            match t.receive(
                Some(JIRA),
                &format!("https://github.com/o/r{n}"),
                &evidence("X"),
                Some(&limited),
                at,
            ) {
                Received::Held(k) => keys.push((k, at)),
                other => panic!("{other:?}"),
            }
        }
        keys.sort_by_key(|(_, at)| *at);
        settle(&mut t, Some(&limited), T0 + 10_000);
        let accepted: Vec<bool> = keys
            .iter()
            .map(|(k, _)| matches!(decision_of(&t, k), Decision::Accepted(_)))
            .collect();
        assert_eq!(
            accepted,
            [true, true, false, false],
            "the first two seen are taken"
        );
        assert!(matches!(
            decision_of(&t, &keys[2].0),
            Decision::Rejected {
                rule: Rule::Limits,
                ..
            }
        ));
        // An hour on, the first two are old news and the others fit.
        settle(&mut t, Some(&limited), T0 + 3000 + HOUR_MS + 1);
        let accepted: Vec<bool> = keys
            .iter()
            .map(|(k, _)| matches!(decision_of(&t, k), Decision::Accepted(_)))
            .collect();
        assert_eq!(accepted, [true, true, true, true]);
    }

    #[test]
    fn a_claim_exactly_an_hour_old_no_longer_counts_toward_the_flood_limit() {
        let mut t = Table::default();
        let mut limited = rules();
        limited.limits.per_proposer_per_hour = 1;
        let a = held(&mut t, JIRA, "https://github.com/o/a", T0);
        let b = held(&mut t, JIRA, "https://github.com/o/b", T0 + 1);
        settle(&mut t, Some(&limited), T0 + HOUR_MS - 1);
        assert!(
            matches!(
                decision_of(&t, &b),
                Decision::Rejected {
                    rule: Rule::Limits,
                    ..
                }
            ),
            "a minute short of the hour, a still counts"
        );
        settle(&mut t, Some(&limited), T0 + HOUR_MS);
        assert!(matches!(decision_of(&t, &a), Decision::Accepted(_)));
        assert!(
            matches!(decision_of(&t, &b), Decision::Accepted(_)),
            "an hour old, a does not count"
        );
    }

    #[test]
    fn a_second_proposer_of_an_accepted_repository_does_not_count_against_the_size_limit() {
        let mut t = Table::default();
        let mut one = rules();
        one.limits.max_entries = 1;
        let first = held(&mut t, JIRA, "https://github.com/o/a", T0);
        let second = held(&mut t, GITLAB, "https://github.com/o/a", T0 + 1);
        settle(&mut t, Some(&one), T0 + 10);
        assert!(matches!(decision_of(&t, &first), Decision::Accepted(_)));
        assert!(
            matches!(decision_of(&t, &second), Decision::Accepted(_)),
            "it is the same entry, so it fits"
        );
    }

    #[test]
    fn the_flood_limit_is_per_proposer() {
        let mut t = Table::default();
        let mut limited = rules();
        limited.limits.per_proposer_per_hour = 1;
        for (n, proposer) in [JIRA, GITLAB].iter().enumerate() {
            t.receive(
                Some(proposer),
                &format!("https://github.com/o/r{n}"),
                &evidence("X"),
                Some(&limited),
                T0 + n as i64,
            );
        }
        let changed = settle(&mut t, Some(&limited), T0 + 10);
        assert!(
            changed.iter().all(|d| d.state == ProposalState::Accepted),
            "{changed:?}"
        );
    }

    #[test]
    fn the_size_limit_counts_repositories_not_claims() {
        let mut t = Table::default();
        let mut small = rules();
        small.limits.max_entries = 2;
        t.receive(
            Some(JIRA),
            "https://github.com/o/a",
            &evidence("X"),
            Some(&small),
            T0,
        );
        t.receive(
            Some(GITLAB),
            "https://github.com/o/a",
            &evidence("X"),
            Some(&small),
            T0 + 1,
        );
        t.receive(
            Some(JIRA),
            "https://github.com/o/b",
            &evidence("X"),
            Some(&small),
            T0 + 2,
        );
        t.receive(
            Some(JIRA),
            "https://github.com/o/c",
            &evidence("X"),
            Some(&small),
            T0 + 3,
        );
        settle(&mut t, Some(&small), T0 + 10);
        let configs = t.accepted_configs();
        assert_eq!(configs.len(), 2, "a and b fit, c does not: {configs:?}");
        assert!(matches!(
            decision_of(&t, &key("https://github.com/o/c", JIRA)),
            Decision::Rejected {
                rule: Rule::Limits,
                ..
            }
        ));
    }

    #[test]
    fn a_name_is_looked_up_once_however_many_claims_are_on_it_and_not_at_all_for_a_refused_host() {
        let mut t = Table::default();
        for n in 0..5 {
            t.receive(
                Some(JIRA),
                &format!("https://github.com/o/r{n}"),
                &evidence("X"),
                Some(&rules()),
                T0,
            );
        }
        t.receive(
            Some(JIRA),
            "https://elsewhere.example.net/o/r",
            &evidence("X"),
            Some(&rules()),
            T0,
        );
        let asked = RefCell::new(Vec::new());
        let results = t.evaluate(
            Some(&rules()),
            &|host| {
                asked.borrow_mut().push(host.to_string());
                public(host)
            },
            T0 + 1,
        );
        assert_eq!(results.len(), 6);
        assert_eq!(
            asked.into_inner(),
            ["github.com"],
            "one lookup, and none for a host the rules refuse"
        );
    }

    #[test]
    fn evaluating_changes_nothing_until_it_is_applied() {
        let mut t = Table::default();
        let k = held(&mut t, JIRA, "https://github.com/o/r", T0);
        let results = t.evaluate(Some(&rules()), &public, T0);
        assert!(t.claim(&k).unwrap().decision.is_none());
        t.apply(results, T0);
        assert!(t.claim(&k).unwrap().decision.is_some());
    }

    #[test]
    fn a_result_for_a_claim_that_has_since_expired_is_ignored() {
        let mut t = Table::default();
        held(&mut t, JIRA, "https://github.com/o/r", T0);
        let results = t.evaluate(Some(&rules()), &public, T0);
        t.expire(T0 + 10_000, 1000);
        assert!(t.apply(results, T0 + 10_000).is_empty());
        assert!(t.is_empty());
    }

    // --- what is added to the set

    #[test]
    fn what_is_accepted_is_added_with_the_defaults_and_the_credential_for_its_host() {
        let mut t = Table::default();
        held(&mut t, JIRA, "https://github.com/o/pub", T0);
        held(&mut t, JIRA, "https://gitlab.example.org/g/priv", T0 + 1);
        held(&mut t, JIRA, "https://other.example.net/o/r", T0 + 2); // refused: the host
        settle(&mut t, Some(&rules()), T0 + 10);
        let configs = t.accepted_configs();
        assert_eq!(configs.len(), 2);
        let public = configs
            .iter()
            .find(|c| c.repo_url.contains("github.com"))
            .unwrap();
        assert_eq!(public.credential, None);
        assert_eq!(public.remotes, ["origin"]);
        assert!(public.refs.is_empty(), "the default branch");
        assert_eq!(public.frequency, 900);
        assert_eq!(public.max_depth, 100);
        assert_eq!(public.shallow_depth, None);
        assert_eq!(public.repo_id, id("https://github.com/o/pub"));
        let private = configs
            .iter()
            .find(|c| c.repo_url.contains("gitlab"))
            .unwrap();
        assert_eq!(private.credential.as_deref(), Some("gitlab-sei"));
    }

    #[test]
    fn a_repository_two_proposers_named_is_added_once_and_stays_while_either_announces_it() {
        let mut t = Table::default();
        held(&mut t, JIRA, "https://github.com/o/r", T0);
        held(&mut t, GITLAB, "https://github.com/o/r", T0 + 1);
        settle(&mut t, Some(&rules()), T0 + 10);
        assert_eq!(t.accepted_configs().len(), 1);
        // GitLab keeps announcing; Jira stops.
        t.receive(
            Some(GITLAB),
            "https://github.com/o/r",
            &evidence("X"),
            Some(&rules()),
            T0 + 2000,
        );
        t.expire(T0 + 2500, 1500);
        assert_eq!(t.len(), 1);
        assert_eq!(t.accepted_configs().len(), 1);
        t.expire(T0 + 4000, 1500);
        assert!(t.accepted_configs().is_empty());
    }

    #[test]
    fn nothing_pending_or_rejected_is_added() {
        let mut t = Table::default();
        held(&mut t, JIRA, "https://github.com/o/r", T0);
        let mut approving = rules();
        approving.require_approval = true;
        settle(&mut t, Some(&approving), T0);
        assert!(t.accepted_configs().is_empty());
        settle(&mut t, None, T0 + 1);
        assert!(t.accepted_configs().is_empty());
    }

    #[test]
    fn the_added_repositories_come_in_a_stable_order() {
        let mut t = Table::default();
        for name in ["zeta", "alpha", "mid"] {
            held(&mut t, JIRA, &format!("https://github.com/o/{name}"), T0);
        }
        settle(&mut t, Some(&rules()), T0 + 1);
        let order: Vec<String> = t
            .accepted_configs()
            .iter()
            .map(|c| c.repo_id.to_string())
            .collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
        assert_eq!(order.len(), 3);
    }

    fn config(url: &str, frequency: u64) -> RepoObservationConfig {
        let mut c = RepoObservationConfig::new(
            id(url),
            url.to_string(),
            vec!["origin".into()],
            None,
            vec![],
        );
        c.frequency = frequency;
        c
    }

    #[test]
    fn the_signed_section_wins_over_a_proposal_for_the_same_repository() {
        let explicit = vec![config("https://github.com/o/a.git", 3600)];
        let proposed = vec![
            config("https://github.com/o/a", 900),
            config("https://github.com/o/b", 900),
        ];
        let merged = merge(&explicit, proposed);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].frequency, 3600,
            "the signed entry is first and as signed"
        );
        assert_eq!(merged[1].repo_id, id("https://github.com/o/b"));
    }

    #[test]
    fn nothing_signed_and_nothing_proposed_is_an_empty_set() {
        assert!(merge(&[], vec![]).is_empty());
        let only = merge(&[], vec![config("https://github.com/o/b", 900)]);
        assert_eq!(only.len(), 1);
    }
}
