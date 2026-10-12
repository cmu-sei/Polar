//! What an agent holds once it believes authority updates: the latest of each kind, and enough of the
//! credentials agent's sequence to notice an update it missed (docs/architecture/authority-distribution.md).
//!
//! It is pure: it is given payloads that have already been verified (`authority_update::verify`) and decides
//! only whether each replaces what is held. The credentials agent holds the same state, so what it republishes
//! and what a leaf holds are the same thing.

use crate::authority_update::{AuthorityKind, AuthorityPayload};
use std::collections::HashMap;

/// What became of a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// It is now the latest of its kind.
    Replaced,
    /// Its version is not higher than the one held: a republish of what is known, or an old update arriving
    /// late. Nothing changed.
    Stale { held: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub applied: Applied,
    /// The sequence jumped past the next one, so an update was missed and the agent should ask for the
    /// current state. Never set for what arrives as the answer to such a request.
    pub missed_an_update: bool,
}

#[derive(Debug, Default)]
pub struct AuthorityState {
    last_sequence: u64,
    held: HashMap<AuthorityKind, AuthorityPayload>,
}

impl AuthorityState {
    /// Takes a verified payload. `from_catch_up` is true for what arrives in answer to a request for the
    /// current state: its sequences jump by nature, and a jump there is not a missed update.
    pub fn apply(&mut self, payload: AuthorityPayload, from_catch_up: bool) -> Outcome {
        // The place in the credentials agent's sequence is whatever was last heard; the first thing heard sets
        // it, since there is nothing before it to have missed.
        let missed_an_update = !from_catch_up
            && self.last_sequence != 0
            && payload.sequence > self.last_sequence.saturating_add(1);
        self.last_sequence = self.last_sequence.max(payload.sequence);

        let kind = payload.body.kind();
        let applied = match self.held.get(&kind) {
            Some(held) if payload.version <= held.version => Applied::Stale { held: held.version },
            _ => {
                self.held.insert(kind, payload);
                Applied::Replaced
            }
        };
        Outcome {
            applied,
            missed_an_update,
        }
    }

    /// The latest payload of a kind, if one has arrived.
    pub fn held(&self, kind: AuthorityKind) -> Option<&AuthorityPayload> {
        self.held.get(&kind)
    }

    /// The version held of each kind, for asking whether anything newer exists.
    pub fn versions(&self) -> HashMap<AuthorityKind, u64> {
        self.held.iter().map(|(k, p)| (*k, p.version)).collect()
    }

    /// The highest sequence seen.
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority_update::AuthorityBody;

    fn policy(sequence: u64, version: u64) -> AuthorityPayload {
        AuthorityPayload {
            sequence,
            version,
            body: AuthorityBody::Policy {
                source: format!("// version {version}"),
            },
            authored_by: None,
        }
    }

    fn groups(sequence: u64, version: u64) -> AuthorityPayload {
        AuthorityPayload {
            sequence,
            version,
            body: AuthorityBody::Groups {
                memberships: Default::default(),
            },
            authored_by: None,
        }
    }

    #[test]
    fn a_higher_version_replaces_and_an_equal_or_lower_one_is_stale() {
        let mut state = AuthorityState::default();
        assert_eq!(state.apply(policy(1, 1), false).applied, Applied::Replaced);
        assert_eq!(state.apply(policy(2, 2), false).applied, Applied::Replaced);
        // A republish of what is held, and an old one arriving late.
        assert_eq!(
            state.apply(policy(2, 2), false).applied,
            Applied::Stale { held: 2 }
        );
        assert_eq!(
            state.apply(policy(1, 1), false).applied,
            Applied::Stale { held: 2 }
        );
        let AuthorityBody::Policy { source } = &state.held(AuthorityKind::Policy).unwrap().body
        else {
            panic!()
        };
        assert_eq!(
            source, "// version 2",
            "an old update never replaces a newer one"
        );
    }

    #[test]
    fn each_kind_has_a_version_of_its_own() {
        let mut state = AuthorityState::default();
        state.apply(policy(1, 5), false);
        // Groups version 1 is not older than policy version 5.
        assert_eq!(state.apply(groups(2, 1), false).applied, Applied::Replaced);
        assert_eq!(
            state.versions(),
            HashMap::from([(AuthorityKind::Policy, 5), (AuthorityKind::Groups, 1)])
        );
        assert!(state.held(AuthorityKind::Revoked).is_none());
    }

    #[test]
    fn a_jump_in_the_sequence_means_an_update_was_missed() {
        let mut state = AuthorityState::default();
        // The first thing heard sets the place: there is nothing to have missed yet.
        assert!(!state.apply(policy(4, 1), false).missed_an_update);
        assert!(!state.apply(groups(5, 1), false).missed_an_update);
        assert!(state.apply(policy(8, 2), false).missed_an_update);
        assert_eq!(state.last_sequence(), 8);
        // Heard again, or in order from there, is not a miss.
        assert!(!state.apply(policy(8, 2), false).missed_an_update);
        assert!(!state.apply(groups(9, 2), false).missed_an_update);
    }

    #[test]
    fn the_answer_to_a_request_for_the_current_state_never_counts_as_a_miss() {
        let mut state = AuthorityState::default();
        state.apply(policy(1, 1), false);
        // The current policy was sequence 9, the current groups 14: the sequences between were other updates.
        assert!(!state.apply(policy(9, 3), true).missed_an_update);
        assert!(!state.apply(groups(14, 6), true).missed_an_update);
        assert_eq!(state.last_sequence(), 14);
        assert_eq!(state.versions()[&AuthorityKind::Groups], 6);
    }

    #[test]
    fn a_stale_update_does_not_move_the_sequence_backwards() {
        let mut state = AuthorityState::default();
        state.apply(policy(10, 3), false);
        state.apply(policy(2, 1), false);
        assert_eq!(state.last_sequence(), 10);
    }
}
