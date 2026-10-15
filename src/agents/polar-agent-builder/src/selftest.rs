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

//! A check that an agent can still hear the broker.
//!
//! An agent can be registered, healthy and subscribed on paper while nothing reaches it: a
//! subscription the broker lost, a route inside the client that no longer delivers. Its liveness
//! probe cannot see that, and its log is as quiet as an idle agent's. So the agent talks to
//! itself: it publishes a probe on a topic it subscribes to, and expects to hear it come back. A
//! probe unheard for too long means the agent is deaf, and the health check is told the Cassini
//! connection is lost, which the health actor already knows how to act on (a deadline, then an
//! orderly shutdown, and the Job replaces the pod).
//!
//! What it proves: the whole path out and back works now, through the same session, client and
//! router the agent's topics use. What it does not prove: that each of the agent's own topics is
//! subscribed. It checks the machinery they all share, not each one.
//!
//! This module is the decision, free of actors and clocks: it is told the time and what was
//! heard, and says what to do.

use std::time::Duration;

/// What the supervisor is asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Publish a probe carrying this number.
    Probe(u64),
    /// No probe has come back for this long: the agent cannot hear the broker.
    Deaf { unanswered_for: Duration },
    /// A probe came back after the agent was judged deaf.
    Hearing,
}

pub struct SelfTest {
    interval: Duration,
    timeout: Duration,
    /// The number of the last probe sent.
    sent: u64,
    last_sent_at: Option<Duration>,
    /// When the oldest probe still unanswered was sent.
    unanswered_since: Option<Duration>,
    deaf: bool,
}

impl SelfTest {
    /// A probe every `interval`; deaf when one has gone `timeout` without any answer.
    pub fn new(interval: Duration, timeout: Duration) -> Self {
        Self {
            interval,
            timeout,
            sent: 0,
            last_sent_at: None,
            unanswered_since: None,
            deaf: false,
        }
    }

    /// The client registered (again): everything is subscribed afresh, so earlier silence says
    /// nothing about now.
    pub fn registered(&mut self) {
        self.last_sent_at = None;
        self.unanswered_since = None;
        self.deaf = false;
    }

    /// The time has come to look. `connected` is whether the client is registered; while it is
    /// not there is nothing to judge, the health check already knows.
    pub fn tick(&mut self, now: Duration, connected: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        if !connected {
            return actions;
        }
        if let Some(since) = self.unanswered_since
            && !self.deaf
            && now.saturating_sub(since) >= self.timeout
        {
            self.deaf = true;
            actions.push(Action::Deaf {
                unanswered_for: now.saturating_sub(since),
            });
        }
        if self
            .last_sent_at
            .is_none_or(|at| now.saturating_sub(at) >= self.interval)
        {
            self.sent += 1;
            self.last_sent_at = Some(now);
            self.unanswered_since.get_or_insert(now);
            actions.push(Action::Probe(self.sent));
        }
        actions
    }

    /// The probe numbered `number` came back.
    pub fn heard(&mut self, number: u64) -> Option<Action> {
        // Any probe that was sent proves the path works, however late. One never sent proves
        // nothing.
        if number == 0 || number > self.sent {
            return None;
        }
        self.unanswered_since = None;
        if std::mem::take(&mut self.deaf) {
            Some(Action::Hearing)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_secs(30);
    const TIMEOUT: Duration = Duration::from_secs(20);

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn machine() -> SelfTest {
        SelfTest::new(INTERVAL, TIMEOUT)
    }

    #[test]
    fn the_first_look_sends_a_probe() {
        let mut test = machine();
        assert_eq!(test.tick(secs(0), true), vec![Action::Probe(1)]);
    }

    #[test]
    fn nothing_is_sent_while_the_client_is_not_registered() {
        let mut test = machine();
        assert_eq!(test.tick(secs(0), false), vec![]);
        assert_eq!(test.tick(secs(100), false), vec![]);
    }

    #[test]
    fn a_probe_is_sent_once_an_interval_not_at_every_look() {
        let mut test = machine();
        assert_eq!(test.tick(secs(0), true), vec![Action::Probe(1)]);
        assert_eq!(test.heard(1), None);
        assert_eq!(test.tick(secs(5), true), vec![]);
        assert_eq!(test.tick(secs(29), true), vec![]);
        assert_eq!(test.tick(secs(30), true), vec![Action::Probe(2)]);
    }

    #[test]
    fn a_probe_unanswered_for_the_timeout_makes_the_agent_deaf_once() {
        let mut test = machine();
        test.tick(secs(0), true);
        assert_eq!(test.tick(secs(19), true), vec![], "not yet");
        assert_eq!(
            test.tick(secs(20), true),
            vec![Action::Deaf {
                unanswered_for: secs(20)
            }]
        );
        // Still deaf, and said once: the next look at the interval probes again, silently.
        assert_eq!(test.tick(secs(25), true), vec![]);
        assert_eq!(test.tick(secs(30), true), vec![Action::Probe(2)]);
        assert_eq!(test.tick(secs(60), true), vec![Action::Probe(3)]);
    }

    #[test]
    fn deafness_is_said_once_however_long_it_lasts() {
        let mut test = machine();
        test.tick(secs(0), true);
        assert_eq!(
            test.tick(secs(30), true),
            vec![
                Action::Deaf {
                    unanswered_for: secs(30)
                },
                Action::Probe(2)
            ]
        );
        assert_eq!(test.tick(secs(60), true), vec![Action::Probe(3)]);
    }

    #[test]
    fn a_probe_that_comes_back_after_the_agent_was_judged_deaf_restores_it() {
        let mut test = machine();
        test.tick(secs(0), true);
        test.tick(secs(20), true);
        assert_eq!(test.heard(1), Some(Action::Hearing));
        assert_eq!(test.heard(1), None, "only once");
        // Judged afresh from here: a new silence is deaf again.
        assert_eq!(test.tick(secs(30), true), vec![Action::Probe(2)]);
        assert_eq!(
            test.tick(secs(50), true),
            vec![Action::Deaf {
                unanswered_for: secs(20)
            }]
        );
    }

    #[test]
    fn a_late_answer_to_an_old_probe_restores_the_agent() {
        let mut test = machine();
        test.tick(secs(0), true);
        test.tick(secs(30), true); // judged deaf, probe 2 sent
        assert_eq!(
            test.heard(1),
            Some(Action::Hearing),
            "the path works, however late"
        );
        assert_eq!(
            test.tick(secs(45), true),
            vec![],
            "nothing is owed any more"
        );
    }

    #[test]
    fn an_answer_to_a_probe_never_sent_is_ignored() {
        let mut test = machine();
        test.tick(secs(0), true);
        assert_eq!(test.heard(7), None);
        assert_eq!(
            test.tick(secs(20), true),
            vec![Action::Deaf {
                unanswered_for: secs(20)
            }],
            "still unanswered"
        );
    }

    #[test]
    fn registering_again_forgets_the_silence_and_gives_a_fresh_start() {
        let mut test = machine();
        test.tick(secs(0), true);
        test.tick(secs(20), true);
        test.registered();
        assert_eq!(test.tick(secs(21), true), vec![Action::Probe(2)]);
        assert_eq!(
            test.tick(secs(40), true),
            vec![],
            "judged from the new probe, 19 s old"
        );
    }

    #[test]
    fn losing_the_connection_pauses_the_judging() {
        let mut test = machine();
        test.tick(secs(0), true);
        assert_eq!(test.tick(secs(100), false), vec![]);
    }
}
