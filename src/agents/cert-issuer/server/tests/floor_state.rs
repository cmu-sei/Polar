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

//! Properties of cert-issuer's floor state (docs/architecture/core-restarts.md, "The plan", step 2): no message opens
//! an entry; for any sequence of events, what is open follows the table; after a restart with a record, nothing
//! opens before the credentials agent is heard or the bound passes.

use cert_issuer::floor_state::{FloorState, Heard, Open, PRESENCE_BOUND};
use proptest::prelude::*;
use std::time::{Duration, Instant};

fn heard() -> impl Strategy<Value = Heard> {
    prop_oneof![
        Just(Heard::CassiniRegistered),
        Just(Heard::CassiniLost),
        Just(Heard::CredentialsAgent),
    ]
}

/// What cert-issuer hears, each after a gap of up to two minutes from the one before, from its start.
fn history() -> impl Strategy<Value = Vec<(u64, Heard)>> {
    prop::collection::vec((0u64..120, heard()), 0..12)
}

/// The history at absolute times from `start`, and the state that heard it.
fn replay(
    fresh: bool,
    start: Instant,
    history: &[(u64, Heard)],
) -> (FloorState, Vec<(Instant, Heard)>) {
    let mut state = FloorState::new(fresh, start);
    let mut at = start;
    let mut timed = Vec::new();
    for (gap, heard) in history {
        at += Duration::from_secs(*gap);
        state.hear(*heard, at);
        timed.push((at, *heard));
    }
    (state, timed)
}

/// The table, read off the whole history at `now` rather than kept as it goes.
fn table(fresh: bool, start: Instant, timed: &[(Instant, Heard)], now: Instant) -> Open {
    let cassini = timed
        .iter()
        .rfind(|(_, h)| *h != Heard::CredentialsAgent)
        .is_some_and(|(_, h)| *h == Heard::CassiniRegistered);
    let last_heard = timed
        .iter()
        .filter(|(_, h)| *h == Heard::CredentialsAgent)
        .map(|(at, _)| *at)
        .max();
    let absent = if cassini {
        Open::CredentialsAgentOnly
    } else {
        Open::All
    };
    match last_heard {
        // Never heard: a fresh cluster is bootstrap; a restart waits out the bound, then the credentials agent is absent.
        None if fresh => Open::All,
        None if now.duration_since(start) < PRESENCE_BOUND => Open::Nothing,
        None => absent,
        // Heard: present within the bound, absent after it.
        Some(at) if now.duration_since(at) < PRESENCE_BOUND => Open::Nothing,
        Some(_) => absent,
    }
}

proptest! {
    #[test]
    fn what_is_open_follows_the_table_for_any_history(
        fresh in any::<bool>(),
        history in history(),
        later in 0u64..200,
    ) {
        let start = Instant::now();
        let (state, timed) = replay(fresh, start, &history);
        let now = timed.last().map_or(start, |(at, _)| *at) + Duration::from_secs(later);
        prop_assert_eq!(state.open(now), table(fresh, start, &timed, now));
    }

    #[test]
    fn no_message_opens_an_entry(
        fresh in any::<bool>(),
        history in history(),
        gap in 0u64..120,
        message in prop_oneof![Just(Heard::CassiniRegistered), Just(Heard::CredentialsAgent)],
        later in 0u64..200,
    ) {
        // Hearing the credentials agent, or Cassini accepting cert-issuer, is something anyone who can reach it could
        // fake; so neither may open what was closed, now or at any time after.
        let start = Instant::now();
        let (before, timed) = replay(fresh, start, &history);
        let at = timed.last().map_or(start, |(at, _)| *at) + Duration::from_secs(gap);
        let mut after = before.clone();
        after.hear(message, at);
        for query in [at, at + Duration::from_secs(later)] {
            prop_assert!(
                after.open(query) <= before.open(query),
                "{:?} opened {:?} into {:?}", message, before.open(query), after.open(query)
            );
        }
    }

    #[test]
    fn after_a_restart_with_a_record_nothing_opens_before_the_credentials_agent_is_heard_or_the_bound_passes(
        history in prop::collection::vec((0u64..20, prop_oneof![Just(Heard::CassiniRegistered), Just(Heard::CassiniLost)]), 0..6),
        within in 0u64..60,
    ) {
        let start = Instant::now();
        let (state, _) = replay(false, start, &history);
        let now = start + Duration::from_secs(within);
        prop_assert_eq!(state.open(now), Open::Nothing);
    }
}
