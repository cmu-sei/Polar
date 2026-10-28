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

//! What cert-issuer's floor has open, from what cert-issuer hears (docs/architecture/core-restarts.md, question 2, and
//! "The plan", step 2). A pure state: cert-issuer tells it what happened and when, and asks it what is open now.
//!
//! The rule, with cert-issuer deciding, since it holds the floor:
//!
//! - *Presence.* The credentials agent is present while cert-issuer has heard it within the bound ([`PRESENCE_BOUND`],
//!   two of its 30-second record requests).
//! - *A fresh cluster* (a record with nothing but cert-issuer's own certificates when it started) is bootstrap until
//!   the credentials agent is first heard: every entry is open.
//! - *A start with a record* holds everything closed until the credentials agent is heard, or the bound passes.
//! - Then: the credentials agent present, nothing opens (Cassini's and the observer's restarts are held requests,
//!   step 3); absent while cert-issuer's Cassini session holds, its entry alone opens (its crash, situation 3); absent
//!   with Cassini gone too, every entry opens (two or more of the core down: bootstrap again).
//!
//! No message opens an entry but one: hearing the credentials agent, or Cassini accepting cert-issuer, only ever closes,
//! and what opens is an absence, and time. The one is the credentials agent's own successor ticket, for its own pod
//! (`crate::ticket`; docs/architecture/core-restarts.md, "Step 5, the ticket"; the maintainer, 2026-10-28), taken only
//! from the credentials agent, whom whoever could send it as already holds the key of.

use std::time::{Duration, Instant};

/// How long the credentials agent counts as present after cert-issuer last heard it: two missed record requests.
pub const PRESENCE_BOUND: Duration = Duration::from_secs(60);

/// Which bootstrap entries are open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Open {
    /// None: the core is up, or cert-issuer has just restarted and has not yet heard whether it is.
    Nothing,
    /// The credentials agent's alone: it is gone while the rest of the core is up.
    CredentialsAgentOnly,
    /// Every bootstrap entry: a fresh cluster, or the credentials agent and Cassini both gone.
    All,
}

impl Open {
    /// Whether a token-only request for a bootstrap identity is admitted; `credentials_agent` says whether it is the
    /// credentials agent's.
    pub fn admits(self, credentials_agent: bool) -> bool {
        match self {
            Open::All => true,
            Open::CredentialsAgentOnly => credentials_agent,
            Open::Nothing => false,
        }
    }
}

/// What cert-issuer hears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Its Cassini client registered with the broker.
    CassiniRegistered,
    /// Its Cassini client lost the broker.
    CassiniLost,
    /// The credentials agent spoke to it: a record request over Cassini (and, from step 3, its poll over mTLS).
    CredentialsAgent,
}

#[derive(Debug, Clone)]
pub struct FloorState {
    fresh: bool,
    started: Instant,
    bound: Duration,
    cassini: bool,
    last_heard: Option<Instant>,
}

impl FloorState {
    /// The state of a cert-issuer that started at `started`, on a fresh cluster or not.
    pub fn new(fresh: bool, started: Instant) -> Self {
        Self::with_bound(fresh, started, PRESENCE_BOUND)
    }

    pub fn with_bound(fresh: bool, started: Instant, bound: Duration) -> Self {
        Self {
            fresh,
            started,
            bound,
            cassini: false,
            last_heard: None,
        }
    }

    pub fn hear(&mut self, heard: Heard, at: Instant) {
        match heard {
            Heard::CassiniRegistered => self.cassini = true,
            Heard::CassiniLost => self.cassini = false,
            Heard::CredentialsAgent => {
                self.last_heard = Some(self.last_heard.map_or(at, |last| last.max(at)));
            }
        }
    }

    /// What is open at `now`.
    pub fn open(&self, now: Instant) -> Open {
        let Some(last) = self.last_heard else {
            if self.fresh {
                return Open::All;
            }
            if now.saturating_duration_since(self.started) < self.bound {
                return Open::Nothing;
            }
            return self.absent();
        };
        if now.saturating_duration_since(last) < self.bound {
            Open::Nothing
        } else {
            self.absent()
        }
    }

