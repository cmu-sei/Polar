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

//! What a run tells both its producers and its sinks, declared once. The script that runs the harness
//! (`infra/load-harness.nu`) gives each Job these as environment variables; the producer's and the sink's own
//! variables are in their crates.

use crate::lanes::{LANES, lane_of};
use polar_env::{Config, ConfigError, Defaulted, Described, Env, Section};

pub static RUN_ID: Defaulted<String> = Defaulted::text(
    "HARNESS_RUN_ID",
    "the run this pod is part of, repeated in its result line so that results from different runs are never mixed",
    "unnamed",
);

pub static INDEX: Defaulted<u64> = Defaulted::number(
    "HARNESS_INDEX",
    "which producer, or which sink, of the run this is, from 0: it decides the lane, and a producer's index is in \
     every message it sends",
    "0",
);

pub static PRODUCERS: Defaulted<u64> = Defaulted::number(
    "HARNESS_PRODUCERS",
    "how many producers the run has, so that a sink knows whom to expect on its lane",
    "1",
);

pub static LANE_COUNT: Defaulted<u64> = Defaulted::number(
    "HARNESS_LANES",
    "how many of the harness's eight topics the run uses: producer i publishes on lane i mod this, and sink j \
     subscribes to lane j mod this",
    "1",
);

pub static COUNT: Defaulted<u64> = Defaulted::number(
    "HARNESS_COUNT",
    "how many messages each producer sends",
    "1000",
);

static VARIABLES: &[&dyn Described] = &[&RUN_ID, &INDEX, &PRODUCERS, &LANE_COUNT, &COUNT];

/// What makes no sense together, or at all.
fn relations(env: &dyn Env) -> Vec<String> {
    let mut problems = Vec::new();
    if let Ok(lanes) = LANE_COUNT.read(env)
        && !(1..=LANES.len() as u64).contains(&lanes)
    {
        problems.push(format!(
            "HARNESS_LANES is {lanes}, but the harness has lanes 1 to {}",
            LANES.len()
        ));
    }
    if let Ok(0) = PRODUCERS.read(env) {
        problems.push("HARNESS_PRODUCERS is 0: a run has at least one producer".to_string());
    }
    if let Ok(0) = COUNT.read(env) {
        problems.push("HARNESS_COUNT is 0: a run sends at least one message".to_string());
    }
    problems
}

/// The section both agents register.
pub static SECTION: Section = Section {
    name: "cassini-harness",
    vars: VARIABLES,
    relations,
};

/// A run, as one producer or sink is given it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub index: u64,
    pub producers: u64,
    pub lanes: u64,
    pub count: u64,
}

impl Run {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            id: config.read(&RUN_ID)?,
            index: config.read(&INDEX)?,
            producers: config.read(&PRODUCERS)?,
            lanes: config.read(&LANE_COUNT)?,
            count: config.read(&COUNT)?,
        })
    }

    /// The topic this producer publishes on, or this sink subscribes to.
    pub fn topic(&self) -> &'static str {
        LANES[lane_of(self.index, self.lanes)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn a_run_is_read_and_its_lane_follows_the_index() {
        let vars = env(&[
            ("HARNESS_RUN_ID", "r1"),
            ("HARNESS_INDEX", "5"),
            ("HARNESS_PRODUCERS", "8"),
            ("HARNESS_LANES", "4"),
            ("HARNESS_COUNT", "20"),
        ]);
        let config = Config::builder(&vars).register(&SECTION).build();
        let run = Run::read(&config).unwrap();
        assert_eq!(
            run,
            Run {
                id: "r1".into(),
                index: 5,
                producers: 8,
                lanes: 4,
                count: 20
            }
        );
        assert_eq!(run.topic(), "cassini.harness.lane.1");
    }

    #[test]
    fn lanes_past_the_harness_s_and_empty_runs_are_refused() {
        let vars = env(&[
            ("HARNESS_LANES", "9"),
            ("HARNESS_PRODUCERS", "0"),
            ("HARNESS_COUNT", "0"),
        ]);
        let problems = SECTION.check(&vars);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("lanes 1 to 8"), "{problems:?}");
    }
}
