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

//! What the sink reads beyond the run (`harness_common::settings`), declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Section};
use std::time::Duration;

pub static WARMUP: Defaulted<u64> = Defaulted::number(
    "HARNESS_WARMUP",
    "how many of each producer's first messages are left out of the latencies (they are still counted as received): \
     the first messages of a run measure connections and caches warming, not the broker",
    "0",
);

pub static IDLE_SECS: Defaulted<u64> = Defaulted::number(
    "HARNESS_IDLE_SECS",
    "how long the sink waits, once messages have started arriving, for the next one before it reports what it has",
    "30",
);

pub static FIRST_WAIT_SECS: Defaulted<u64> = Defaulted::number(
    "HARNESS_FIRST_WAIT_SECS",
    "how long the sink waits, once ready, for the first message before it reports that none came",
    "300",
);

static VARIABLES: &[&dyn Described] = &[&WARMUP, &IDLE_SECS, &FIRST_WAIT_SECS];

pub static SECTION: Section = Section {
    name: "harness-sink",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct SinkSettings {
    pub warmup: u64,
    pub idle: Duration,
    pub first_wait: Duration,
}

impl SinkSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            warmup: config.read(&WARMUP)?,
            idle: Duration::from_secs(config.read(&IDLE_SECS)?),
            first_wait: Duration::from_secs(config.read(&FIRST_WAIT_SECS)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn what_is_given_is_read() {
        let vars: HashMap<String, String> = [
            ("HARNESS_WARMUP", "100"),
            ("HARNESS_IDLE_SECS", "5"),
            ("HARNESS_FIRST_WAIT_SECS", "60"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = SinkSettings::read(&config).unwrap();
        assert_eq!(s.warmup, 100);
        assert_eq!(s.idle, Duration::from_secs(5));
        assert_eq!(s.first_wait, Duration::from_secs(60));
    }
}
