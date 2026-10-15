//! A proposal that a repository be observed, and what the controller decided about it
//! (docs/architecture/proposals.md).
//!
//! The proposer's identity is not a field: it is what the broker says published the message. A proposal is
//! about something the proposer saw, so every string in it is untrusted.

use polar::graph::nodes::git::RepoId;
use rkyv::{Archive, Deserialize, Serialize};

/// Where agents propose repositories to observe: one topic per kind, as `proposals.md` has it.
pub const PROPOSAL_OBSERVE_REPO_TOPIC: &str = "polar-proposal-observe-repo";

/// Where the controller says what it decided about each proposal. The git processor writes them to the graph so a
/// person can read them; nothing reads them back to rebuild a decision.
pub const PROPOSAL_DECISIONS_TOPIC: &str = "polar.git.proposals.decisions";

/// How much evidence a proposal may carry, and how long each part may be. A proposal is untrusted and
/// the controller keeps it in memory.
pub const MAX_EVIDENCE_SOURCE: usize = 64;
pub const MAX_EVIDENCE_REFERENCE: usize = 256;

/// Where the proposer found the repository, for the trail and for a person deciding whether to approve it.
#[derive(Serialize, Deserialize, Archive, Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// What kind of place: `jira`, `gitlab`, ...
    pub source: String,
    /// Which one: an issue key, a project path.
    pub reference: Option<String>,
}

impl Evidence {
    /// The evidence made fit to keep and show: one line, no control characters, within the bounds.
    pub fn tidy(&self) -> Evidence {
        fn clean(text: &str, max: usize) -> String {
            let one_line: String = text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            one_line.chars().take(max).collect()
        }
        Evidence {
            source: clean(&self.source, MAX_EVIDENCE_SOURCE),
            reference: self
                .reference
                .as_deref()
                .map(|r| clean(r, MAX_EVIDENCE_REFERENCE)),
        }
    }
}

/// "Here is a repository I saw; you may want to observe it."
#[derive(Serialize, Deserialize, Archive, Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// The URL as the proposer found it. Untrusted: the controller reads it, and passes on only what it made of it.
    pub url: String,
    pub evidence: Evidence,
}

#[derive(Serialize, Deserialize, Archive, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalState {
    Accepted,
    Rejected,
    /// Not refused, and not accepted: it waits for something, and `rule` says what.
    Pending,
    /// Its proposer stopped announcing it, so the controller dropped it.
    Expired,
}

impl ProposalState {
    /// The word written to the graph.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProposalState::Accepted => "accepted",
            ProposalState::Rejected => "rejected",
            ProposalState::Pending => "pending",
            ProposalState::Expired => "expired",
        }
    }
}

/// What the controller decided about one proposer's proposal of one repository.
#[derive(Serialize, Deserialize, Archive, Debug, Clone, PartialEq, Eq)]
pub struct ProposalDecision {
    /// `None` when the proposal could not be tied to a repository: its URL was unreadable.
    pub repo_id: Option<RepoId>,
    /// The canonical URL if it was read, otherwise the proposer's text, cleaned and cut short.
    pub url: String,
    /// The identity the broker named, if it named one.
    pub proposer: Option<String>,
    pub state: ProposalState,
    /// For a rejection the rule that refused it, for a pending proposal what it waits for.
    pub rule: Option<String>,
    pub reason: String,
    pub evidence: Vec<Evidence>,
    pub first_seen: i64,
    pub last_seen: i64,
    /// How many times the proposer has announced it.
    pub announcements: u32,
    /// When this decision was made, in milliseconds since the epoch.
    pub decided_at: i64,
    /// Whether the controller holds the proposal in its table. A refusal the controller makes outright (no publisher
    /// identity, an unreadable URL, an identity the rules do not list, a full table) is published for whoever is
    /// listening but is not held, and what is not held is not unbounded in the graph either: the processor
    /// writes only the decisions about proposals that are held.
    pub held: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision() -> ProposalDecision {
        ProposalDecision {
            repo_id: Some(RepoId::new("https_github.com_o_r".into())),
            url: "https://github.com/o/r".into(),
            proposer: Some("jira-consumer.polar.serviceaccount.cluster.local".into()),
            state: ProposalState::Pending,
            rule: Some("awaiting approval".into()),
            reason: "the rules require a person to approve it".into(),
            evidence: vec![Evidence {
                source: "jira".into(),
                reference: Some("ABC-12".into()),
            }],
            first_seen: 1,
            last_seen: 2,
            announcements: 3,
            decided_at: 4,
            held: true,
        }
    }

    #[test]
    fn a_proposal_survives_the_wire() {
        let sent = Proposal {
            url: "https://github.com/o/r".into(),
            evidence: Evidence {
                source: "gitlab".into(),
                reference: None,
            },
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&sent).unwrap();
        assert_eq!(
            rkyv::from_bytes::<Proposal, rkyv::rancor::Error>(&bytes).unwrap(),
            sent
        );
    }

    #[test]
    fn a_decision_survives_the_wire_with_and_without_a_repository() {
        for d in [
            decision(),
            ProposalDecision {
                repo_id: None,
                proposer: None,
                rule: None,
                held: false,
                ..decision()
            },
        ] {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&d).unwrap();
            assert_eq!(
                rkyv::from_bytes::<ProposalDecision, rkyv::rancor::Error>(&bytes).unwrap(),
                d
            );
        }
    }

    #[test]
    fn each_state_has_its_own_word() {
        let words: std::collections::HashSet<_> = [
            ProposalState::Accepted,
            ProposalState::Rejected,
            ProposalState::Pending,
            ProposalState::Expired,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect();
        assert_eq!(words.len(), 4);
    }

    #[test]
    fn evidence_is_made_one_short_clean_line() {
        let e = Evidence {
            source: "ji\nra\u{7}".into(),
            reference: Some(format!("ABC-12\n{}", "x".repeat(1000))),
        }
        .tidy();
        assert_eq!(e.source, "ji ra");
        assert_eq!(
            e.reference.as_ref().unwrap().chars().count(),
            MAX_EVIDENCE_REFERENCE
        );
        assert!(!e.reference.unwrap().contains('\n'));
        let long_source = Evidence {
            source: "s".repeat(500),
            reference: None,
        }
        .tidy();
        assert_eq!(long_source.source.chars().count(), MAX_EVIDENCE_SOURCE);
    }
}
