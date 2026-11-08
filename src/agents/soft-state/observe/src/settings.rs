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

//! The variables the state agent reads itself.

use polar_env::{Defaulted, Described, Env, Section};
use std::time::Duration;

/// How long a read of the graph is waited for before the asker is told to try again.
pub static GRAPH_TIMEOUT_SECS: Defaulted<u64> = Defaulted::seconds(
    "POLAR_STATE_GRAPH_TIMEOUT_SECS",
    "how long the state agent waits for the graph to answer one read, in seconds; a read not answered by then is \
     answered as unavailable, and the asker tries again (docs/architecture/soft-state.md)",
    "10",
);

static VARIABLES: &[&dyn Described] = &[&GRAPH_TIMEOUT_SECS];

/// The section the state agent registers.
pub static SECTION: Section = Section {
    name: "state-agent",
    vars: VARIABLES,
    relations,
};

fn relations(env: &dyn Env) -> Vec<String> {
    if GRAPH_TIMEOUT_SECS.read(env).is_ok_and(|n| n == 0) {
        vec![format!("{} must be at least 1", GRAPH_TIMEOUT_SECS.name())]
    } else {
        Vec::new()
    }
}

/// The graph's timeout, from the agent's snapshot.
pub fn graph_timeout(env: &dyn Env) -> Result<Duration, String> {
    GRAPH_TIMEOUT_SECS
        .read(env)
        .map(|secs| Duration::from_secs(secs.max(1)))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_graph_is_waited_for_ten_seconds_unless_the_deployment_says_otherwise_and_never_zero() {
        assert_eq!(
            graph_timeout(&|_: &str| None).unwrap(),
            Duration::from_secs(10)
        );
        let three =
            |name: &str| (name == "POLAR_STATE_GRAPH_TIMEOUT_SECS").then(|| "3".to_string());
        assert_eq!(graph_timeout(&three).unwrap(), Duration::from_secs(3));
        let zero = |name: &str| (name == "POLAR_STATE_GRAPH_TIMEOUT_SECS").then(|| "0".to_string());
        assert_eq!(relations(&zero).len(), 1);
    }
}