    /// The credentials agent absent: its entry alone while Cassini holds, every entry without it.
    fn absent(&self) -> Open {
        if self.cassini {
            Open::CredentialsAgentOnly
        } else {
            Open::All
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn a_fresh_cluster_is_open_until_the_credentials_agent_is_first_heard() {
        let t = Instant::now();
        let mut s = FloorState::new(true, t);
        assert_eq!(s.open(t), Open::All);
        s.hear(Heard::CassiniRegistered, t + 10 * S);
        assert_eq!(
            s.open(t + 20 * S),
            Open::All,
            "Cassini up is not nominal yet"
        );
        assert_eq!(s.open(t + 3600 * S), Open::All, "nor is time");
        s.hear(Heard::CredentialsAgent, t + 30 * S);
        assert_eq!(s.open(t + 30 * S), Open::Nothing);
    }

    #[test]
    fn a_restart_with_a_record_holds_everything_closed_for_the_bound() {
        let t = Instant::now();
        let s = FloorState::new(false, t);
        assert_eq!(s.open(t), Open::Nothing);
        assert_eq!(s.open(t + 59 * S), Open::Nothing);
        assert_eq!(
            s.open(t + 60 * S),
            Open::All,
            "nothing heard, Cassini gone: bootstrap"
        );
        let mut up = FloorState::new(false, t);
        up.hear(Heard::CassiniRegistered, t + S);
        assert_eq!(
            up.open(t + 60 * S),
            Open::CredentialsAgentOnly,
            "Cassini up, the credentials agent not"
        );
    }

    #[test]
    fn the_credentials_agent_heard_closes_everything_until_it_falls_silent_for_the_bound() {
        let t = Instant::now();
        let mut s = FloorState::new(false, t);
        s.hear(Heard::CassiniRegistered, t + S);
        s.hear(Heard::CredentialsAgent, t + 2 * S);
        assert_eq!(s.open(t + 61 * S), Open::Nothing, "59 s since it was heard");
        assert_eq!(
            s.open(t + 62 * S),
            Open::CredentialsAgentOnly,
            "its crash: its entry alone"
        );
        s.hear(Heard::CredentialsAgent, t + 70 * S);
        assert_eq!(
            s.open(t + 70 * S),
            Open::Nothing,
            "the new one heard: closed at once"
        );
    }

    #[test]
    fn the_credentials_agent_and_cassini_both_gone_is_bootstrap_again() {
        let t = Instant::now();
        let mut s = FloorState::new(false, t);
        s.hear(Heard::CassiniRegistered, t + S);
        s.hear(Heard::CredentialsAgent, t + 2 * S);
        s.hear(Heard::CassiniLost, t + 10 * S);
        assert_eq!(
            s.open(t + 30 * S),
            Open::Nothing,
            "Cassini restarting, the credentials agent heard lately"
        );
        assert_eq!(s.open(t + 62 * S), Open::All);
        s.hear(Heard::CassiniRegistered, t + 70 * S);
        assert_eq!(s.open(t + 70 * S), Open::CredentialsAgentOnly);
    }

    #[test]
    fn hearing_out_of_order_never_moves_the_last_time_heard_back() {
        let t = Instant::now();
        let mut s = FloorState::new(false, t);
        s.hear(Heard::CredentialsAgent, t + 50 * S);
        s.hear(Heard::CredentialsAgent, t + 10 * S);
        assert_eq!(s.open(t + 100 * S), Open::Nothing, "heard 50 s ago, not 90");
    }

    #[test]
    fn what_is_admitted_follows_what_is_open() {
        assert!(Open::All.admits(true) && Open::All.admits(false));
        assert!(Open::CredentialsAgentOnly.admits(true));
        assert!(!Open::CredentialsAgentOnly.admits(false));
        assert!(!Open::Nothing.admits(true) && !Open::Nothing.admits(false));
    }
}
