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

//! The variables the state agent's writer reads itself: how often it asks, and how long it waits for an answer.

use polar_env::{Defaulted, Described, Env, Section};
use std::time::Duration;

/// How often every expected workload is asked about its health.
pub static ASK_INTERVAL_SECS: Defaulted<u64> = Defaulted::seconds(
    "POLAR_HEALTH_ASK_INTERVAL_SECS",
    "how often the state agent's writer asks every workload its `agents` section names about its health, in \
     seconds (docs/architecture/soft-state.md)",
    "60",
);

/// How long a round waits for the answers: a workload that has not answered by then is unresponsive.
pub static ANSWER_TIMEOUT_SECS: Defaulted<u64> = Defaulted::seconds(
    "POLAR_HEALTH_ANSWER_TIMEOUT_SECS",
    "how long the state agent's writer waits for each workload's answer in one round, in seconds; a workload that \
     has not answered by then is written as unresponsive. Less than POLAR_HEALTH_ASK_INTERVAL_SECS",
    "10",
);

static VARIABLES: &[&dyn Described] = &[&ASK_INTERVAL_SECS, &ANSWER_TIMEOUT_SECS];

/// The section the writer registers.
pub static SECTION: Section = Section {
    name: "state-writer",
    vars: VARIABLES,
    relations,
};

fn relations(env: &dyn Env) -> Vec<String> {
    let mut problems = Vec::new();
    let interval = ASK_INTERVAL_SECS.read(env);
    let timeout = ANSWER_TIMEOUT_SECS.read(env);
    if interval.as_ref().is_ok_and(|n| *n == 0) {
        problems.push(format!("{} must be at least 1", ASK_INTERVAL_SECS.name()));
    }
    if timeout.as_ref().is_ok_and(|n| *n == 0) {
        problems.push(format!("{} must be at least 1", ANSWER_TIMEOUT_SECS.name()));
    }
    if let (Ok(interval), Ok(timeout)) = (interval, timeout)
        && timeout >= interval
    {
        problems.push(format!(
            "{} must be less than {}: a round is closed before the next begins",
            ANSWER_TIMEOUT_SECS.name(),
            ASK_INTERVAL_SECS.name()
        ));
    }
    problems
}

/// The writer's timing, from the agent's snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Timing {
    pub fn from_env(env: &dyn Env) -> Result<Self, String> {
        Ok(Self {
            interval: Duration::from_secs(
                ASK_INTERVAL_SECS
                    .read(env)
                    .map_err(|e| e.to_string())?
                    .max(1),
            ),
            timeout: Duration::from_secs(
                ANSWER_TIMEOUT_SECS
                    .read(env)
                    .map_err(|e| e.to_string())?
                    .max(1),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn a_round_every_minute_waits_ten_seconds_unless_the_deployment_says_otherwise() {
        assert_eq!(
            Timing::from_env(&|_: &str| None).unwrap(),
            Timing {
                interval: Duration::from_secs(60),
                timeout: Duration::from_secs(10)
            }
        );
        assert!(relations(&|_: &str| None).is_empty());
        assert_eq!(
            Timing::from_env(&env(&[("POLAR_HEALTH_ANSWER_TIMEOUT_SECS", "3")]))
                .unwrap()
                .timeout,
            Duration::from_secs(3)
        );
    }

    #[test]
    fn a_zero_or_a_timeout_as_long_as_the_interval_is_refused() {
        assert_eq!(
            relations(&env(&[("POLAR_HEALTH_ASK_INTERVAL_SECS", "0")])).len(),
            2,
            "zero, and a timeout no shorter than it"
        );
        assert_eq!(
            relations(&env(&[("POLAR_HEALTH_ANSWER_TIMEOUT_SECS", "0")])).len(),
            1
        );
        let problems = relations(&env(&[
            ("POLAR_HEALTH_ASK_INTERVAL_SECS", "10"),
            ("POLAR_HEALTH_ANSWER_TIMEOUT_SECS", "10"),
        ]));
        assert!(problems[0].contains("less than"), "{problems:?}");
    }
}
